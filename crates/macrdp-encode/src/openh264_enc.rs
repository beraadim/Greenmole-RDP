//! OpenH264 software H.264 encoder optimized for screen content

use std::ffi::c_void;

use anyhow::{Context, Result};
use bytes::Bytes;
use openh264::encoder::{Encoder, EncoderConfig, FrameType, VuiConfig};
use openh264::formats::YUVSlices;

use crate::color_convert::VImageConverter;
use crate::{Avc444EncodedFrame, EncodedFrame, Quality, VideoEncoder};

pub struct OpenH264Encoder {
    encoder: Encoder,
    width: u32,
    height: u32,
    force_keyframe: bool,
    yuv_buf: Vec<u8>,
    /// Current target bitrate
    target_bitrate: u32,
    bitrate_applied: bool,
    main_initialized: bool,
    fps: f32,
    quality: Quality,
    mode_444: bool,
    /// vImage SIMD accelerated BGRA→I420 converter (macOS Accelerate.framework)
    vimage: Option<VImageConverter>,
    /// Reusable buffers for AVC444 YUV444 split
    yuv444_bufs: Option<Yuv444SplitBufs>,
}

/// Reusable buffers for AVC444 encoding
struct Yuv444SplitBufs {
    y444: Vec<u8>,
    u444: Vec<u8>,
    v444: Vec<u8>,
    main_view: crate::yuv444_split::Yuv420Frame,
    aux_view: crate::yuv444_split::Yuv420Frame,
}

impl Yuv444SplitBufs {
    fn new(width: u32, height: u32) -> Self {
        let full = (width * height) as usize;
        Self {
            y444: vec![0u8; full],
            u444: vec![0u8; full],
            v444: vec![0u8; full],
            main_view: crate::yuv444_split::Yuv420Frame::new(width, height),
            aux_view: crate::yuv444_split::Yuv420Frame::new(width, height),
        }
    }
}

fn create_oh264_encoder(
    _width: u32,
    _height: u32,
    fps: f32,
    bitrate: u32,
    quality: Quality,
    mode_444: bool,
) -> Result<Encoder> {
    let (min_qp, max_qp) = quality.quantizer_range()?;
    let complexity = match quality {
        Quality::LowLatency => openh264::encoder::Complexity::Low,
        Quality::Balanced => openh264::encoder::Complexity::Medium,
        Quality::HighQuality => openh264::encoder::Complexity::High,
        Quality::Lossless => unreachable!("lossless rejected before codec configuration"),
    };
    let config = EncoderConfig::new()
        .vui(VuiConfig::bt709_full())
        .bitrate(openh264::encoder::BitRate::from_bps(bitrate))
        .max_frame_rate(openh264::encoder::FrameRate::from_hz(fps))
        .rate_control_mode(openh264::encoder::RateControlMode::Quality)
        .background_detection(false)
        // Alternating main/auxiliary views are different representations of
        // one desktop, not scene cuts. Preserve their shared P-frame chain.
        .scene_change_detect(!mode_444)
        .adaptive_quantization(true)
        .qp(openh264::encoder::QpRange::new(min_qp, max_qp))
        .skip_frames(false)
        // ScreenContentRealTime forcibly reenables scene detection inside
        // OpenH264, turning every alternating AVC444 view into another IDR.
        // CameraVideoRealTime permits disabling that heuristic; all screen
        // bitrate/quantizer and no-frame-skip policy above remains in force.
        .usage_type(if mode_444 {
            openh264::encoder::UsageType::CameraVideoRealTime
        } else {
            openh264::encoder::UsageType::ScreenContentRealTime
        })
        .complexity(complexity)
        .intra_frame_period(openh264::encoder::IntraFramePeriod::from_num_frames(
            fps as u32 * 5,
        ))
        .long_term_reference(true)
        .num_threads(4);

    Encoder::with_api_config(openh264::OpenH264API::from_source(), config)
        .context("Failed to create OpenH264 encoder")
}

impl OpenH264Encoder {
    fn validate_avc444_input(
        &self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: usize,
    ) -> Result<()> {
        if width == 0 || height == 0 || width > self.width || height > self.height {
            anyhow::bail!(
                "AVC444 frame too large: frame={}x{}, encoder={}x{}",
                width,
                height,
                self.width,
                self.height
            );
        }
        if width % 2 != 0 || height % 2 != 0 {
            anyhow::bail!(
                "AVC444 requires even frame dimensions, got {}x{}",
                width,
                height
            );
        }
        let min_stride = width as usize * 4;
        if stride < min_stride {
            anyhow::bail!(
                "AVC444 BGRA stride too small: stride={}, expected at least {} for width={}",
                stride,
                min_stride,
                width
            );
        }
        let required_len = (height as usize - 1)
            .saturating_mul(stride)
            .saturating_add(min_stride);
        if data.len() < required_len {
            anyhow::bail!(
                "AVC444 BGRA buffer too short: len={}, required={}, width={}, height={}, stride={}",
                data.len(),
                required_len,
                width,
                height,
                stride
            );
        }
        Ok(())
    }

    /// Create a balanced encoder; callers selecting a preset use new_with_quality.
    pub fn new(width: u32, height: u32, fps: f32, bitrate: u32, mode_444: bool) -> Result<Self> {
        Self::new_with_quality(width, height, fps, bitrate, mode_444, Quality::Balanced)
    }

    pub fn new_with_quality(
        width: u32,
        height: u32,
        fps: f32,
        bitrate: u32,
        mode_444: bool,
        quality: Quality,
    ) -> Result<Self> {
        let (min_qp, max_qp) = quality.quantizer_range()?;
        // MS-RDPEGFX 2.2.4.5 requires both views in one H.264 reference
        // chain, decoded in main/auxiliary order by a single decoder.
        let coded_fps = if mode_444 { fps * 2.0 } else { fps };
        let encoder = create_oh264_encoder(width, height, coded_fps, bitrate, quality, mode_444)?;

        let vimage = VImageConverter::new()
            .map_err(|e| tracing::warn!("vImage init failed, using scalar fallback: {e}"))
            .ok();

        let yuv_size = (width * height * 3 / 2) as usize;

        let yuv444_bufs = if mode_444 {
            Some(Yuv444SplitBufs::new(width, height))
        } else {
            None
        };

        tracing::info!(
            width,
            height,
            fps,
            mode_444,
            bitrate_mbps = bitrate as f64 / 1_000_000.0,
            ?quality,
            min_qp,
            max_qp,
            "OpenH264 encoder configured (quality confirmed on first native frame)"
        );

        Ok(Self {
            encoder,
            width,
            height,
            force_keyframe: false,
            yuv_buf: vec![0u8; yuv_size],
            target_bitrate: bitrate,
            bitrate_applied: false,
            main_initialized: false,
            fps,
            quality,
            mode_444,
            vimage,
            yuv444_bufs,
        })
    }
}

/// Convert BGRA to YUV420 into an encoder-sized buffer (handles padding).
fn bgra_to_yuv420_padded(
    bgra: &[u8],
    src_w: u32,
    src_h: u32,
    stride: usize,
    enc_w: u32,
    enc_h: u32,
    yuv: &mut [u8],
) {
    let ew = enc_w as usize;
    let sw = src_w as usize;
    let sh = src_h as usize;
    let y_plane_size = ew * enc_h as usize;
    let uv_w = ew / 2;

    let (y_plane, uv_planes) = yuv.split_at_mut(y_plane_size);
    let uv_plane_size = uv_w * (enc_h as usize / 2);
    let (u_plane, v_plane) = uv_planes.split_at_mut(uv_plane_size);

    for row in 0..sh {
        for col in 0..sw {
            let bgra_offset = row * stride + col * 4;
            if bgra_offset + 2 >= bgra.len() {
                continue;
            }
            let b = bgra[bgra_offset] as i32;
            let g = bgra[bgra_offset + 1] as i32;
            let r = bgra[bgra_offset + 2] as i32;

            y_plane[row * ew + col] = crate::color::luma(r, g, b);
        }
    }
    // Match the SIMD converter's 2x2 chroma average, including odd visible
    // edges. Sampling only the top-left pixel causes avoidable color aliasing.
    for row in (0..sh).step_by(2) {
        for col in (0..sw).step_by(2) {
            let mut rgb = [0i32; 3];
            let mut count = 0;
            for y in row..(row + 2).min(sh) {
                for x in col..(col + 2).min(sw) {
                    let offset = y * stride + x * 4;
                    if let Some(pixel) = bgra.get(offset..offset + 3) {
                        rgb[0] += i32::from(pixel[2]);
                        rgb[1] += i32::from(pixel[1]);
                        rgb[2] += i32::from(pixel[0]);
                        count += 1;
                    }
                }
            }
            if count != 0 {
                let (u, v) = crate::color::chroma(rgb[0] / count, rgb[1] / count, rgb[2] / count);
                let index = (row / 2) * uv_w + col / 2;
                u_plane[index] = u;
                v_plane[index] = v;
            }
        }
    }
}

impl VideoEncoder for OpenH264Encoder {
    fn prepare(&mut self) -> Result<()> {
        let y_size = (self.width * self.height) as usize;
        let uv_size = y_size / 4;
        self.yuv_buf[..y_size].fill(0);
        self.yuv_buf[y_size..].fill(128);
        let (y, uv) = self.yuv_buf.split_at(y_size);
        let (u, v) = uv.split_at(uv_size);
        let yuv = YUVSlices::new(
            (y, u, v),
            (self.width as usize, self.height as usize),
            (
                self.width as usize,
                self.width as usize / 2,
                self.width as usize / 2,
            ),
        );
        // The wrapper initializes the native codec only on encode. Discard this
        // neutral frame and force an IDR so no client references that hidden frame.
        self.encoder
            .encode(&yuv)
            .context("OpenH264 native startup failed")?;
        self.main_initialized = true;
        self.bitrate_applied = true;
        self.force_keyframe = true;
        tracing::info!(quality = ?self.quality, "OpenH264 native quality and bitrate configuration accepted");
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "openh264"
    }

    fn encode_bgra(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: usize,
    ) -> Result<EncodedFrame> {
        if width > self.width || height > self.height {
            anyhow::bail!(
                "Frame too large: encoder {}x{}, got {}x{}",
                self.width,
                self.height,
                width,
                height
            );
        }

        // Zero the padding area
        let y_size = (self.width * self.height) as usize;
        let uv_size = (self.width / 2 * self.height / 2) as usize;
        self.yuv_buf[..y_size].fill(0);
        self.yuv_buf[y_size..y_size + uv_size].fill(128);
        self.yuv_buf[y_size + uv_size..].fill(128);

        // Convert visible BGRA area to YUV420
        if let Some(ref converter) = self.vimage {
            // vImage handles width*height directly; we need to handle encoder padding separately
            // For now, use vImage for the visible area, then zero-fill padding
            if width == self.width && height == self.height {
                // No padding needed — vImage directly
                converter
                    .bgra_to_i420(data, width, height, stride, &mut self.yuv_buf)
                    .map_err(|e| anyhow::anyhow!("vImage BGRA->I420 failed: {e}"))?;
            } else {
                // Encoder dimensions have padding — use scalar fallback for now
                // TODO: vImage with manual padding
                bgra_to_yuv420_padded(
                    data,
                    width,
                    height,
                    stride,
                    self.width,
                    self.height,
                    &mut self.yuv_buf,
                );
            }
        } else {
            bgra_to_yuv420_padded(
                data,
                width,
                height,
                stride,
                self.width,
                self.height,
                &mut self.yuv_buf,
            );
        }

        let y_size = (self.width * self.height) as usize;
        let uv_size = (self.width / 2 * self.height / 2) as usize;
        let (y_plane, uv_planes) = self.yuv_buf.split_at(y_size);
        let (u_plane, v_plane) = uv_planes.split_at(uv_size);
        let yuv = YUVSlices::new(
            (y_plane, u_plane, v_plane),
            (self.width as usize, self.height as usize),
            (
                self.width as usize,
                self.width as usize / 2,
                self.width as usize / 2,
            ),
        );

        // Force IDR on next frame if requested
        if self.force_keyframe {
            self.encoder.force_intra_frame();
            self.force_keyframe = false;
        }

        let bitstream = self
            .encoder
            .encode(&yuv)
            .context("OpenH264 encode failed")?;

        let mut nal_data = Vec::new();
        bitstream.write_vec(&mut nal_data);

        let is_keyframe = matches!(bitstream.frame_type(), FrameType::IDR | FrameType::I);
        if !self.main_initialized {
            self.main_initialized = true;
            self.bitrate_applied = true;
            tracing::info!(quality = ?self.quality, "OpenH264 accepted quality on its first native frame");
        }

        Ok(EncodedFrame {
            data: Bytes::from(nal_data),
            is_keyframe,
            width: self.width,
            height: self.height,
        })
    }

    fn encode_bgra_444(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: usize,
    ) -> Result<Avc444EncodedFrame> {
        self.validate_avc444_input(data, width, height, stride)?;

        let requested_keyframe = self.force_keyframe;
        if requested_keyframe {
            self.encoder.force_intra_frame();
            self.force_keyframe = false;
        }
        let bufs = self
            .yuv444_bufs
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("AVC444 not enabled: no YUV444 buffers"))?;

        let w = self.width;
        let h = self.height;

        // Step 1: BGRA -> YUV444
        crate::yuv444_split::bgra_to_yuv444_padded(
            data,
            width,
            height,
            stride,
            w,
            h,
            &mut bufs.y444,
            &mut bufs.u444,
            &mut bufs.v444,
        )?;

        // Step 2: YUV444 -> Main YUV420 + Aux YUV420 (B-area split)
        crate::yuv444_split::yuv444_split_to_yuv420(
            &bufs.y444,
            &bufs.u444,
            &bufs.v444,
            w,
            h,
            &mut bufs.main_view,
            &mut bufs.aux_view,
        );

        // Step 3: encode both streams from borrowed planes
        let main_yuv = YUVSlices::new(
            (&bufs.main_view.y, &bufs.main_view.u, &bufs.main_view.v),
            (w as usize, h as usize),
            (w as usize, w as usize / 2, w as usize / 2),
        );
        let main_bitstream = self.encoder.encode(&main_yuv).map_err(|error| {
            self.force_keyframe = true;
            anyhow::anyhow!("OpenH264 main encode failed: {error}")
        })?;
        let mut main_nal = Vec::new();
        main_bitstream.write_vec(&mut main_nal);
        let main_keyframe = matches!(main_bitstream.frame_type(), FrameType::IDR | FrameType::I);
        if !self.main_initialized {
            self.main_initialized = true;
            self.bitrate_applied = true;
            tracing::info!(quality = ?self.quality, "OpenH264 accepted quality on its first native frame");
        }

        let aux_yuv = YUVSlices::new(
            (&bufs.aux_view.y, &bufs.aux_view.u, &bufs.aux_view.v),
            (w as usize, h as usize),
            (w as usize, w as usize / 2, w as usize / 2),
        );
        // One decoder consumes the views in order. Only main needs an IDR;
        // auxiliary may reference that fresh main without a second large IDR.
        let aux_bitstream = self.encoder.encode(&aux_yuv).map_err(|error| {
            // Main may already be a native reference. Never return a partial
            // pair and make the next delivered pair self-contained.
            self.force_keyframe = true;
            anyhow::anyhow!("OpenH264 auxiliary encode failed: {error}")
        })?;
        let mut aux_nal = Vec::new();
        aux_bitstream.write_vec(&mut aux_nal);
        let aux_keyframe = matches!(aux_bitstream.frame_type(), FrameType::IDR | FrameType::I);
        if main_nal.is_empty() || aux_nal.is_empty() || (requested_keyframe && !main_keyframe) {
            self.force_keyframe = true;
            anyhow::bail!("OpenH264 returned an incomplete AVC444 pair or ignored a requested IDR");
        }

        tracing::debug!(
            main_bytes = main_nal.len(),
            aux_bytes = aux_nal.len(),
            main_keyframe,
            aux_keyframe,
            "AVC444 OpenH264 dual-stream encode complete"
        );

        Ok(Avc444EncodedFrame {
            main_view: EncodedFrame {
                data: Bytes::from(main_nal),
                is_keyframe: main_keyframe,
                width: w,
                height: h,
            },
            aux_view: EncodedFrame {
                data: Bytes::from(aux_nal),
                is_keyframe: aux_keyframe,
                width: w,
                height: h,
            },
        })
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        anyhow::ensure!(
            (1..=crate::MAX_BITRATE_BPS).contains(&bitrate_bps),
            "Invalid bitrate"
        );
        if self.target_bitrate == bitrate_bps && (!self.main_initialized || self.bitrate_applied) {
            return Ok(());
        }
        // The wrapper initializes its native codec lazily. Reconfigure before
        // first use, otherwise preserve the active main/aux reference chain.
        if !self.main_initialized {
            self.encoder = create_oh264_encoder(
                self.width,
                self.height,
                if self.mode_444 {
                    self.fps * 2.0
                } else {
                    self.fps
                },
                bitrate_bps,
                self.quality,
                self.mode_444,
            )?;
            self.target_bitrate = bitrate_bps;
            self.bitrate_applied = false;
            return Ok(());
        }
        self.bitrate_applied = false;
        unsafe {
            let mut bitrate_info = openh264_sys2::SBitrateInfo {
                iLayer: openh264_sys2::SPATIAL_LAYER_ALL as i32,
                iBitrate: bitrate_bps as i32,
            };
            let status = self.encoder.raw_api().set_option(
                openh264_sys2::ENCODER_OPTION_BITRATE,
                &mut bitrate_info as *mut _ as *mut c_void,
            );
            anyhow::ensure!(status == 0, "OpenH264 bitrate update failed: {status}");
        }
        self.target_bitrate = bitrate_bps;
        self.bitrate_applied = true;
        tracing::debug!(bitrate_bps, "OpenH264 accepted bitrate update");
        Ok(())
    }

    fn applied_bitrate(&self) -> Option<u32> {
        self.bitrate_applied.then_some(self.target_bitrate)
    }

    fn force_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    fn supports_444(&self) -> bool {
        self.mode_444
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_openh264_output_preserves_colors_with_simd_scalar_and_padding() {
        for (simd, width, height) in [(true, 128, 64), (false, 128, 64), (true, 126, 60)] {
            let mut encoder = OpenH264Encoder::new_with_quality(
                128,
                64,
                30.0,
                3_000_000,
                false,
                Quality::HighQuality,
            )
            .unwrap();
            if !simd {
                encoder.vimage = None;
            }
            crate::color::tests::assert_encoded_color_fidelity(&mut encoder, width, height);
            let parameters = native_parameters(&mut encoder.encoder);
            let layer = &parameters.sSpatialLayers[0];
            assert!(
                layer.bVideoSignalTypePresent && layer.bColorDescriptionPresent && layer.bFullRange
            );
            assert_eq!(
                (
                    layer.uiColorPrimaries,
                    layer.uiTransferCharacteristics,
                    layer.uiColorMatrix
                ),
                (1, 1, 1)
            );
        }
    }

    #[test]
    fn actual_avc444_single_stream_preserves_one_pixel_chroma_with_width_padding() {
        for (width, height) in [(128, 64), (126, 60)] {
            let mut encoder = OpenH264Encoder::new_with_quality(
                128,
                64,
                30.0,
                12_000_000,
                true,
                Quality::HighQuality,
            )
            .unwrap();
            crate::color::tests::assert_avc444_fidelity(&mut encoder, width, height);
        }
    }

    fn native_parameters(encoder: &mut Encoder) -> openh264_sys2::SEncParamExt {
        let mut parameters = openh264_sys2::SEncParamExt::default();
        let status = unsafe {
            encoder.raw_api().get_option(
                openh264_sys2::ENCODER_OPTION_SVC_ENCODE_PARAM_EXT,
                &mut parameters as *mut _ as *mut c_void,
            )
        };
        assert_eq!(status, 0, "Cannot read native encoder parameters");
        parameters
    }

    #[test]
    fn quality_presets_reach_shared_native_encoder_after_lazy_bitrate_reconfiguration() {
        for (quality, expected, complexity) in [
            (Quality::LowLatency, (20, 40), openh264_sys2::LOW_COMPLEXITY),
            (
                Quality::Balanced,
                (16, 34),
                openh264_sys2::MEDIUM_COMPLEXITY,
            ),
            (
                Quality::HighQuality,
                (12, 28),
                openh264_sys2::HIGH_COMPLEXITY,
            ),
        ] {
            let mut encoder =
                OpenH264Encoder::new_with_quality(64, 64, 30.0, 2_000_000, true, quality).unwrap();
            // Both native contexts are still uninitialized here. A target
            // change must retain the selected policy when recreating them.
            encoder.set_bitrate(1_000_000).unwrap();
            let output = encoder
                .encode_bgra_444(&vec![128; 64 * 64 * 4], 64, 64, 256)
                .unwrap();
            assert!(!output.main_view.data.is_empty());
            assert!(!output.aux_view.data.is_empty());
            let parameters = native_parameters(&mut encoder.encoder);
            assert_eq!((parameters.iMinQp, parameters.iMaxQp), expected);
            assert_eq!(parameters.iComplexityMode, complexity);
            assert_eq!(parameters.iUsageType, openh264_sys2::CAMERA_VIDEO_REAL_TIME);
            assert!(!parameters.bEnableSceneChangeDetect);
        }
    }

    #[test]
    fn lossless_quality_cannot_fall_back_to_a_lossy_video_encoder() {
        for preference in [
            crate::EncoderPreference::Software,
            crate::EncoderPreference::Hardware,
        ] {
            assert!(crate::create_encoder(
                64,
                64,
                30.0,
                Quality::Lossless,
                preference,
                false,
                2_000_000
            )
            .is_err());
        }
        assert!(OpenH264Encoder::new_with_quality(
            64,
            64,
            30.0,
            2_000_000,
            false,
            Quality::Lossless
        )
        .is_err());
        assert_eq!(
            crate::screen_bitrate(1920, 1080, 60.0, Quality::Lossless),
            0
        );
    }

    #[test]
    fn prepared_encoders_deliver_a_self_contained_initial_pair() {
        for mode_444 in [false, true] {
            let mut encoder = OpenH264Encoder::new(64, 64, 30.0, 2_000_000, mode_444).unwrap();
            encoder.prepare().unwrap();
            assert!(encoder.main_initialized);
            assert_eq!(encoder.applied_bitrate(), Some(2_000_000));
            // A hot bitrate update is now native, not a deferred configuration.
            encoder.set_bitrate(1_000_000).unwrap();
            let bgra = vec![128; 64 * 64 * 4];
            if mode_444 {
                let frame = encoder.encode_bgra_444(&bgra, 64, 64, 256).unwrap();
                assert!(frame.main_view.is_keyframe);
                assert!(!frame.aux_view.is_keyframe);
                assert!(!frame.main_view.data.is_empty() && !frame.aux_view.data.is_empty());
            } else {
                let frame = encoder.encode_bgra(&bgra, 64, 64, 256).unwrap();
                assert!(frame.is_keyframe && !frame.data.is_empty());
            }
        }
    }

    #[test]
    fn bitrate_tracks_only_accepted_values_and_valid_updates_still_encode() {
        let mut encoder = OpenH264Encoder::new(64, 64, 30.0, 1_000_000, true).unwrap();
        assert_eq!(encoder.applied_bitrate(), None);
        assert!(encoder.set_bitrate(0).is_err());
        assert_eq!(encoder.applied_bitrate(), None);
        encoder.set_bitrate(2_000_000).unwrap();
        assert_eq!(encoder.applied_bitrate(), None);
        let output = encoder
            .encode_bgra_444(&vec![128; 64 * 64 * 4], 64, 64, 256)
            .unwrap();
        assert!(!output.main_view.data.is_empty());
        assert!(!output.aux_view.data.is_empty());
        assert_eq!(encoder.applied_bitrate(), Some(2_000_000));
        encoder.set_bitrate(1_000_000).unwrap();
        assert_eq!(encoder.applied_bitrate(), Some(1_000_000));
        encoder.set_bitrate(2_000_000).unwrap();
        assert_eq!(encoder.applied_bitrate(), Some(2_000_000));
    }

    #[test]
    fn bitrate_update_preserves_initialized_encoder_before_first_auxiliary_view() {
        let mut encoder = OpenH264Encoder::new(64, 64, 30.0, 2_000_000, true).unwrap();
        let bgra = vec![128; 64 * 64 * 4];
        encoder.encode_bgra(&bgra, 64, 64, 256).unwrap();
        assert!(encoder.main_initialized);
        encoder.set_bitrate(1_000_000).unwrap();
        assert_eq!(encoder.applied_bitrate(), Some(1_000_000));
        assert!(encoder.main_initialized);
        let output = encoder.encode_bgra_444(&bgra, 64, 64, 256).unwrap();
        assert!(!output.main_view.data.is_empty());
        assert!(!output.aux_view.data.is_empty());
    }

    #[test]
    fn test_encode_frame() {
        let mut encoder = OpenH264Encoder::new(64, 64, 30.0, 1_000_000, false).unwrap();
        let bgra = vec![128u8; 64 * 64 * 4];
        let frame = encoder.encode_bgra(&bgra, 64, 64, 64 * 4).unwrap();
        assert!(!frame.data.is_empty());
        assert!(frame.is_keyframe);
    }

    #[test]
    fn test_screen_bitrate() {
        let br = crate::screen_bitrate(1920, 1080, 60.0, Quality::HighQuality);
        assert!(
            br > 30_000_000,
            "1080p60 HighQuality should be > 30Mbps, got {}",
            br
        );
    }
}

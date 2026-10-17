//! Video encoding abstraction (H.264 via VideoToolbox / OpenH264)

#[cfg(any(target_os = "macos", test))]
mod avcc;
mod color;
#[cfg(target_os = "macos")]
pub mod color_convert;
mod completion;
mod openh264_enc;
#[cfg(any(target_os = "macos", test))]
mod retirement;
pub use completion::EncodeTiming;
#[cfg(target_os = "macos")]
mod videotoolbox;
pub mod yuv444_split;
use anyhow::Result;
use bytes::Bytes;

pub use openh264_enc::OpenH264Encoder;
#[cfg(target_os = "macos")]
pub use videotoolbox::VtEncoder;

/// Native sessions retain ownership through asynchronous retirement. Server
/// restart must wait for this to clear after its display handles are dropped.
pub fn native_cleanup_pending() -> bool {
    #[cfg(target_os = "macos")]
    {
        videotoolbox::native_cleanup_pending()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Keep the native encoders' signed integer bitrate properties in range.
pub const MAX_BITRATE_BPS: u32 = 1_000_000_000;

/// Encoded frame output
pub struct EncodedFrame {
    /// H.264 NAL units (Annex B format)
    pub data: Bytes,
    /// Whether this is a key frame (IDR)
    pub is_keyframe: bool,
    pub width: u32,
    pub height: u32,
}

/// AVC444 dual-stream encoded result
pub struct Avc444EncodedFrame {
    /// Stream1: Main YUV420 H.264 (luma + downsampled chroma)
    pub main_view: EncodedFrame,
    /// Stream2: Auxiliary YUV420 H.264 (chroma compensation)
    pub aux_view: EncodedFrame,
}

/// Quality preset
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    LowLatency,
    Balanced,
    HighQuality,
    /// Desktop transport bypasses video encoding to preserve RGB pixels.
    Lossless,
}

impl Quality {
    /// Lower quantizers retain more detail at the cost of larger frames.
    /// Lossless desktop transport must never silently instantiate a lossy codec.
    pub(crate) fn quantizer_range(self) -> Result<(u8, u8)> {
        match self {
            Self::LowLatency => Ok((20, 40)),
            Self::Balanced => Ok((16, 34)),
            // OpenH264 rate control clamps its lower bound to 12. Use the
            // actual supported range consistently across both backends.
            Self::HighQuality => Ok((12, 28)),
            Self::Lossless => anyhow::bail!("Lossless desktop quality bypasses H.264 encoding"),
        }
    }
}

/// Video encoder trait
pub trait VideoEncoder: Send {
    /// Run any lazy native initialization on the bounded startup worker before
    /// exposing the encoder to the display loop. No generated frame is sent.
    fn prepare(&mut self) -> Result<()> {
        Ok(())
    }

    /// Backend actually instantiated, including any initialization fallback.
    fn backend_name(&self) -> &'static str;

    /// AVC420 encode (existing)
    fn encode_bgra(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: usize,
    ) -> Result<EncodedFrame>;

    /// AVC444 dual-stream encode.
    /// Internally performs BGRA -> YUV444 -> B-area split -> dual session encode.
    fn encode_bgra_444(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: usize,
    ) -> Result<Avc444EncodedFrame>;

    /// Zero-copy encode from CVPixelBuffer (NV12). Default returns error (unsupported).
    /// Only VtEncoder overrides this.
    fn encode_pixel_buffer(
        &mut self,
        _ptr: *mut std::ffi::c_void,
        _force_keyframe: bool,
    ) -> Result<EncodedFrame> {
        anyhow::bail!("encode_pixel_buffer not supported by this encoder")
    }

    /// Confirm that the backend accepted the bitrate; errors must not be
    /// reported as an applied setting.
    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()>;
    fn applied_bitrate(&self) -> Option<u32> {
        None
    }
    fn force_keyframe(&mut self);

    /// Whether this encoder supports AVC444 dual-stream encoding
    fn supports_444(&self) -> bool;

    /// Whether native NV12 buffers can be encoded without a BGRA conversion.
    fn supports_pixel_buffer(&self) -> bool {
        false
    }

    /// Whether this encoder supports async submit/collect pipelining.
    fn supports_pipelining(&self) -> bool {
        false
    }

    /// Callback completion, usable without waiting for another captured frame.
    fn encode_completion(&self) -> Option<tokio::sync::watch::Receiver<Option<EncodeTiming>>> {
        None
    }

    fn last_encode_timing(&self) -> Option<EncodeTiming> {
        None
    }

    /// Submit a BGRA frame for async encoding. Returns immediately.
    fn submit_bgra(
        &mut self,
        _data: &[u8],
        _width: u32,
        _height: u32,
        _stride: usize,
    ) -> Result<()> {
        anyhow::bail!("submit_bgra not supported by this encoder")
    }

    /// Submit a CVPixelBuffer for async encoding. Returns immediately.
    fn submit_pixel_buffer(&mut self, _ptr: *mut std::ffi::c_void) -> Result<()> {
        anyhow::bail!("submit_pixel_buffer not supported by this encoder")
    }

    /// Collect result from a previous submit. Blocks up to timeout.
    fn collect_encoded(&mut self, _timeout: std::time::Duration) -> Result<Option<EncodedFrame>> {
        Ok(None)
    }
}

/// Align a dimension up to the nearest multiple of 16 (H.264 macroblock size)
pub fn align16(v: u32) -> u32 {
    (v + 15) & !15
}

/// Calculate optimal bitrate for screen content
pub fn screen_bitrate(width: u32, height: u32, fps: f32, quality: Quality) -> u32 {
    let pixels = width as f64 * height as f64;
    let base_bpp = match quality {
        Quality::LowLatency => 8.0,
        Quality::Balanced => 16.0,
        Quality::HighQuality => 24.0,
        Quality::Lossless => return 0,
    };
    let fps_factor = (fps as f64 / 30.0).max(1.0);
    ((pixels * base_bpp * fps_factor) as u32).clamp(1, MAX_BITRATE_BPS)
}

/// Encoder preference
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderPreference {
    /// Explicit classic RDP bitmap transport; never construct a video encoder.
    Bitmap,
    /// OpenH264 CPU encoder — full P-frame support, higher latency (~40ms)
    Software,
    /// VideoToolbox GPU encoder — low latency (~6ms), supports P-frames
    Hardware,
    /// Same as Software (recommended default)
    Auto,
}

impl EncoderPreference {
    pub fn from_str_opt(s: Option<&str>) -> Self {
        match s.map(|s| s.to_lowercase()).as_deref() {
            Some("bitmap") => Self::Bitmap,
            Some("hardware") | Some("gpu") | Some("videotoolbox") | Some("vt") => Self::Hardware,
            Some("software") | Some("cpu") | Some("openh264") | Some("oh264") => Self::Software,
            _ => Self::Auto,
        }
    }
}

/// Create an H.264 encoder based on preference.
/// AVC444 encodes main and auxiliary views in one ordered H.264 reference chain.
pub fn create_encoder(
    width: u32,
    height: u32,
    fps: f32,
    quality: Quality,
    preference: EncoderPreference,
    mode_444: bool,
    bitrate: u32,
) -> Result<Box<dyn VideoEncoder>> {
    anyhow::ensure!(
        preference != EncoderPreference::Bitmap,
        "Bitmap desktop transport bypasses H.264 encoding"
    );
    quality.quantizer_range()?;
    let bitrate = bitrate.clamp(1, MAX_BITRATE_BPS);
    let enc_w = align16(width);
    let enc_h = align16(height);

    // Hardware: VideoToolbox GPU encoder
    #[cfg(target_os = "macos")]
    if preference == EncoderPreference::Hardware {
        match VtEncoder::new_with_quality(enc_w, enc_h, fps, bitrate, mode_444, quality) {
            Ok(encoder) => {
                tracing::info!(
                    enc_w,
                    enc_h,
                    mode_444,
                    "Using VideoToolbox hardware encoder (GPU)"
                );
                return Ok(Box::new(encoder));
            }
            Err(e) => {
                tracing::warn!("VideoToolbox unavailable: {e}, falling back to OpenH264");
            }
        }
    }

    // Software / Auto: OpenH264 CPU encoder (full P-frame support)
    tracing::info!(
        enc_w,
        enc_h,
        mode_444,
        "Using OpenH264 software encoder (CPU)"
    );
    let encoder = OpenH264Encoder::new_with_quality(enc_w, enc_h, fps, bitrate, mode_444, quality)?;
    Ok(Box::new(encoder))
}

#[cfg(test)]
mod transport_selection_tests {
    use super::*;

    #[test]
    fn explicit_bitmap_never_constructs_a_video_encoder() {
        assert_eq!(
            EncoderPreference::from_str_opt(Some("bitmap")),
            EncoderPreference::Bitmap
        );
        for quality in [
            Quality::LowLatency,
            Quality::Balanced,
            Quality::HighQuality,
            Quality::Lossless,
        ] {
            let error = create_encoder(
                16,
                16,
                30.0,
                quality,
                EncoderPreference::Bitmap,
                false,
                1_000_000,
            )
            .err()
            .expect("Bitmap selection must bypass all native codecs");
            assert!(error.to_string().contains("Bitmap desktop transport"));
        }
    }
}

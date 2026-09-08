use anyhow::Result;
use bytes::Bytes;
use ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout;
use ironrdp_server::{
    gfx::GfxState, BitmapUpdate, DesktopSize, DisplayUpdate, GfxFrameUpdate, GfxUncompressedUpdate,
    PixelFormat as RdpPixelFormat, RdpServerDisplay, RdpServerDisplayUpdates, UncompressedRect,
};
use macrdp_audio::SharedAudioTx;
use macrdp_capture::{
    CaptureConfig, CapturePixelFormat, CapturedFrame, CgFallbackCapturer, FrameData, Rect,
    ScreenCapturer,
};
use macrdp_encode::{self, Quality, VideoEncoder};
use std::num::{NonZeroU16, NonZeroUsize};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::handler::MouseCoordMapper;
use crate::perf_stats::SharedPerfStats;

/// Maximum tile size for bitmap updates
const TILE_SIZE: u16 = 64;

/// Maximum total dirty area (in pixels) for the uncompressed GFX path.
/// Below this threshold, raw pixels are sent instead of H.264 encoding.
/// 65536 pixels ≈ 256×256 rect ≈ 256 KB of raw BGRA data.
const UNCOMPRESSED_MAX_PIXELS: u32 = 65536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClampedRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

fn prepare_uncompressed_dirty_rects(
    frame_width: u32,
    frame_height: u32,
    dirty_rects: &[Rect],
) -> Vec<ClampedRect> {
    let mut rects: Vec<ClampedRect> = dirty_rects
        .iter()
        .filter(|r| r.width > 0 && r.height > 0)
        .filter_map(|r| {
            let x = r.x.min(frame_width.saturating_sub(1));
            let y = r.y.min(frame_height.saturating_sub(1));
            let width = r.width.min(frame_width - x);
            let height = r.height.min(frame_height - y);
            (width > 0 && height > 0).then_some(ClampedRect {
                x,
                y,
                width,
                height,
            })
        })
        .collect();

    if rects.len() > 1 {
        let total_area: u32 = rects.iter().map(|r| r.width * r.height).sum();
        let min_x = rects.iter().map(|r| r.x).min().unwrap_or(0);
        let min_y = rects.iter().map(|r| r.y).min().unwrap_or(0);
        let max_x = rects.iter().map(|r| r.x + r.width).max().unwrap_or(0);
        let max_y = rects.iter().map(|r| r.y + r.height).max().unwrap_or(0);
        let bbox = ClampedRect {
            x: min_x,
            y: min_y,
            width: max_x.saturating_sub(min_x),
            height: max_y.saturating_sub(min_y),
        };
        let bbox_area = bbox.width.saturating_mul(bbox.height);
        if bbox.width > 0
            && bbox.height > 0
            && bbox_area <= UNCOMPRESSED_MAX_PIXELS
            && bbox_area <= total_area.saturating_mul(2)
        {
            rects.clear();
            rects.push(bbox);
        }
    }

    rects
}

fn extract_bgra_rect(bgra: &[u8], stride: usize, rect: ClampedRect) -> Option<Bytes> {
    let bpp = 4usize;
    let row_bytes = rect.width as usize * bpp;
    let total_bytes = rect.height as usize * row_bytes;
    let mut pixels = vec![0u8; total_bytes];

    for row in 0..rect.height as usize {
        let src_y = rect.y as usize + row;
        let src_offset = src_y * stride + rect.x as usize * bpp;
        let src_end = src_offset + row_bytes;
        if src_end > bgra.len() {
            return None;
        }
        let dst_offset = row * row_bytes;
        pixels[dst_offset..dst_offset + row_bytes].copy_from_slice(&bgra[src_offset..src_end]);
    }

    Some(Bytes::from(pixels))
}

/// Convert a captured frame into tiled BitmapUpdate chunks
pub fn frame_to_bitmap_updates(frame: &CapturedFrame, tile_size: u16) -> Vec<BitmapUpdate> {
    let bgra = match frame.data.as_bgra_bytes() {
        Some(b) => b,
        None => return Vec::new(), // PixelBuffer frames don't go through bitmap path
    };
    let mut updates = Vec::new();
    let bpp: usize = 4;

    let cols = (frame.width as u16 + tile_size - 1) / tile_size;
    let rows = (frame.height as u16 + tile_size - 1) / tile_size;

    for row in 0..rows {
        for col in 0..cols {
            let x = col * tile_size;
            let y = row * tile_size;
            let w = (frame.width as u16 - x).min(tile_size);
            let h = (frame.height as u16 - y).min(tile_size);

            let Some(width) = NonZeroU16::new(w) else {
                continue;
            };
            let Some(height) = NonZeroU16::new(h) else {
                continue;
            };

            let mut tile_data = Vec::with_capacity(w as usize * h as usize * bpp);
            for dy in 0..h {
                let src_y = (y + dy) as usize;
                let src_x_start = x as usize * bpp;
                let src_x_end = src_x_start + w as usize * bpp;
                let row_start = src_y * frame.stride;
                if row_start + src_x_end <= bgra.len() {
                    tile_data
                        .extend_from_slice(&bgra[row_start + src_x_start..row_start + src_x_end]);
                }
            }

            let stride = w as usize * bpp;
            let Some(stride) = NonZeroUsize::new(stride) else {
                continue;
            };

            updates.push(BitmapUpdate {
                x,
                y,
                width,
                height,
                format: RdpPixelFormat::BgrA32,
                data: Bytes::from(tile_data),
                stride,
                dirty_rects: Vec::new(),
            });
        }
    }

    updates
}

/// Display adapter that bridges ScreenCapturer to ironrdp-server
pub struct MacDisplay {
    width: u16,
    height: u16,
    /// Maximum capture resolution (physical display resolution).
    /// Client can resize freely up to this limit.
    max_width: u16,
    max_height: u16,
    /// Shared mouse coordinate mapper — updated on resize
    coord_mapper: MouseCoordMapper,
    /// Whether resolution is fixed by config (true) or follows client (false)
    fixed_resolution: bool,
    frame_rate: u32,
    quality: Quality,
    encoder_pref: macrdp_encode::EncoderPreference,
    /// Whether AVC444 mode is requested by config
    mode_444: bool,
    show_cursor: bool,
    base_bitrate: u32,
    gfx_state: Arc<Mutex<GfxState>>,
    /// Shared audio sender slot — updated per connection by AudioFactory
    shared_audio_tx: Option<SharedAudioTx>,
    /// Shared performance statistics collector (None = disabled)
    perf_stats: Option<SharedPerfStats>,
}

impl MacDisplay {
    fn apply_resize(&mut self, width: u16, height: u16, source: &str) {
        let w = width.min(self.max_width);
        let h = height.min(self.max_height);
        if w > 0 && h > 0 && (w != self.width || h != self.height) {
            tracing::info!(
                old_w = self.width,
                old_h = self.height,
                new_w = w,
                new_h = h,
                source,
                "Applying resolution change"
            );
            self.width = w;
            self.height = h;
            self.base_bitrate = macrdp_encode::screen_bitrate(
                w as u32,
                h as u32,
                self.frame_rate as f32,
                self.quality,
            );
            self.coord_mapper.update_rdp_size(w, h);
        }
    }

    fn apply_layout_resize(&mut self, layout: DisplayControlMonitorLayout) {
        let Some(primary) = layout
            .monitors()
            .iter()
            .find(|monitor| monitor.is_primary())
        else {
            tracing::warn!("Ignoring display layout update with no primary monitor");
            return;
        };

        let (requested_w, requested_h) = primary.dimensions();
        let requested_w = requested_w.clamp(1, u32::from(u16::MAX)) as u16;
        let requested_h = requested_h.clamp(1, u32::from(u16::MAX)) as u16;
        tracing::info!(
            requested_w,
            requested_h,
            monitor_count = layout.monitors().len(),
            desktop_scale_factor = primary.desktop_scale_factor(),
            "Applying Display Control layout update"
        );
        self.apply_resize(requested_w, requested_h, "display-control");
    }

    pub fn new(
        width: u16,
        height: u16,
        fixed_resolution: bool,
        frame_rate: u32,
        quality: Quality,
        encoder_pref: macrdp_encode::EncoderPreference,
        mode_444: bool,
        show_cursor: bool,
        bitrate_override: Option<u32>,
        gfx_state: Arc<Mutex<GfxState>>,
        coord_mapper: MouseCoordMapper,
        shared_audio_tx: Option<SharedAudioTx>,
        perf_stats: Option<SharedPerfStats>,
    ) -> Self {
        let base_bitrate = bitrate_override.unwrap_or_else(|| {
            macrdp_encode::screen_bitrate(width as u32, height as u32, frame_rate as f32, quality)
        });
        tracing::info!(
            base_bitrate_mbps = base_bitrate as f64 / 1_000_000.0,
            "Base bitrate"
        );
        // Max resolution: use physical display size so clients can resize freely.
        // SCK scales_to_fit handles any resolution.
        let (max_w, max_h) = macrdp_capture::detect_display_pixel_size()
            .map(|(physical_w, physical_h)| {
                (width.max(physical_w as u16), height.max(physical_h as u16))
            })
            .unwrap_or((width.max(3840), height.max(2160)));
        Self {
            width,
            height,
            max_width: max_w,
            max_height: max_h,
            coord_mapper,
            fixed_resolution,
            frame_rate,
            quality,
            encoder_pref,
            mode_444,
            show_cursor,
            base_bitrate,
            gfx_state,
            shared_audio_tx,
            perf_stats,
        }
    }
}

#[async_trait::async_trait]
impl RdpServerDisplay for MacDisplay {
    async fn size(&mut self) -> DesktopSize {
        DesktopSize {
            width: self.width,
            height: self.height,
        }
    }

    fn request_layout(&mut self, layout: DisplayControlMonitorLayout) {
        if self.fixed_resolution {
            tracing::debug!("Ignoring Display Control layout — resolution is fixed by config");
            return;
        }
        self.apply_layout_resize(layout);
    }

    fn request_resize(&mut self, width: u16, height: u16) {
        if self.fixed_resolution {
            tracing::debug!("Ignoring resize request — resolution is fixed by config");
            return;
        }
        self.apply_resize(width, height, "client-requested");
    }

    async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
        let capture_config = CaptureConfig {
            width: self.width as u32,
            height: self.height as u32,
            frame_rate: self.frame_rate,
            pixel_format: if self.encoder_pref == macrdp_encode::EncoderPreference::Hardware
                && !self.mode_444
            {
                CapturePixelFormat::Nv12
            } else {
                CapturePixelFormat::Bgra
            },
            show_cursor: self.show_cursor,
        };
        // Read the current audio sender from the shared slot (set by AudioFactory per connection)
        let audio_tx = self
            .shared_audio_tx
            .as_ref()
            .and_then(|shared| shared.lock().unwrap().clone());
        let capturer = ScreenCapturer::new(capture_config.clone(), audio_tx).await?;

        // Create H.264 encoder with configured quality and encoder preference
        let encoder = macrdp_encode::create_encoder(
            self.width as u32,
            self.height as u32,
            self.frame_rate as f32,
            self.quality,
            self.encoder_pref,
            self.mode_444,
            self.base_bitrate,
        )
        .ok();

        if encoder.is_some() {
            tracing::info!("H.264 encoder available — will use GFX path when client supports it");
        }

        Ok(Box::new(MacDisplayUpdates {
            capturer,
            capture_config,
            encoder,
            gfx_state: Arc::clone(&self.gfx_state),
            coord_mapper: self.coord_mapper.clone(),
            base_bitrate: self.base_bitrate,
            mode_444: self.mode_444,
            display_frame_count: 0,
            skip_next_frame: false,
            overload_count: 0,
            perf_stats: self.perf_stats.clone(),
        }))
    }
}

struct MacDisplayUpdates {
    capturer: ScreenCapturer,
    capture_config: CaptureConfig,
    encoder: Option<Box<dyn VideoEncoder>>,
    gfx_state: Arc<Mutex<GfxState>>,
    coord_mapper: MouseCoordMapper,
    base_bitrate: u32,
    mode_444: bool,
    display_frame_count: u64,
    skip_next_frame: bool,
    /// Counter for rate-limiting encode overload warnings
    overload_count: u64,
    /// Shared performance statistics collector (None = disabled)
    perf_stats: Option<SharedPerfStats>,
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for MacDisplayUpdates {
    async fn next_update(&mut self) -> Result<Option<DisplayUpdate>> {
        // Drain stale frames — always use the latest available frame.
        // If SCK capturer stops (e.g. screen locked), fall back to CGDisplayCreateImage
        // which works at the display level (including lock screen).
        let frame = loop {
            let event = match self.capturer.next_frame().await {
                Some(e) => e,
                None => {
                    if let Some(update) = self.run_coregraphics_fallback_until_recovered().await? {
                        return Ok(Some(update));
                    }
                    continue; // retry next_frame with restored SCK capturer
                }
            };
            // Handle CaptureEvent — extract frame or skip idle
            let frame = match event {
                macrdp_capture::CaptureEvent::Frame(f) => f,
                macrdp_capture::CaptureEvent::Idle => continue,
                macrdp_capture::CaptureEvent::Unavailable => {
                    if let Some(update) = self.run_coregraphics_fallback_until_recovered().await? {
                        return Ok(Some(update));
                    }
                    continue;
                }
            };
            // If another frame is already buffered, skip this one and grab the newer one
            // This prevents frame queuing which adds latency
            match self.capturer.try_next_frame() {
                Some(macrdp_capture::CaptureEvent::Frame(_newer)) => continue,
                Some(macrdp_capture::CaptureEvent::Idle)
                | Some(macrdp_capture::CaptureEvent::Unavailable) => break frame,
                None => break frame,
            }
        };

        self.encode_and_send(frame)
    }
}

impl MacDisplayUpdates {
    async fn run_coregraphics_fallback_until_recovered(&mut self) -> Result<Option<DisplayUpdate>> {
        tracing::warn!("SCStream unavailable — switching to CoreGraphics fallback");
        let fallback = CgFallbackCapturer::new(&self.capture_config);
        loop {
            match ScreenCapturer::new(self.capture_config.clone(), None).await {
                Ok(mut new_capturer) => {
                    match tokio::time::timeout(
                        Duration::from_millis(250),
                        new_capturer.next_frame(),
                    )
                    .await
                    {
                        Ok(Some(macrdp_capture::CaptureEvent::Frame(frame))) => {
                            tracing::info!(
                                "SCStream recovered with a usable frame — switching back from CoreGraphics"
                            );
                            self.capturer = new_capturer;
                            return self.encode_and_send(frame);
                        }
                        Ok(Some(macrdp_capture::CaptureEvent::Idle)) => {
                            tracing::info!(
                                "SCStream recovered with idle frame flow — switching back from CoreGraphics"
                            );
                            self.capturer = new_capturer;
                            return Ok(None);
                        }
                        Ok(Some(macrdp_capture::CaptureEvent::Unavailable)) | Ok(None) | Err(_) => {
                            tracing::debug!(
                                "SCStream probe still unavailable during CoreGraphics fallback"
                            );
                        }
                    }
                }
                Err(_) => {
                    tracing::debug!("SCStream probe creation failed during CoreGraphics fallback");
                }
            }
            if let Some(cg_frame) = fallback.capture_frame() {
                return self.encode_and_send(cg_frame);
            }
            tokio::time::sleep(fallback.frame_interval()).await;
        }
    }

    fn encode_and_send(&mut self, frame: CapturedFrame) -> Result<Option<DisplayUpdate>> {
        self.coord_mapper.update_frame_content_rect(
            frame.width,
            frame.height,
            frame.content_rect.as_ref(),
        );

        // Encode overload protection: skip this frame if previous encode took too long
        if self.skip_next_frame {
            self.skip_next_frame = false;
            return Ok(Some(DisplayUpdate::DefaultPointer));
        }

        // Check GFX state and AVC444 negotiation
        let (gfx_ready, use_444) = {
            let state = self.gfx_state.lock().unwrap();
            let ready = state.is_ready() && self.encoder.is_some();
            let use_444 = self.mode_444 && state.avc444_supported && state.avc444_enabled;
            (ready, use_444)
        };

        if gfx_ready {
            // GFX uncompressed path — for small dirty regions, skip H.264 encoding entirely
            if !frame.dirty_rects.is_empty() {
                let total_area: u32 = frame.dirty_rects.iter().map(|r| r.width * r.height).sum();

                if total_area > 0 && total_area <= UNCOMPRESSED_MAX_PIXELS {
                    let prepared_rects = prepare_uncompressed_dirty_rects(
                        frame.width,
                        frame.height,
                        &frame.dirty_rects,
                    );
                    let rects: Vec<UncompressedRect> = match &frame.data {
                        FrameData::Raw(bgra) => prepared_rects
                            .iter()
                            .filter_map(|r| {
                                let pixel_data = extract_bgra_rect(bgra, frame.stride, *r)?;
                                Some(UncompressedRect {
                                    x: r.x as u16,
                                    y: r.y as u16,
                                    width: r.width as u16,
                                    height: r.height as u16,
                                    pixel_data,
                                })
                            })
                            .collect(),
                        FrameData::BgraPixelBuffer(buf) => buf
                            .with_locked_bgra(frame.width, frame.height, |bgra, stride| {
                                prepared_rects
                                    .iter()
                                    .filter_map(|r| {
                                        let pixel_data = extract_bgra_rect(bgra, stride, *r)?;
                                        Some(UncompressedRect {
                                            x: r.x as u16,
                                            y: r.y as u16,
                                            width: r.width as u16,
                                            height: r.height as u16,
                                            pixel_data,
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        FrameData::PixelBuffer(_) => Vec::new(),
                    };

                    if !rects.is_empty() {
                        self.display_frame_count += 1;
                        let total_bytes: usize = rects.iter().map(|r| r.pixel_data.len()).sum();
                        {
                            let mut st = self.gfx_state.lock().unwrap();
                            st.last_encode_ms = 0.0; // zero encode time
                            st.last_frame_bytes = total_bytes as u32;
                        }
                        tracing::debug!(
                            display_frame = self.display_frame_count,
                            rects = rects.len(),
                            total_pixels = total_area,
                            total_bytes,
                            "Display: sending GFX uncompressed dirty rects"
                        );
                        return Ok(Some(DisplayUpdate::GfxUncompressed(
                            GfxUncompressedUpdate {
                                rects,
                                width: frame.width as u16,
                                height: frame.height as u16,
                            },
                        )));
                    }
                    // PixelBuffer frames or extraction failure — fall through to H.264
                }
            }

            // GFX H.264 path — always send at capture rate, never block on acks
            if let Some(encoder) = &mut self.encoder {
                self.display_frame_count += 1;
                let t0 = std::time::Instant::now();

                // Route based on frame data type
                match &frame.data {
                    FrameData::PixelBuffer(buf) => {
                        // Zero-copy VT path — encode CVPixelBuffer directly
                        match encoder.encode_pixel_buffer(buf.as_ptr(), false) {
                            Ok(encoded) if !encoded.data.is_empty() => {
                                let encode_ms = t0.elapsed().as_secs_f64() * 1000.0;
                                let is_keyframe = encoded.is_keyframe;
                                tracing::debug!(
                                    display_frame = self.display_frame_count,
                                    h264_bytes = encoded.data.len(),
                                    is_keyframe,
                                    encode_ms = format!("{:.1}", encode_ms),
                                    "Display: sending zero-copy GFX frame"
                                );
                                {
                                    let mut st = self.gfx_state.lock().unwrap();
                                    st.last_encode_ms = encode_ms;
                                    st.last_frame_bytes = encoded.data.len() as u32;
                                }
                                if let Some(ps) = &self.perf_stats {
                                    ps.lock().unwrap().record_frame(
                                        encode_ms,
                                        encoded.data.len() as u32,
                                        is_keyframe,
                                    );
                                }
                                let frame_interval_ms =
                                    1000.0 / self.capture_config.frame_rate as f64;
                                if encode_ms > frame_interval_ms * 0.95 {
                                    self.skip_next_frame = true;
                                    self.overload_count += 1;
                                    // Rate-limit log: warn once per ~60 overloads, debug otherwise
                                    if self.overload_count % 60 == 1 {
                                        tracing::warn!(
                                            encode_ms = format!("{:.1}", encode_ms),
                                            frame_interval_ms = format!("{:.1}", frame_interval_ms),
                                            overload_total = self.overload_count,
                                            "encode overload — skipping frames"
                                        );
                                    }
                                }
                                return Ok(Some(DisplayUpdate::GfxFrame(GfxFrameUpdate {
                                    h264_data: encoded.data,
                                    width: frame.width as u16,
                                    height: frame.height as u16,
                                    enc_width: encoded.width as u16,
                                    enc_height: encoded.height as u16,
                                    is_keyframe,
                                    h264_aux: None,
                                })));
                            }
                            Ok(_) => {
                                tracing::warn!("Zero-copy encode returned empty data");
                                return Ok(Some(DisplayUpdate::DefaultPointer));
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Zero-copy encode failed: {e}, falling back to DefaultPointer"
                                );
                                return Ok(Some(DisplayUpdate::DefaultPointer));
                            }
                        }
                    }
                    FrameData::BgraPixelBuffer(buf) => {
                        let encoded =
                            match buf.with_locked_bgra(frame.width, frame.height, |bgra, stride| {
                                if use_444 && encoder.supports_444() {
                                    match encoder.encode_bgra_444(
                                        bgra,
                                        frame.width,
                                        frame.height,
                                        stride,
                                    ) {
                                        Ok(encoded) if !encoded.main_view.data.is_empty() => {
                                            return Ok(Some(DisplayUpdate::GfxFrame(
                                                GfxFrameUpdate {
                                                    h264_data: encoded.main_view.data,
                                                    width: frame.width as u16,
                                                    height: frame.height as u16,
                                                    enc_width: encoded.main_view.width as u16,
                                                    enc_height: encoded.main_view.height as u16,
                                                    is_keyframe: encoded.main_view.is_keyframe,
                                                    h264_aux: Some(encoded.aux_view.data),
                                                },
                                            )));
                                        }
                                        Ok(_) => {
                                            tracing::warn!(
                                                display_frame = self.display_frame_count,
                                                "AVC444 encode returned EMPTY data — frame dropped!"
                                            );
                                            return Ok(None);
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                display_frame = self.display_frame_count,
                                                "AVC444 encode failed: {e}, falling back to AVC420"
                                            );
                                        }
                                    }
                                }

                                match encoder.encode_bgra(bgra, frame.width, frame.height, stride) {
                                    Ok(encoded) if !encoded.data.is_empty() => {
                                        Ok(Some(DisplayUpdate::GfxFrame(GfxFrameUpdate {
                                            h264_data: encoded.data,
                                            width: frame.width as u16,
                                            height: frame.height as u16,
                                            enc_width: encoded.width as u16,
                                            enc_height: encoded.height as u16,
                                            is_keyframe: encoded.is_keyframe,
                                            h264_aux: None,
                                        })))
                                    }
                                    Ok(_) => {
                                        tracing::warn!(
                                            display_frame = self.display_frame_count,
                                            "H.264 encode returned EMPTY data — frame dropped!"
                                        );
                                        Ok(None)
                                    }
                                    Err(e) => Err(e),
                                }
                            }) {
                                Some(result) => result?,
                                None => {
                                    tracing::warn!("BGRA pixel buffer lock failed during encode");
                                    return Ok(Some(DisplayUpdate::DefaultPointer));
                                }
                            };

                        if let Some(DisplayUpdate::GfxFrame(update)) = encoded {
                            let encode_ms = t0.elapsed().as_secs_f64() * 1000.0;
                            let total_bytes = update.h264_data.len()
                                + update.h264_aux.as_ref().map_or(0, |d| d.len());
                            let is_keyframe = update.is_keyframe;
                            tracing::debug!(
                                display_frame = self.display_frame_count,
                                h264_bytes = update.h264_data.len(),
                                aux_bytes = update.h264_aux.as_ref().map_or(0, |d| d.len()),
                                is_keyframe,
                                encode_ms = format!("{:.1}", encode_ms),
                                "Display: sending BGRA pixel buffer GFX frame"
                            );
                            {
                                let mut st = self.gfx_state.lock().unwrap();
                                st.last_encode_ms = encode_ms;
                                st.last_frame_bytes = total_bytes as u32;
                            }
                            if let Some(ps) = &self.perf_stats {
                                ps.lock().unwrap().record_frame(
                                    encode_ms,
                                    total_bytes as u32,
                                    is_keyframe,
                                );
                            }
                            let frame_interval_ms = 1000.0 / self.capture_config.frame_rate as f64;
                            if encode_ms > frame_interval_ms * 0.95 {
                                self.skip_next_frame = true;
                                self.overload_count += 1;
                                if self.overload_count % 60 == 1 {
                                    tracing::warn!(
                                        encode_ms = format!("{:.1}", encode_ms),
                                        frame_interval_ms = format!("{:.1}", frame_interval_ms),
                                        overload_total = self.overload_count,
                                        "encode overload — skipping frames"
                                    );
                                }
                            }
                            return Ok(Some(DisplayUpdate::GfxFrame(update)));
                        }

                        return Ok(Some(DisplayUpdate::DefaultPointer));
                    }
                    FrameData::Raw(_) => {
                        // Existing BGRA encode path — continues below
                    }
                }
                let bgra = frame.data.as_bgra_bytes().unwrap();

                // AVC444 dual-stream path
                if use_444 && encoder.supports_444() {
                    match encoder.encode_bgra_444(bgra, frame.width, frame.height, frame.stride) {
                        Ok(encoded) if !encoded.main_view.data.is_empty() => {
                            let encode_ms = t0.elapsed().as_secs_f64() * 1000.0;
                            let total_bytes =
                                encoded.main_view.data.len() + encoded.aux_view.data.len();
                            let is_keyframe = encoded.main_view.is_keyframe;
                            tracing::debug!(
                                display_frame = self.display_frame_count,
                                main_bytes = encoded.main_view.data.len(),
                                aux_bytes = encoded.aux_view.data.len(),
                                is_keyframe,
                                encode_ms = format!("{:.1}", encode_ms),
                                "Display: sending AVC444 GFX frame"
                            );
                            {
                                let mut st = self.gfx_state.lock().unwrap();
                                st.last_encode_ms = encode_ms;
                                st.last_frame_bytes = total_bytes as u32;
                            }
                            if let Some(ps) = &self.perf_stats {
                                ps.lock().unwrap().record_frame(
                                    encode_ms,
                                    total_bytes as u32,
                                    is_keyframe,
                                );
                            }
                            let frame_interval_ms = 1000.0 / self.capture_config.frame_rate as f64;
                            if encode_ms > frame_interval_ms * 0.95 {
                                self.skip_next_frame = true;
                                self.overload_count += 1;
                                if self.overload_count % 60 == 1 {
                                    tracing::warn!(
                                        encode_ms = format!("{:.1}", encode_ms),
                                        frame_interval_ms = format!("{:.1}", frame_interval_ms),
                                        overload_total = self.overload_count,
                                        "encode overload — skipping frames"
                                    );
                                }
                            }
                            return Ok(Some(DisplayUpdate::GfxFrame(GfxFrameUpdate {
                                h264_data: encoded.main_view.data,
                                width: frame.width as u16,
                                height: frame.height as u16,
                                enc_width: encoded.main_view.width as u16,
                                enc_height: encoded.main_view.height as u16,
                                is_keyframe,
                                h264_aux: Some(encoded.aux_view.data),
                            })));
                        }
                        Ok(_) => {
                            tracing::warn!(
                                display_frame = self.display_frame_count,
                                "AVC444 encode returned EMPTY data — frame dropped!"
                            );
                            return Ok(Some(DisplayUpdate::DefaultPointer));
                        }
                        Err(e) => {
                            tracing::warn!(
                                display_frame = self.display_frame_count,
                                "AVC444 encode failed: {e}, falling back to AVC420"
                            );
                            // Fall through to AVC420 path below
                        }
                    }
                }

                // AVC420 path (default or fallback from AVC444 failure)
                match encoder.encode_bgra(bgra, frame.width, frame.height, frame.stride) {
                    Ok(encoded) if !encoded.data.is_empty() => {
                        let encode_ms = t0.elapsed().as_secs_f64() * 1000.0;
                        let is_keyframe = encoded.is_keyframe;
                        tracing::debug!(
                            display_frame = self.display_frame_count,
                            h264_bytes = encoded.data.len(),
                            is_keyframe,
                            encode_ms = format!("{:.1}", encode_ms),
                            "Display: sending GFX frame"
                        );
                        {
                            let mut st = self.gfx_state.lock().unwrap();
                            st.last_encode_ms = encode_ms;
                            st.last_frame_bytes = encoded.data.len() as u32;
                        }
                        if let Some(ps) = &self.perf_stats {
                            ps.lock().unwrap().record_frame(
                                encode_ms,
                                encoded.data.len() as u32,
                                is_keyframe,
                            );
                        }
                        let frame_interval_ms = 1000.0 / self.capture_config.frame_rate as f64;
                        if encode_ms > frame_interval_ms * 0.95 {
                            self.skip_next_frame = true;
                            self.overload_count += 1;
                            if self.overload_count % 60 == 1 {
                                tracing::warn!(
                                    encode_ms = format!("{:.1}", encode_ms),
                                    frame_interval_ms = format!("{:.1}", frame_interval_ms),
                                    overload_total = self.overload_count,
                                    "encode overload — skipping frames"
                                );
                            }
                        }
                        return Ok(Some(DisplayUpdate::GfxFrame(GfxFrameUpdate {
                            h264_data: encoded.data,
                            width: frame.width as u16,
                            height: frame.height as u16,
                            enc_width: encoded.width as u16,
                            enc_height: encoded.height as u16,
                            is_keyframe,
                            h264_aux: None,
                        })));
                    }
                    Ok(_) => {
                        tracing::warn!(
                            display_frame = self.display_frame_count,
                            "H.264 encode returned EMPTY data — frame dropped!"
                        );
                        return Ok(Some(DisplayUpdate::DefaultPointer));
                    }
                    Err(e) => {
                        tracing::warn!(
                            display_frame = self.display_frame_count,
                            "H.264 encode failed: {e}"
                        );
                    }
                }
            }
        } else if self.encoder.is_some() {
            // H.264 encoder exists — never send bitmaps, wait for GFX to become ready.
            // Mixing bitmap and GFX causes 0xd06 DECOMPRESSION_FAILED on reconnect.
            return Ok(Some(DisplayUpdate::DefaultPointer));
        }

        // Bitmap path (only when GFX is not available at all)
        let build_bitmap_update =
            |bgra_bitmap: &[u8], stride_usize: usize| -> Option<DisplayUpdate> {
                if !frame.dirty_rects.is_empty() {
                    let mut min_x = frame.width;
                    let mut min_y = frame.height;
                    let mut max_x = 0u32;
                    let mut max_y = 0u32;

                    for r in &frame.dirty_rects {
                        min_x = min_x.min(r.x);
                        min_y = min_y.min(r.y);
                        max_x = max_x.max(r.x + r.width);
                        max_y = max_y.max(r.y + r.height);
                    }

                    max_x = max_x.min(frame.width);
                    max_y = max_y.min(frame.height);

                    if max_x > min_x && max_y > min_y {
                        let w = max_x - min_x;
                        let h = max_y - min_y;
                        let width = NonZeroU16::new(w as u16)?;
                        let height = NonZeroU16::new(h as u16)?;
                        let bpp = 4usize;
                        let dirty_stride = w as usize * bpp;
                        let mut dirty_data = Vec::with_capacity(dirty_stride * h as usize);
                        for row in min_y..max_y {
                            let src_offset = row as usize * stride_usize + min_x as usize * bpp;
                            let src_end = src_offset + dirty_stride;
                            if src_end <= bgra_bitmap.len() {
                                dirty_data.extend_from_slice(&bgra_bitmap[src_offset..src_end]);
                            }
                        }

                        let stride = NonZeroUsize::new(dirty_stride)?;
                        return Some(DisplayUpdate::Bitmap(BitmapUpdate {
                            x: min_x as u16,
                            y: min_y as u16,
                            width,
                            height,
                            format: RdpPixelFormat::BgrA32,
                            data: Bytes::from(dirty_data),
                            stride,
                            dirty_rects: Vec::new(),
                        }));
                    }
                }

                let width = NonZeroU16::new(frame.width as u16)?;
                let height = NonZeroU16::new(frame.height as u16)?;
                let stride = NonZeroUsize::new(stride_usize)?;
                Some(DisplayUpdate::Bitmap(BitmapUpdate {
                    x: 0,
                    y: 0,
                    width,
                    height,
                    format: RdpPixelFormat::BgrA32,
                    data: Bytes::copy_from_slice(bgra_bitmap),
                    stride,
                    dirty_rects: Vec::new(),
                }))
            };

        match &frame.data {
            FrameData::Raw(bytes) => Ok(build_bitmap_update(bytes, frame.stride)),
            FrameData::BgraPixelBuffer(buf) => Ok(buf
                .with_locked_bgra(frame.width, frame.height, |bgra, stride| {
                    build_bitmap_update(bgra, stride)
                })
                .flatten()
                .or(Some(DisplayUpdate::DefaultPointer))),
            FrameData::PixelBuffer(_) => {
                tracing::warn!("PixelBuffer frame in bitmap path — should not happen");
                Ok(Some(DisplayUpdate::DefaultPointer))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_to_bitmap_updates() {
        let frame = CapturedFrame {
            width: 100,
            height: 50,
            data: FrameData::Raw(Bytes::from(vec![0u8; 100 * 50 * 4])),
            stride: 400,
            timestamp_us: 0,
            dirty_rects: vec![],
            content_rect: None,
        };

        let updates = frame_to_bitmap_updates(&frame, 64);
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].x, 0);
        assert_eq!(updates[0].width.get(), 64);
        assert_eq!(updates[1].x, 64);
        assert_eq!(updates[1].width.get(), 36);
    }

    #[test]
    fn test_frame_to_bitmap_updates_exact_tile() {
        let frame = CapturedFrame {
            width: 128,
            height: 64,
            data: FrameData::Raw(Bytes::from(vec![0u8; 128 * 64 * 4])),
            stride: 512,
            timestamp_us: 0,
            dirty_rects: vec![],
            content_rect: None,
        };

        let updates = frame_to_bitmap_updates(&frame, 64);
        assert_eq!(updates.len(), 2);
    }
}

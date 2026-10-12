//! macOS screen capture via ScreenCaptureKit

mod audio;
mod cursor;
mod fallback_image;
mod geometry;
pub use cursor::{CursorCapture, CursorUpdate};
mod mailbox;
use audio::AudioOutputHandler;
pub use audio::{
    audio_channel, AudioCaptureSnapshot, AudioCaptureStats, AudioFrame, AudioFrameReceiver,
    AudioFrameSender, MAX_AUDIO_AGE,
};
pub use geometry::{ContentRect, MouseCoordMapper};
pub use mailbox::CaptureDeliveryStats;

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use core_graphics::access::ScreenCaptureAccess;
use screencapturekit::cv::CVPixelBuffer;
use screencapturekit::prelude::*;
use screencapturekit::stream::configuration::{AudioChannelCount, AudioSampleRate};

static UNEXPECTED_BGRA_LAYOUT_WARNINGS: AtomicU64 = AtomicU64::new(0);
static INVALID_FRAME_GEOMETRY_WARNINGS: AtomicU64 = AtomicU64::new(0);
static FALLBACK_IMAGE_WARNINGS: AtomicU64 = AtomicU64::new(0);

/// Check if Screen Recording permission is granted (no prompt)
pub fn check_screen_recording_permission() -> bool {
    ScreenCaptureAccess::default().preflight()
}

/// Request Screen Recording permission (triggers system dialog if not granted)
/// Returns true if already granted. Note: even after granting, the app
/// may need to be restarted for the permission to take effect.
pub fn request_screen_recording_permission() -> bool {
    ScreenCaptureAccess::default().request()
}

/// Open System Settings to Privacy & Security page
pub fn open_screen_recording_settings() {
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        .spawn();
}

/// A rectangle region
#[derive(Clone, Debug)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Pixel format for screen capture output
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CapturePixelFormat {
    /// BGRA 32-bit (default, needed for OpenH264 and bitmap fallback path)
    Bgra,
    /// NV12 (420f full-range) — zero-copy to VideoToolbox, no color conversion needed
    Nv12,
}

/// Frame pixel data — either raw BGRA bytes or a zero-copy CVPixelBuffer reference
#[derive(Debug)]
pub enum FrameData {
    /// BGRA raw bytes copied from CVPixelBuffer (existing behavior)
    Raw(Bytes),
    /// IOSurface-backed CVPixelBuffer containing BGRA pixels.
    /// Kept zero-copy until a downstream consumer explicitly locks and reads it.
    BgraPixelBuffer(SafePixelBuffer),
    /// IOSurface-backed CVPixelBuffer — zero copy, passed directly to VideoToolbox
    PixelBuffer(SafePixelBuffer),
}

impl FrameData {
    /// Get raw BGRA bytes if this is a Raw frame. Returns None for PixelBuffer frames.
    pub fn as_bgra_bytes(&self) -> Option<&[u8]> {
        match self {
            FrameData::Raw(bytes) => Some(bytes),
            FrameData::BgraPixelBuffer(_) | FrameData::PixelBuffer(_) => None,
        }
    }
}

/// Events from the screen capture pipeline.
#[derive(Debug)]
pub enum CaptureEvent {
    /// A complete frame with pixel data ready for encoding.
    Frame(CapturedFrame),
    /// Desktop is unchanged — no new pixel data.
    Idle,
    /// ScreenCaptureKit reported that capture is temporarily unavailable,
    /// such as when the display is blanked, suspended, or stopped.
    Unavailable,
}

/// A captured screen frame
#[derive(Debug)]
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    pub data: FrameData,
    /// Bytes per row for BGRA-backed frames. Zero for NV12 PixelBuffer mode.
    pub stride: usize,
    pub timestamp_us: u64,
    /// Regions that changed since the last frame.
    /// Empty means info unavailable — treat as full frame change.
    pub dirty_rects: Vec<Rect>,
    /// Visible display content inside this frame. `None` means the whole frame.
    pub content_rect: Option<ContentRect>,
}

#[derive(Clone, Debug)]
pub struct CursorShape {
    pub width: u16,
    pub height: u16,
    pub hot_x: u16,
    pub hot_y: u16,
    pub rgba: Vec<u8>,
}

/// Configuration for screen capture
#[derive(Clone)]
pub struct CaptureConfig {
    /// Pinned CoreGraphics display ID. Capture never falls back to another display.
    pub display_id: u32,
    pub width: u32,
    pub height: u32,
    pub frame_rate: u32,
    pub pixel_format: CapturePixelFormat,
    pub show_cursor: bool,
}

/// Screen capturer using ScreenCaptureKit
pub struct ScreenCapturer {
    stream: SCStream,
    stopped: bool,
    native_stopped: Arc<AtomicBool>,
    frame_rx: Arc<mailbox::FrameMailbox>,
    config: CaptureConfig,
    actual_width: u32,
    actual_height: u32,
    captures_audio: bool,
}

struct CaptureStreamDelegate {
    native_stopped: Arc<AtomicBool>,
    frames: mailbox::FrameSender,
}

impl SCStreamDelegateTrait for CaptureStreamDelegate {
    fn did_stop_with_error(&self, error: screencapturekit::error::SCError) {
        // This is Apple's didStopWithError notification, not a failed request
        // to stop. It confirms a system-terminated stream is already retired.
        self.native_stopped.store(true, Ordering::Release);
        self.frames.send(CaptureEvent::Unavailable);
        tracing::warn!(%error, "Screen capture stopped by the system");
    }
}

/// Frame delivery is independent of synchronous ScreenCaptureKit controls.
/// A native worker can retain the stream while the server awaits this mailbox.
pub struct CaptureReceiver(Arc<mailbox::FrameMailbox>);

impl CaptureReceiver {
    pub async fn next_frame(&mut self) -> Option<CaptureEvent> {
        self.0.recv().await
    }

    pub fn try_latest_image(&mut self) -> Option<CapturedFrame> {
        self.0.try_recv_frame()
    }

    pub fn last_delivery_stats(&self) -> CaptureDeliveryStats {
        self.0.last_delivery_stats()
    }
}

struct VideoOutputHandler {
    frame_tx: mailbox::FrameSender,
    pixel_format: CapturePixelFormat,
}

impl SCStreamOutputTrait for VideoOutputHandler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
        if of_type != SCStreamOutputType::Screen {
            return;
        }

        let event = match self.pixel_format {
            CapturePixelFormat::Nv12 => extract_frame_nv12(&sample),
            CapturePixelFormat::Bgra => extract_frame(&sample),
        };
        let Some(event) = event else { return };

        self.frame_tx.send(event);
    }
}

fn build_stream_config(
    width: u32,
    height: u32,
    config: &CaptureConfig,
    captures_audio: bool,
) -> SCStreamConfiguration {
    let frame_interval = CMTime::new(1, config.frame_rate as i32);

    SCStreamConfiguration::new()
        .with_width(width)
        .with_height(height)
        .with_scales_to_fit(true)
        .with_minimum_frame_interval(&frame_interval)
        .with_pixel_format(match config.pixel_format {
            CapturePixelFormat::Nv12 => PixelFormat::YCbCr_420f,
            CapturePixelFormat::Bgra => PixelFormat::BGRA,
        })
        .with_shows_cursor(config.show_cursor)
        .with_captures_audio(captures_audio)
        .with_sample_rate(AudioSampleRate::Rate48000)
        .with_channel_count(AudioChannelCount::Stereo)
        .with_excludes_current_process_audio(true)
}

/// Normalize metadata at the capture boundary so downstream paths always use pixels.
fn frame_content_pixels(sample: &CMSampleBuffer) -> Option<ContentRect> {
    let rect = sample.content_rect().map(|rect| ContentRect {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    });
    let scale_factor = sample.scale_factor();
    let pixels = rect
        .zip(scale_factor)
        .and_then(|(rect, scale)| rect.from_surface_points(scale));
    if pixels.is_none() {
        let count = INVALID_FRAME_GEOMETRY_WARNINGS.fetch_add(1, Ordering::Relaxed) + 1;
        if count <= 3 {
            tracing::warn!(?rect, ?scale_factor,
                "ScreenCaptureKit frame geometry missing or invalid; using full-frame pointer mapping");
        }
    }
    tracing::trace!(
        content_rect_points = ?rect,
        ?scale_factor, content_scale = ?sample.content_scale(), ?pixels,
        "ScreenCaptureKit content geometry"
    );
    pixels
}

fn extract_frame(sample: &CMSampleBuffer) -> Option<CaptureEvent> {
    use screencapturekit::cm::SCFrameStatus;

    // Idle means desktop is unchanged — propagate as CaptureEvent::Idle.
    // Blank/Suspended/Stopped mean ScreenCaptureKit has no displayable pixels
    // for us right now, which should trigger the higher-level fallback path.
    match sample.frame_status() {
        Some(SCFrameStatus::Idle) => {
            return Some(CaptureEvent::Idle);
        }
        Some(SCFrameStatus::Blank)
        | Some(SCFrameStatus::Suspended)
        | Some(SCFrameStatus::Stopped) => {
            return Some(CaptureEvent::Unavailable);
        }
        _ => {}
    }

    let pixel_buffer: CVPixelBuffer = sample.image_buffer()?;
    let pixel_format = unsafe { CVPixelBufferGetPixelFormatType(pixel_buffer.as_ptr()) };

    let width = pixel_buffer.width() as u32;
    let height = pixel_buffer.height() as u32;
    let stride = unsafe { CVPixelBufferGetBytesPerRow(pixel_buffer.as_ptr()) };
    let raw_len = unsafe { CVPixelBufferGetDataSize(pixel_buffer.as_ptr()) };

    if width == 0 || height == 0 {
        return None;
    }

    let min_stride = width as usize * 4;
    let required_len = (height as usize)
        .saturating_sub(1)
        .saturating_mul(stride)
        .saturating_add(min_stride);
    if pixel_format != K_CV_PIXEL_FORMAT_32BGRA || stride < min_stride || raw_len < required_len {
        let warn_count = UNEXPECTED_BGRA_LAYOUT_WARNINGS.fetch_add(1, Ordering::Relaxed) + 1;
        if warn_count <= 3 || warn_count % 60 == 0 {
            tracing::warn!(
                warning_count = warn_count,
                width,
                height,
                stride,
                raw_len,
                min_stride,
                required_len,
                pixel_format = format_pixel_format(pixel_format),
                "ScreenCaptureKit returned a non-BGRA or invalid BGRA frame in BGRA mode — dropping frame early"
            );
        }
        return None;
    }

    // Extract dirty rects from the sample buffer
    // Extract dirty rects — screencapturekit's CGRect has x/y/width/height fields
    let dirty_rects = sample
        .dirty_rects()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.width > 0.0 && r.height > 0.0)
        .map(|r| Rect {
            x: r.x.max(0.0) as u32,
            y: r.y.max(0.0) as u32,
            width: r.width as u32,
            height: r.height as u32,
        })
        .collect::<Vec<_>>();

    let content_rect = frame_content_pixels(sample);

    let safe_buf = unsafe { SafePixelBuffer::from_raw(pixel_buffer.as_ptr()) };

    let t = sample.presentation_timestamp();
    let timestamp_us = if t.timescale > 0 {
        ((t.value as u128 * 1_000_000) / t.timescale as u128) as u64
    } else {
        0
    };

    Some(CaptureEvent::Frame(CapturedFrame {
        width,
        height,
        data: FrameData::BgraPixelBuffer(safe_buf),
        stride,
        timestamp_us,
        dirty_rects,
        content_rect,
    }))
}

/// Extract a frame in NV12 mode — zero-copy CVPixelBuffer wrapped as SafePixelBuffer.
/// The pixel buffer is retained and passed through the channel without locking or copying.
fn extract_frame_nv12(sample: &CMSampleBuffer) -> Option<CaptureEvent> {
    use screencapturekit::cm::SCFrameStatus;

    // Idle means desktop is unchanged — propagate as CaptureEvent::Idle.
    // Blank/Suspended/Stopped mean ScreenCaptureKit has no displayable pixels
    // for us right now, which should trigger the higher-level fallback path.
    match sample.frame_status() {
        Some(SCFrameStatus::Idle) => {
            return Some(CaptureEvent::Idle);
        }
        Some(SCFrameStatus::Blank)
        | Some(SCFrameStatus::Suspended)
        | Some(SCFrameStatus::Stopped) => {
            return Some(CaptureEvent::Unavailable);
        }
        _ => {}
    }

    let pixel_buffer: CVPixelBuffer = sample.image_buffer()?;

    // width()/height() read from the CVPixelBuffer header — no lock required
    let width = pixel_buffer.width() as u32;
    let height = pixel_buffer.height() as u32;

    if width == 0 || height == 0 {
        return None;
    }

    // Extract dirty rects (same logic as the BGRA path)
    let dirty_rects = sample
        .dirty_rects()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.width > 0.0 && r.height > 0.0)
        .map(|r| Rect {
            x: r.x.max(0.0) as u32,
            y: r.y.max(0.0) as u32,
            width: r.width as u32,
            height: r.height as u32,
        })
        .collect::<Vec<_>>();

    let content_rect = frame_content_pixels(sample);

    let t = sample.presentation_timestamp();
    let timestamp_us = if t.timescale > 0 {
        ((t.value as u128 * 1_000_000) / t.timescale as u128) as u64
    } else {
        0
    };

    // Zero-copy: retain the CVPixelBuffer and wrap it as SafePixelBuffer
    let safe_buf = unsafe { SafePixelBuffer::from_raw(pixel_buffer.as_ptr()) };

    Some(CaptureEvent::Frame(CapturedFrame {
        width,
        height,
        data: FrameData::PixelBuffer(safe_buf),
        stride: 0, // Not applicable for NV12 PixelBuffer mode
        timestamp_us,
        dirty_rects,
        content_rect,
    }))
}

/// Query the main display's physical pixel size.
///
/// This must be used for auto-resolution on macOS scaled modes, where the
/// framebuffer size is often not an integer multiple of the logical size.
pub fn detect_display_pixel_size() -> Result<(u32, u32)> {
    use core_graphics::display::CGDisplay;
    let main = CGDisplay::main();
    let width = main.pixels_wide() as u32;
    let height = main.pixels_high() as u32;
    if width == 0 || height == 0 {
        anyhow::bail!("CGDisplay returned zero pixel size");
    }
    Ok((width, height))
}

/// Detect the main display's native scale factor (1 for non-Retina, 2 for Retina).
pub fn detect_display_scale() -> Result<u32> {
    let (physical_w, _) = detect_display_pixel_size()?;
    let content = SCShareableContent::get().context("Failed to get shareable content")?;
    let display = content
        .displays()
        .into_iter()
        .find(|display| display.display_id() == core_graphics::display::CGDisplay::main().id)
        .context("Main display is not available to ScreenCaptureKit")?;
    let logical_w = display.width();
    let scale = if logical_w > 0 {
        physical_w / logical_w
    } else {
        1
    };
    Ok(scale.max(1))
}

/// Query the main display's resolution (from ScreenCaptureKit, used for capture sizing)
pub fn detect_display_size() -> Result<(u32, u32)> {
    let content = SCShareableContent::get().context("Failed to get shareable content")?;
    let display = content
        .displays()
        .into_iter()
        .find(|display| display.display_id() == core_graphics::display::CGDisplay::main().id)
        .context("Main display is not available to ScreenCaptureKit")?;
    Ok((display.width() as u32, display.height() as u32))
}

/// Query the main display's logical bounds from CoreGraphics.
/// This is the coordinate system CGEvent uses, and MUST be used for mouse mapping.
/// May differ from SCDisplay dimensions on non-standard scaling modes.
pub fn detect_cg_display_size() -> Result<(u32, u32)> {
    use core_graphics::display::CGDisplay;
    let main = CGDisplay::main();
    let bounds = main.bounds();
    let w = bounds.size.width as u32;
    let h = bounds.size.height as u32;
    if w == 0 || h == 0 {
        anyhow::bail!("CGDisplay returned zero bounds");
    }
    Ok((w, h))
}

impl ScreenCapturer {
    /// Create a screen capturer for the exact display selected by the session.
    ///
    /// If `audio_tx` is `Some`, audio capture is enabled and frames are sent
    /// to the provided channel as `AudioFrame` (interleaved Float32 PCM, 48 kHz stereo).
    /// Must run on the capture owner thread: both enumeration and start wait
    /// synchronously for Apple completion callbacks, without a native timeout.
    pub fn new_blocking(config: CaptureConfig, audio_tx: Option<AudioFrameSender>) -> Result<Self> {
        let content = SCShareableContent::get()
            .context("Failed to get shareable content (Screen Recording permission needed)")?;

        let display = content
            .displays()
            .into_iter()
            .find(|display| display.display_id() == config.display_id)
            .with_context(|| {
                format!(
                    "Selected display {} is not available to ScreenCaptureKit",
                    config.display_id
                )
            })?;

        let actual_width = if config.width == 0 {
            display.width() as u32
        } else {
            config.width
        };
        let actual_height = if config.height == 0 {
            display.height() as u32
        } else {
            config.height
        };

        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build();

        let captures_audio = audio_tx.is_some();
        let stream_config =
            build_stream_config(actual_width, actual_height, &config, captures_audio);

        let frame_rx = Arc::new(mailbox::FrameMailbox::default());
        let frame_tx = mailbox::FrameSender(frame_rx.clone());

        let video_handler = VideoOutputHandler {
            frame_tx,
            pixel_format: config.pixel_format,
        };

        let native_stopped = Arc::new(AtomicBool::new(false));
        let mut stream = SCStream::new_with_delegate(
            &filter,
            &stream_config,
            CaptureStreamDelegate {
                native_stopped: native_stopped.clone(),
                frames: mailbox::FrameSender(frame_rx.clone()),
            },
        );
        stream.add_output_handler(video_handler, SCStreamOutputType::Screen);

        // Register audio handler only if a sender was provided
        if let Some(tx) = audio_tx {
            let audio_handler = AudioOutputHandler { audio_tx: tx };
            stream.add_output_handler(audio_handler, SCStreamOutputType::Audio);
        }

        // Establish the retirement guard before start. If Apple reports an
        // error or Rust unwinds, cleanup still confirms stop on this owner
        // thread before its process-wide capture lease can be released.
        let mut capturer = Self {
            stream,
            stopped: false,
            native_stopped,
            frame_rx,
            config,
            actual_width,
            actual_height,
            captures_audio,
        };
        if let Err(error) = capturer.stream.start_capture() {
            // A returned completion error confirms that this new stream failed
            // to start. Calling stop repeatedly would receive "not running"
            // and incorrectly quarantine the process for a normal permission
            // or display-availability failure. A pending/panicking start still
            // retains the guard and must confirm retirement.
            capturer.stopped = true;
            return Err(error).context("Failed to start capture");
        }

        tracing::info!(
            display_id = capturer.config.display_id,
            width = actual_width,
            height = actual_height,
            fps = capturer.config.frame_rate,
            pixel_format = ?capturer.config.pixel_format,
            audio = captures_audio,
            "Screen capture started"
        );

        Ok(capturer)
    }

    pub fn receiver(&self) -> CaptureReceiver {
        CaptureReceiver(self.frame_rx.clone())
    }

    pub fn frame_rate(&self) -> u32 {
        self.config.frame_rate
    }

    /// Receive the next capture event (async, cancellation safe)
    pub async fn next_frame(&mut self) -> Option<CaptureEvent> {
        self.frame_rx.recv().await
    }

    /// Try to get a buffered capture event without waiting. Returns None if no event ready.
    pub fn try_next_frame(&mut self) -> Option<CaptureEvent> {
        self.frame_rx.try_recv()
    }

    /// Refresh queued bitmap pixels without consuming capture status events.
    pub fn try_latest_image(&mut self) -> Option<CapturedFrame> {
        self.frame_rx.try_recv_frame()
    }

    pub fn last_delivery_stats(&self) -> CaptureDeliveryStats {
        self.frame_rx.last_delivery_stats()
    }

    /// Wait for ScreenCaptureKit to finish stopping before changing display modes.
    ///
    /// The upstream API waits for Apple's completion callback and has no timeout.
    /// A failure is returned without marking the stream stopped, so a caller can
    /// abort reconfiguration and cleanup can retry. The dedicated capture owner
    /// isolates this wait while retaining exclusive process-wide ownership.
    pub fn stop(&mut self) -> Result<()> {
        if self.stopped || self.native_stopped.load(Ordering::Acquire) {
            self.stopped = true;
            return Ok(());
        }
        if let Err(error) = self.stream.stop_capture() {
            // A system stop can race the explicit stop completion. Only the
            // delegate's native acknowledgement permits treating it as done;
            // never infer native state from a localized error string.
            if !self.native_stopped.load(Ordering::Acquire) {
                return Err(error).context("Failed to stop screen capture");
            }
        }
        self.stopped = true;
        tracing::debug!(
            display_id = self.config.display_id,
            "Screen capture stopped"
        );
        Ok(())
    }

    /// Update capture frame rate at runtime.
    /// Note: SCStream::update_configuration() blocks until completion via internal semaphore.
    pub fn set_frame_rate(&mut self, fps: u32) -> anyhow::Result<()> {
        let mut next_config = self.config.clone();
        next_config.frame_rate = fps;
        let config = build_stream_config(
            self.actual_width,
            self.actual_height,
            &next_config,
            self.captures_audio,
        );
        self.stream
            .update_configuration(&config)
            .map_err(|e| anyhow::anyhow!("Failed to update SCStream frame rate: {:?}", e))?;
        self.config = next_config;
        tracing::info!(
            fps,
            width = self.actual_width,
            height = self.actual_height,
            pixel_format = ?self.config.pixel_format,
            audio = self.captures_audio,
            "Capture frame rate updated"
        );
        Ok(())
    }
}

impl Drop for ScreenCapturer {
    fn drop(&mut self) {
        // SCStream's own Drop only releases its reference; it does not wait for
        // capture to stop. Keep callbacks and their Rust context alive until the
        // stop completion arrives, including disconnect and recovery teardown.
        let mut retry_delay = std::time::Duration::from_millis(250);
        let mut warned = false;
        loop {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.stop()))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Native capture stop panicked")));
            let Err(error) = result else { break };
            // All ScreenCapturer instances belong to a dedicated native owner.
            // Even startup failure/unwinding must not release callbacks or the
            // owner's display lease while a native stream could remain alive.
            if !warned {
                tracing::warn!(display_id = self.config.display_id, %error, "Capture cleanup could not confirm stop; retaining native ownership");
                warned = true;
            }
            std::thread::sleep(retry_delay);
            retry_delay = (retry_delay * 2).min(std::time::Duration::from_secs(2));
        }
    }
}

/// Fallback capturer using CGDisplayCreateImage (CoreGraphics).
/// Works during lock screen because it captures at the display level,
/// below the window server / ScreenCaptureKit layer.
pub struct CgFallbackCapturer {
    display_id: u32,
    width: u32,
    height: u32,
    frame_interval: std::time::Duration,
}

impl CgFallbackCapturer {
    /// Keep fallback capture pinned to the session's selected display.
    pub fn new(config: &CaptureConfig) -> Self {
        let fps = if config.frame_rate > 0 {
            config.frame_rate
        } else {
            30
        };
        Self {
            display_id: config.display_id,
            width: config.width,
            height: config.height,
            frame_interval: std::time::Duration::from_micros(1_000_000 / fps as u64),
        }
    }

    /// Capture a single frame using CGDisplayCreateImage
    pub fn capture_frame(&self) -> Option<CapturedFrame> {
        let display = core_graphics::display::CGDisplay::new(self.display_id);
        let image = display.image()?;
        let normalized = match fallback_image::normalize(&image) {
            Ok(normalized) => normalized,
            Err(error) => {
                let count = FALLBACK_IMAGE_WARNINGS.fetch_add(1, Ordering::Relaxed) + 1;
                if count <= 3 || count % 60 == 0 {
                    tracing::warn!(
                        warning_count = count,
                        width = image.width(),
                        height = image.height(),
                        bits_per_component = image.bits_per_component(),
                        bits_per_pixel = image.bits_per_pixel(),
                        %error,
                        "Unable to normalize CoreGraphics fallback image to BGRA8"
                    );
                }
                return None;
            }
        };

        Some(CapturedFrame {
            // Report the actual captured image size. The encoder/display path can
            // pad or scale from a smaller fallback frame, but lying about the
            // visible dimensions causes BGRA out-of-bounds reads downstream.
            width: normalized.width,
            height: normalized.height,
            data: FrameData::Raw(Bytes::from(normalized.pixels)),
            stride: normalized.stride,
            timestamp_us: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros() as u64,
            dirty_rects: vec![],
            content_rect: None,
        })
    }

    /// Frame interval for pacing
    pub fn frame_interval(&self) -> std::time::Duration {
        self.frame_interval
    }
}

// ---------------------------------------------------------------------------
// CoreVideo FFI for CVPixelBuffer retain/release and plane access
// ---------------------------------------------------------------------------

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferRetain(pixel_buffer: *mut c_void) -> *mut c_void;
    fn CVPixelBufferRelease(pixel_buffer: *mut c_void);
    fn CVPixelBufferLockBaseAddress(pixel_buffer: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pixel_buffer: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(pixel_buffer: *mut c_void) -> *mut u8;
    fn CVPixelBufferGetBytesPerRow(pixel_buffer: *mut c_void) -> usize;
    fn CVPixelBufferGetDataSize(pixel_buffer: *mut c_void) -> usize;
    fn CVPixelBufferGetBaseAddressOfPlane(pixel_buffer: *mut c_void, plane: usize) -> *mut u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(pixel_buffer: *mut c_void, plane: usize) -> usize;
    fn CVPixelBufferGetHeightOfPlane(pixel_buffer: *mut c_void, plane: usize) -> usize;
    fn CVPixelBufferGetPixelFormatType(pixel_buffer: *mut c_void) -> u32;
}

/// kCVPixelBufferLock_ReadOnly
const CV_PIXEL_BUFFER_LOCK_READ_ONLY: u64 = 0x0000_0001;
const K_CV_PIXEL_FORMAT_32BGRA: u32 = 0x42475241; // 'BGRA'

fn format_pixel_format(pixel_format: u32) -> String {
    let bytes = pixel_format.to_be_bytes();
    let printable = bytes.iter().all(|b| matches!(*b, 0x20..=0x7e));
    if printable {
        format!("{} (0x{pixel_format:08x})", String::from_utf8_lossy(&bytes))
    } else {
        format!("0x{pixel_format:08x}")
    }
}

// ---------------------------------------------------------------------------
// NV12PlaneData — extracted Y and UV plane data from an NV12 pixel buffer
// ---------------------------------------------------------------------------

/// Holds copied plane data from an NV12 CVPixelBuffer.
/// Used for the OpenH264 software encoding fallback path.
pub struct NV12PlaneData {
    /// Y (luma) plane data, one byte per pixel, row-major
    pub y_data: Vec<u8>,
    /// Y plane stride (bytes per row, may include padding)
    pub y_stride: usize,
    /// UV (chroma) plane data, interleaved U/V, half resolution
    pub uv_data: Vec<u8>,
    /// UV plane stride (bytes per row, may include padding)
    pub uv_stride: usize,
    /// Width of the Y plane in pixels
    pub width: usize,
    /// Height of the Y plane in pixels
    pub height: usize,
}

// ---------------------------------------------------------------------------
// SafePixelBuffer — RAII wrapper around a retained CVPixelBufferRef
// ---------------------------------------------------------------------------

/// A safe RAII wrapper around a `CVPixelBufferRef` that manages the
/// retain/release lifecycle. Intended for zero-copy frame passing to
/// VideoToolbox (hardware encoder) while also supporting a locked-read
/// path for OpenH264 (software encoder fallback).
///
/// # Safety
///
/// The inner pointer must originate from a valid `CVPixelBufferRef`.
/// `Send` is implemented because IOSurface-backed pixel buffers are safe
/// to transfer across threads. `Sync` is deliberately NOT implemented
/// because `CVPixelBufferLockBaseAddress` / `UnlockBaseAddress` are not
/// safe for concurrent access from multiple threads.
pub struct SafePixelBuffer {
    ptr: *mut c_void,
}

// SAFETY: IOSurface-backed CVPixelBuffers can be sent across threads.
// We do NOT implement Sync — lock/unlock is not thread-safe for
// concurrent access.
unsafe impl Send for SafePixelBuffer {}

impl SafePixelBuffer {
    /// Create a `SafePixelBuffer` by retaining the given `CVPixelBufferRef`.
    ///
    /// # Safety
    ///
    /// `ptr` must be a valid, non-null `CVPixelBufferRef`.
    pub unsafe fn from_raw(ptr: *mut c_void) -> Self {
        debug_assert!(!ptr.is_null(), "CVPixelBufferRef must not be null");
        CVPixelBufferRetain(ptr);
        Self { ptr }
    }

    /// Return the raw `CVPixelBufferRef` pointer (e.g. for passing to
    /// VideoToolbox's `VTCompressionSessionEncodeFrame`).
    pub fn as_ptr(&self) -> *mut c_void {
        self.ptr
    }

    /// Create a new `SafePixelBuffer` that shares the same underlying
    /// `CVPixelBuffer`, incrementing its retain count.
    pub fn clone_ref(&self) -> Self {
        unsafe {
            CVPixelBufferRetain(self.ptr);
        }
        Self { ptr: self.ptr }
    }

    /// Lock a BGRA pixel buffer read-only and expose its visible bytes to a closure.
    ///
    /// Returns `None` if the lock fails, the buffer is not BGRA, or the layout is invalid.
    pub fn with_locked_bgra<R>(
        &self,
        width: u32,
        height: u32,
        f: impl FnOnce(&[u8], usize) -> R,
    ) -> Option<R> {
        unsafe {
            let status = CVPixelBufferLockBaseAddress(self.ptr, CV_PIXEL_BUFFER_LOCK_READ_ONLY);
            if status != 0 {
                tracing::warn!(status, "CVPixelBufferLockBaseAddress failed");
                return None;
            }

            let result = self
                .read_locked_bgra(width, height)
                .map(|(pixels, stride)| f(pixels, stride));

            CVPixelBufferUnlockBaseAddress(self.ptr, CV_PIXEL_BUFFER_LOCK_READ_ONLY);
            result
        }
    }

    /// Lock the pixel buffer, copy NV12 plane data out, and unlock.
    ///
    /// This is the software-encoding path: we lock the buffer read-only,
    /// memcpy the Y and UV planes into owned `Vec<u8>`s, then unlock.
    /// The lock is held for the shortest possible duration.
    ///
    /// Returns `None` if the lock fails or plane pointers are null.
    pub fn lock_and_read_nv12(&self) -> Option<NV12PlaneData> {
        unsafe {
            // Lock for read-only access
            let status = CVPixelBufferLockBaseAddress(self.ptr, CV_PIXEL_BUFFER_LOCK_READ_ONLY);
            if status != 0 {
                tracing::warn!(status, "CVPixelBufferLockBaseAddress failed");
                return None;
            }

            let result = self.read_nv12_planes();

            // Always unlock, even if plane read failed
            CVPixelBufferUnlockBaseAddress(self.ptr, CV_PIXEL_BUFFER_LOCK_READ_ONLY);

            result
        }
    }

    unsafe fn read_locked_bgra(&self, width: u32, height: u32) -> Option<(&[u8], usize)> {
        let pixel_format = CVPixelBufferGetPixelFormatType(self.ptr);
        let stride = CVPixelBufferGetBytesPerRow(self.ptr);
        let raw_len = CVPixelBufferGetDataSize(self.ptr);
        let pixels = CVPixelBufferGetBaseAddress(self.ptr);

        if pixels.is_null() || width == 0 || height == 0 {
            return None;
        }

        let min_stride = width as usize * 4;
        let required_len = (height as usize)
            .saturating_sub(1)
            .saturating_mul(stride)
            .saturating_add(min_stride);

        if pixel_format != K_CV_PIXEL_FORMAT_32BGRA || stride < min_stride || raw_len < required_len
        {
            let warn_count = UNEXPECTED_BGRA_LAYOUT_WARNINGS.fetch_add(1, Ordering::Relaxed) + 1;
            if warn_count <= 3 || warn_count % 60 == 0 {
                tracing::warn!(
                    warning_count = warn_count,
                    width,
                    height,
                    stride,
                    raw_len,
                    min_stride,
                    required_len,
                    pixel_format = format_pixel_format(pixel_format),
                    "SafePixelBuffer BGRA lock returned a non-BGRA or invalid layout"
                );
            }
            return None;
        }

        Some((std::slice::from_raw_parts(pixels, required_len), stride))
    }

    /// Read Y and UV planes while the buffer is locked.
    /// Caller must ensure the buffer is locked before calling.
    unsafe fn read_nv12_planes(&self) -> Option<NV12PlaneData> {
        // Plane 0 = Y (luma)
        let y_ptr = CVPixelBufferGetBaseAddressOfPlane(self.ptr, 0);
        let y_stride = CVPixelBufferGetBytesPerRowOfPlane(self.ptr, 0);
        let y_height = CVPixelBufferGetHeightOfPlane(self.ptr, 0);

        // Plane 1 = UV (chroma, interleaved)
        let uv_ptr = CVPixelBufferGetBaseAddressOfPlane(self.ptr, 1);
        let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(self.ptr, 1);
        let uv_height = CVPixelBufferGetHeightOfPlane(self.ptr, 1);

        if y_ptr.is_null() || uv_ptr.is_null() {
            tracing::warn!("NV12 plane base address is null");
            return None;
        }

        let y_len = y_stride * y_height;
        let uv_len = uv_stride * uv_height;

        let y_data = std::slice::from_raw_parts(y_ptr, y_len).to_vec();
        let uv_data = std::slice::from_raw_parts(uv_ptr, uv_len).to_vec();

        // Width is derived from plane 0 stride and pixel format.
        // For NV12 Y plane, each pixel is one byte, but stride may include
        // padding. We use the plane height directly and report stride so
        // callers can handle padding.
        Some(NV12PlaneData {
            y_data,
            y_stride,
            uv_data,
            uv_stride,
            width: y_stride, // conservative: callers should clamp to actual width
            height: y_height,
        })
    }
}

impl Drop for SafePixelBuffer {
    fn drop(&mut self) {
        // SAFETY: ptr was retained in `from_raw`, so we must release it.
        unsafe {
            CVPixelBufferRelease(self.ptr);
        }
    }
}

impl std::fmt::Debug for SafePixelBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SafePixelBuffer")
            .field("ptr", &self.ptr)
            .finish()
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    #[tokio::test]
    async fn native_stop_notification_acknowledges_retirement_and_wakes_frame_delivery() {
        let native_stopped = Arc::new(AtomicBool::new(false));
        let mailbox = Arc::new(mailbox::FrameMailbox::default());
        let mut receiver = CaptureReceiver(mailbox.clone());
        let delegate = CaptureStreamDelegate {
            native_stopped: native_stopped.clone(),
            frames: mailbox::FrameSender(mailbox),
        };
        delegate.did_stop_with_error(screencapturekit::error::SCError::InternalError(
            "simulated system stop".into(),
        ));
        assert!(native_stopped.load(Ordering::Acquire));
        assert!(matches!(
            receiver.next_frame().await,
            Some(CaptureEvent::Unavailable)
        ));
        drop(delegate);
        assert!(receiver.next_frame().await.is_none());
    }
}

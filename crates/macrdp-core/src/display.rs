use anyhow::Result;
use bytes::Bytes;
use ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout;
use ironrdp_server::{
    gfx::GfxState, BitmapUpdate, DesktopSize, DiffRect, DisplayUpdate, GfxFrameUpdate,
    GfxUncompressedUpdate, PixelFormat as RdpPixelFormat, RdpServerDisplay,
    RdpServerDisplayUpdates, UncompressedRect,
};
use macrdp_audio::SharedAudioTx;
use macrdp_capture::{
    capture_current_cursor_shape, CaptureConfig, CapturePixelFormat, CapturedFrame,
    CgFallbackCapturer, CursorShape, FrameData, Rect, SafePixelBuffer, ScreenCapturer,
};
use macrdp_encode::{self, Quality, VideoEncoder};
use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::num::{NonZeroU16, NonZeroUsize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::bitrate_controller::{is_private_ip, BitrateController, FrameStats, NetworkQuality};
use crate::handler::MouseCoordMapper;
use crate::perf_stats::SharedPerfStats;

/// Maximum total dirty-rect area for the uncompressed GFX fast path.
/// Favor fairly large dirty-rect updates over 9 MB full-frame fallbacks.
const UNCOMPRESSED_MAX_PIXELS: u32 = 1_048_576;
/// Keep only genuinely tiny UI/cursor rects on the uncompressed fast path
/// once AVC is available. Larger updates should prefer hardware/software AVC.
const UNCOMPRESSED_TINY_UI_MAX_PIXELS: u32 = 8_192;
const UNCOMPRESSED_DUPLICATE_MIN_INTERVAL: Duration = Duration::from_millis(120);
const UNCOMPRESSED_LARGE_UPDATE_BYTES: usize = 512 * 1024;
const UNCOMPRESSED_LARGE_REPEAT_MIN_INTERVAL: Duration = Duration::from_millis(3_500);
const UNCOMPRESSED_LARGE_GEOMETRY_REPEAT_MIN_INTERVAL: Duration = Duration::from_millis(1_500);
const UNCOMPRESSED_FULL_FRAME_MIN_INTERVAL: Duration = Duration::from_millis(2_000);
// Guacamole never acknowledges our AVC frames, so waiting for a full dozen
// frames just burns several seconds before the usable bitmap path kicks in.
const AVC_NO_ACK_FALLBACK_FRAMES: u32 = 4;
const AVC_NO_ACK_FALLBACK_MAX_AGE: Duration = Duration::from_millis(1_200);
const BITMAP_SPLIT_MAX_REGIONS: usize = 12;
const BITMAP_MERGE_MAX_PIXELS: u32 = 786_432;
const BITMAP_MERGE_AREA_FACTOR_NUM: u32 = 3;
const BITMAP_MERGE_AREA_FACTOR_DEN: u32 = 2;
const BITMAP_FORCE_FULL_DIRTY_RECTS: usize = 24;
const BITMAP_FORCE_FULL_AREA_NUM: u32 = 11;
const BITMAP_FORCE_FULL_AREA_DEN: u32 = 20;
const BITMAP_FORCE_FULL_BBOX_NUM: u32 = 4;
const BITMAP_FORCE_FULL_BBOX_DEN: u32 = 5;
const BITMAP_FORCE_FULL_BBOX_AREA_FLOOR_NUM: u32 = 7;
const BITMAP_FORCE_FULL_BBOX_AREA_FLOOR_DEN: u32 = 25;
const BITMAP_CHAOTIC_FULL_FRAME_MIN_INTERVAL: Duration = Duration::from_millis(3_000);
const BITMAP_FORCED_FULL_UPDATES_AFTER_GFX_FALLBACK: u8 = 2;
const DIRTY_RECT_VALIDATION_TILE: u32 = 96;
/// Default validation mode for Guacamole GFX compatibility.
/// When enabled, negotiated GFX sessions stay on uncompressed updates only,
/// preferring dirty rects and falling back to full-frame uncompressed.
const DEFAULT_FORCE_FULL_FRAME_UNCOMPRESSED_GFX: bool = false;
static SESSION_AUDIT_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionPathKind {
    GfxAvc,
    GfxUncompressed,
    Bitmap,
}

impl SessionPathKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::GfxAvc => "gfx-avc",
            Self::GfxUncompressed => "gfx-uncompressed",
            Self::Bitmap => "bitmap",
        }
    }
}

#[derive(Debug)]
struct SessionAudit {
    session_id: u64,
    started_at: Instant,
    peer_addr: Option<String>,
    first_path: Option<SessionPathKind>,
    current_path: Option<SessionPathKind>,
    path_switches: u32,
    fallback_reason: Option<&'static str>,
    avc_frames: u64,
    avc_bytes: u64,
    avc_encode_ms_total: f64,
    avc_encode_ms_max: f64,
    uncompressed_frames: u64,
    uncompressed_bytes: u64,
    bitmap_frames: u64,
    bitmap_bytes: u64,
    max_dirty_rects: usize,
    max_dirty_pixels: u32,
}

impl SessionAudit {
    fn new(peer_addr: Option<String>) -> Self {
        Self {
            session_id: SESSION_AUDIT_COUNTER.fetch_add(1, Ordering::Relaxed),
            started_at: Instant::now(),
            peer_addr,
            first_path: None,
            current_path: None,
            path_switches: 0,
            fallback_reason: None,
            avc_frames: 0,
            avc_bytes: 0,
            avc_encode_ms_total: 0.0,
            avc_encode_ms_max: 0.0,
            uncompressed_frames: 0,
            uncompressed_bytes: 0,
            bitmap_frames: 0,
            bitmap_bytes: 0,
            max_dirty_rects: 0,
            max_dirty_pixels: 0,
        }
    }

    fn note_fallback_reason(&mut self, reason: &'static str) {
        if self.fallback_reason != Some(reason) {
            self.fallback_reason = Some(reason);
            tracing::warn!(
                "SESSION_AUDIT fallback-reason session_id={} reason={} elapsed_ms={}",
                self.session_id,
                reason,
                self.started_at.elapsed().as_millis() as u64
            );
        }
    }

    fn record(
        &mut self,
        path: SessionPathKind,
        bytes_sent: u32,
        encode_ms: Option<f64>,
        dirty_rects: usize,
        dirty_pixels: u32,
        detail: &'static str,
    ) {
        if self.first_path.is_none() {
            self.first_path = Some(path);
        }
        if self.current_path != Some(path) {
            if let Some(previous) = self.current_path {
                self.path_switches = self.path_switches.saturating_add(1);
                tracing::info!(
                    "SESSION_AUDIT path-switch session_id={} from={} to={} detail={} elapsed_ms={}",
                    self.session_id,
                    previous.as_str(),
                    path.as_str(),
                    detail,
                    self.started_at.elapsed().as_millis() as u64
                );
            } else {
                tracing::info!(
                    "SESSION_AUDIT path-start session_id={} path={} detail={} elapsed_ms={}",
                    self.session_id,
                    path.as_str(),
                    detail,
                    self.started_at.elapsed().as_millis() as u64
                );
            }
            self.current_path = Some(path);
        }

        self.max_dirty_rects = self.max_dirty_rects.max(dirty_rects);
        self.max_dirty_pixels = self.max_dirty_pixels.max(dirty_pixels);

        match path {
            SessionPathKind::GfxAvc => {
                self.avc_frames = self.avc_frames.saturating_add(1);
                self.avc_bytes = self.avc_bytes.saturating_add(bytes_sent as u64);
                if let Some(encode_ms) = encode_ms {
                    self.avc_encode_ms_total += encode_ms;
                    self.avc_encode_ms_max = self.avc_encode_ms_max.max(encode_ms);
                }
            }
            SessionPathKind::GfxUncompressed => {
                self.uncompressed_frames = self.uncompressed_frames.saturating_add(1);
                self.uncompressed_bytes = self.uncompressed_bytes.saturating_add(bytes_sent as u64);
            }
            SessionPathKind::Bitmap => {
                self.bitmap_frames = self.bitmap_frames.saturating_add(1);
                self.bitmap_bytes = self.bitmap_bytes.saturating_add(bytes_sent as u64);
            }
        }
    }

    fn log_summary(&self) {
        let elapsed_ms = self.started_at.elapsed().as_millis() as u64;
        let total_frames = self.avc_frames + self.uncompressed_frames + self.bitmap_frames;
        let total_bytes = self.avc_bytes + self.uncompressed_bytes + self.bitmap_bytes;
        let avc_avg_encode_ms = if self.avc_frames > 0 {
            self.avc_encode_ms_total / self.avc_frames as f64
        } else {
            0.0
        };

        tracing::info!(
            "SESSION_AUDIT summary session_id={} peer={} duration_ms={} total_frames={} total_bytes={} first_path={} final_path={} path_switches={} avc_frames={} avc_bytes={} avc_avg_encode_ms={:.2} avc_max_encode_ms={:.2} uncompressed_frames={} uncompressed_bytes={} bitmap_frames={} bitmap_bytes={} max_dirty_rects={} max_dirty_pixels={} fallback_reason={}",
            self.session_id,
            self.peer_addr.as_deref().unwrap_or("unknown"),
            elapsed_ms,
            total_frames,
            total_bytes,
            self.first_path.map(SessionPathKind::as_str).unwrap_or("none"),
            self.current_path.map(SessionPathKind::as_str).unwrap_or("none"),
            self.path_switches,
            self.avc_frames,
            self.avc_bytes,
            avc_avg_encode_ms,
            self.avc_encode_ms_max,
            self.uncompressed_frames,
            self.uncompressed_bytes,
            self.bitmap_frames,
            self.bitmap_bytes,
            self.max_dirty_rects,
            self.max_dirty_pixels,
            self.fallback_reason.unwrap_or("none")
        );
    }
}

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

fn fingerprint_uncompressed_rects(rects: &[UncompressedRect]) -> u64 {
    let mut hasher = DefaultHasher::new();

    for rect in rects {
        rect.x.hash(&mut hasher);
        rect.y.hash(&mut hasher);
        rect.width.hash(&mut hasher);
        rect.height.hash(&mut hasher);
        rect.pixel_data.as_ref().hash(&mut hasher);
    }

    hasher.finish()
}

fn fingerprint_bitmap_update(update: &BitmapUpdate) -> u64 {
    let mut hasher = DefaultHasher::new();
    update.x.hash(&mut hasher);
    update.y.hash(&mut hasher);
    update.width.get().hash(&mut hasher);
    update.height.get().hash(&mut hasher);
    update.stride.get().hash(&mut hasher);
    for rect in &update.dirty_rects {
        rect.x.hash(&mut hasher);
        rect.y.hash(&mut hasher);
        rect.width.hash(&mut hasher);
        rect.height.hash(&mut hasher);
    }
    update.data.as_ref().hash(&mut hasher);
    hasher.finish()
}

fn fingerprint_cursor_shape(shape: &CursorShape) -> u64 {
    let mut hasher = DefaultHasher::new();
    shape.width.hash(&mut hasher);
    shape.height.hash(&mut hasher);
    shape.hot_x.hash(&mut hasher);
    shape.hot_y.hash(&mut hasher);
    shape.rgba.hash(&mut hasher);
    hasher.finish()
}

fn cursor_shape_to_pointer(shape: CursorShape) -> ironrdp_server::ColorPointer {
    let width = usize::from(shape.width);
    let xor_stride = {
        let stride = width * 3;
        stride + (stride % 2)
    };
    let and_stride = {
        let stride = width.div_ceil(8);
        stride + (stride % 2)
    };
    let mut xor_mask = vec![0; usize::from(shape.height) * xor_stride];
    let mut and_mask = vec![0; usize::from(shape.height) * and_stride];

    // Match FreeRDP's alpha-cursor conversion path: 24bpp bottom-up BGR XOR
    // data plus a 1bpp bottom-up AND mask.
    for (dst_row, src_row) in shape.rgba.chunks_exact(width * 4).rev().enumerate() {
        let xor_row = &mut xor_mask[dst_row * xor_stride..(dst_row + 1) * xor_stride];
        let and_row = &mut and_mask[dst_row * and_stride..(dst_row + 1) * and_stride];

        for (x, pixel) in src_row.chunks_exact(4).enumerate() {
            let [r, g, b, a] = [pixel[0], pixel[1], pixel[2], pixel[3]];
            let xor_offset = x * 3;

            if a < 64 {
                and_row[x / 8] |= 0x80 >> (x % 8);
                xor_row[xor_offset..xor_offset + 3].copy_from_slice(&[0, 0, 0]);
            } else {
                xor_row[xor_offset..xor_offset + 3].copy_from_slice(&[b, g, r]);
            }
        }
    }

    ironrdp_server::ColorPointer {
        width: shape.width,
        height: shape.height,
        hot_x: shape.hot_x,
        hot_y: shape.hot_y,
        and_mask,
        xor_mask,
    }
}

fn should_suppress_recent_fingerprint(
    recent: &mut VecDeque<(u64, Instant)>,
    fingerprint: u64,
    now: Instant,
    window: Duration,
    capacity: usize,
) -> bool {
    while let Some((_, seen_at)) = recent.front() {
        if now.duration_since(*seen_at) > window {
            recent.pop_front();
        } else {
            break;
        }
    }

    if recent.iter().any(|(fp, _)| *fp == fingerprint) {
        return true;
    }

    recent.push_back((fingerprint, now));
    while recent.len() > capacity {
        recent.pop_front();
    }

    false
}

fn should_fallback_from_gfx_avc_no_ack(
    frame_id: u32,
    last_ack_frame: u32,
    pending_acks: u32,
    oldest_pending_age: Option<Duration>,
) -> bool {
    if last_ack_frame != 0 || pending_acks == 0 {
        return false;
    }

    (frame_id >= AVC_NO_ACK_FALLBACK_FRAMES && pending_acks >= frame_id)
        || oldest_pending_age.is_some_and(|age| age >= AVC_NO_ACK_FALLBACK_MAX_AGE)
}

fn rect_area(rect: ClampedRect) -> u32 {
    rect.width.saturating_mul(rect.height)
}

fn bgra_rect_changed(
    current: &[u8],
    current_stride: usize,
    previous: &[u8],
    previous_stride: usize,
    rect: ClampedRect,
) -> bool {
    let row_bytes = rect.width as usize * 4;

    for row in 0..rect.height as usize {
        let y = rect.y as usize + row;
        let current_offset = y * current_stride + rect.x as usize * 4;
        let previous_offset = y * previous_stride + rect.x as usize * 4;
        let current_end = current_offset + row_bytes;
        let previous_end = previous_offset + row_bytes;

        if current_end > current.len() || previous_end > previous.len() {
            return true;
        }

        if current[current_offset..current_end] != previous[previous_offset..previous_end] {
            return true;
        }
    }

    false
}

fn validate_dirty_rects_against_previous_bgra(
    frame_width: u32,
    frame_height: u32,
    dirty_rects: &[Rect],
    current: &[u8],
    current_stride: usize,
    previous: &[u8],
    previous_stride: usize,
) -> Vec<Rect> {
    let mut refined = Vec::new();

    for rect in dirty_rects.iter().filter(|r| r.width > 0 && r.height > 0) {
        let x = rect.x.min(frame_width.saturating_sub(1));
        let y = rect.y.min(frame_height.saturating_sub(1));
        let width = rect.width.min(frame_width - x);
        let height = rect.height.min(frame_height - y);
        if width == 0 || height == 0 {
            continue;
        }

        let clamped = ClampedRect {
            x,
            y,
            width,
            height,
        };

        if width <= DIRTY_RECT_VALIDATION_TILE && height <= DIRTY_RECT_VALIDATION_TILE {
            if bgra_rect_changed(current, current_stride, previous, previous_stride, clamped) {
                refined.push(Rect {
                    x,
                    y,
                    width,
                    height,
                });
            }
            continue;
        }

        for tile_y in (y..y + height).step_by(DIRTY_RECT_VALIDATION_TILE as usize) {
            let tile_height = DIRTY_RECT_VALIDATION_TILE.min(y + height - tile_y);
            for tile_x in (x..x + width).step_by(DIRTY_RECT_VALIDATION_TILE as usize) {
                let tile_width = DIRTY_RECT_VALIDATION_TILE.min(x + width - tile_x);
                let tile = ClampedRect {
                    x: tile_x,
                    y: tile_y,
                    width: tile_width,
                    height: tile_height,
                };

                if bgra_rect_changed(current, current_stride, previous, previous_stride, tile) {
                    refined.push(Rect {
                        x: tile_x,
                        y: tile_y,
                        width: tile_width,
                        height: tile_height,
                    });
                }
            }
        }
    }

    refined
}

fn try_merge_bitmap_regions(a: ClampedRect, b: ClampedRect) -> Option<ClampedRect> {
    let left = a.x.min(b.x);
    let top = a.y.min(b.y);
    let right = a.x.saturating_add(a.width).max(b.x.saturating_add(b.width));
    let bottom =
        a.y.saturating_add(a.height)
            .max(b.y.saturating_add(b.height));
    let merged = ClampedRect {
        x: left,
        y: top,
        width: right.saturating_sub(left),
        height: bottom.saturating_sub(top),
    };
    let merged_area = rect_area(merged);
    let separate_area = rect_area(a).saturating_add(rect_area(b));

    (merged_area <= BITMAP_MERGE_MAX_PIXELS
        && merged_area.saturating_mul(BITMAP_MERGE_AREA_FACTOR_DEN)
            <= separate_area.saturating_mul(BITMAP_MERGE_AREA_FACTOR_NUM))
    .then_some(merged)
}

fn prepare_bitmap_dirty_regions(
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

    if rects.len() <= 1 {
        return rects;
    }

    rects.sort_by_key(|r| (r.y, r.x));

    let mut idx = 0;
    while idx < rects.len() {
        let mut next = idx + 1;
        while next < rects.len() {
            if let Some(merged) = try_merge_bitmap_regions(rects[idx], rects[next]) {
                rects[idx] = merged;
                rects.swap_remove(next);
            } else {
                next += 1;
            }
        }
        idx += 1;
    }

    if rects.len() > BITMAP_SPLIT_MAX_REGIONS {
        let total_area: u32 = rects.iter().copied().map(rect_area).sum();
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
        let bbox_area = rect_area(bbox);
        if bbox_area.saturating_mul(BITMAP_MERGE_AREA_FACTOR_DEN)
            <= total_area.saturating_mul(BITMAP_MERGE_AREA_FACTOR_NUM)
        {
            rects.clear();
            rects.push(bbox);
        }
    }

    rects
}

fn should_force_full_bitmap_update(
    frame_width: u32,
    frame_height: u32,
    dirty_rects: &[Rect],
    last_full_frame_age: Option<Duration>,
) -> bool {
    if dirty_rects.is_empty() {
        return false;
    }

    if last_full_frame_age.is_some_and(|age| age < BITMAP_CHAOTIC_FULL_FRAME_MIN_INTERVAL) {
        return false;
    }

    if dirty_rects.len() >= BITMAP_FORCE_FULL_DIRTY_RECTS {
        return true;
    }

    let frame_area = frame_width.saturating_mul(frame_height);
    if frame_area == 0 {
        return true;
    }

    let mut total_area = 0u32;
    let mut min_x = frame_width;
    let mut min_y = frame_height;
    let mut max_x = 0u32;
    let mut max_y = 0u32;

    for rect in dirty_rects.iter().filter(|r| r.width > 0 && r.height > 0) {
        let x = rect.x.min(frame_width.saturating_sub(1));
        let y = rect.y.min(frame_height.saturating_sub(1));
        let width = rect.width.min(frame_width - x);
        let height = rect.height.min(frame_height - y);
        if width == 0 || height == 0 {
            continue;
        }
        total_area = total_area.saturating_add(width.saturating_mul(height));
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x.saturating_add(width));
        max_y = max_y.max(y.saturating_add(height));
    }

    if max_x <= min_x || max_y <= min_y {
        return true;
    }

    let bbox_area = max_x
        .saturating_sub(min_x)
        .saturating_mul(max_y.saturating_sub(min_y));

    let area_is_chaotic = total_area.saturating_mul(BITMAP_FORCE_FULL_AREA_DEN)
        >= frame_area.saturating_mul(BITMAP_FORCE_FULL_AREA_NUM);
    let bbox_sprawls = bbox_area.saturating_mul(BITMAP_FORCE_FULL_BBOX_DEN)
        >= frame_area.saturating_mul(BITMAP_FORCE_FULL_BBOX_NUM);
    let area_floor_for_bbox = total_area.saturating_mul(BITMAP_FORCE_FULL_BBOX_AREA_FLOOR_DEN)
        >= frame_area.saturating_mul(BITMAP_FORCE_FULL_BBOX_AREA_FLOOR_NUM);

    area_is_chaotic || (bbox_sprawls && area_floor_for_bbox)
}

fn should_use_nv12_capture_for_gfx(
    session_uses_gfx: bool,
    gfx_ready: bool,
    avc420_supported: bool,
    last_ack_frame: u32,
    gfx_validation_mode: bool,
    avc_fallback_to_uncompressed: bool,
    bitmap_fallback_mode: bool,
    encoder_pref: macrdp_encode::EncoderPreference,
    mode_444: bool,
    needs_full_gfx_seed: bool,
) -> bool {
    session_uses_gfx
        && gfx_ready
        && avc420_supported
        && last_ack_frame > 0
        && !gfx_validation_mode
        && !avc_fallback_to_uncompressed
        && !bitmap_fallback_mode
        && encoder_pref == macrdp_encode::EncoderPreference::Hardware
        && !mode_444
        && !needs_full_gfx_seed
}

/// Display adapter that bridges ScreenCapturer to ironrdp-server
pub struct MacDisplay {
    width: u16,
    height: u16,
    /// Maximum resolution (auto-detected or configured). Client cannot exceed this.
    max_width: u16,
    max_height: u16,
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
    coord_mapper: MouseCoordMapper,
    /// Shared audio sender slot — updated per connection by AudioFactory
    shared_audio_tx: Option<SharedAudioTx>,
    /// Shared performance statistics collector (None = disabled)
    perf_stats: Option<SharedPerfStats>,
}

impl MacDisplay {
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
            fixed_resolution,
            frame_rate,
            quality,
            encoder_pref,
            mode_444,
            show_cursor,
            base_bitrate,
            gfx_state,
            coord_mapper,
            shared_audio_tx,
            perf_stats,
        }
    }
}

impl MacDisplay {
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

    fn set_size(&mut self, width: u16, height: u16) {
        self.apply_resize(width, height, "server-config");
        self.fixed_resolution = true;
    }

    async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
        let (session_uses_gfx, gfx_avc_ready) = {
            let gfx = self.gfx_state.lock().unwrap();
            (gfx.session_uses_gfx, gfx.is_ready())
        };

        let configured_frame_rate = {
            let gfx = self.gfx_state.lock().unwrap();
            gfx.target_frame_rate.unwrap_or(self.frame_rate).max(1)
        };

        let capture_config = CaptureConfig {
            width: self.width as u32,
            height: self.height as u32,
            frame_rate: configured_frame_rate,
            // Start GFX sessions in BGRA so we can guarantee one visible full-frame
            // seed before switching into the NV12/pipelined hardware path.
            pixel_format: if session_uses_gfx {
                CapturePixelFormat::Bgra
            } else if gfx_avc_ready
                && self.encoder_pref == macrdp_encode::EncoderPreference::Hardware
                && !self.mode_444
            {
                CapturePixelFormat::Nv12
            } else {
                CapturePixelFormat::Bgra
            },
            show_cursor: self.show_cursor,
        };
        tracing::info!(
            session_uses_gfx,
            gfx_avc_ready,
            pixel_format = ?capture_config.pixel_format,
            encoder_pref = ?self.encoder_pref,
            mode_444 = self.mode_444,
            "Capture format selected for connection"
        );
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
            configured_frame_rate as f32,
            self.quality,
            self.encoder_pref,
            self.mode_444,
            self.base_bitrate,
        )
        .ok();

        if encoder.is_some() {
            tracing::info!("H.264 encoder available — will use GFX path when client supports it");
        }

        let peer_addr = {
            let gfx = self.gfx_state.lock().unwrap();
            gfx.peer_addr
        };
        let network = peer_addr
            .map(|ip| NetworkQuality::from_ip(is_private_ip(ip)))
            .unwrap_or(NetworkQuality::from_ip(false));
        let initial_fps = capture_config.frame_rate;
        self.gfx_state.lock().unwrap().applied_frame_rate = Some(initial_fps.max(1));
        let bitrate_ctrl = BitrateController::new(self.base_bitrate, initial_fps, network);

        Ok(Box::new(MacDisplayUpdates {
            capturer,
            capture_config,
            encoder,
            gfx_state: Arc::clone(&self.gfx_state),
            coord_mapper: self.coord_mapper.clone(),
            encoder_pref: self.encoder_pref,
            mode_444: self.mode_444,
            shared_audio_tx: self.shared_audio_tx.clone(),
            display_frame_count: 0,
            skip_next_frame: false,
            overload_count: 0,
            idle_frame_count: 0,
            last_idr_time: Instant::now(),
            last_frame_cache: None,
            last_cursor_fingerprint: None,
            next_cursor_poll_at: Instant::now(),
            last_uncompressed_rects: None,
            last_uncompressed_sent: None,
            last_full_frame_sent: None,
            last_large_uncompressed_fingerprint: None,
            last_large_uncompressed_sent: None,
            last_large_uncompressed_rects: None,
            bitrate_ctrl,
            last_applied_fps: initial_fps,
            has_pending_submit: false,
            pending_visible_size: None,
            pending_frame_time: None,
            needs_full_bitmap_seed: true,
            forced_full_bitmap_updates_remaining: 1,
            pending_bitmap_updates: VecDeque::new(),
            last_full_bitmap_sent: None,
            needs_full_gfx_seed: true,
            logged_gfx_uncompressed_mode: false,
            logged_gfx_avc_mode: false,
            logged_bitmap_mode: false,
            client_may_be_stale: false,
            perf_stats: self.perf_stats.clone(),
            session_audit: SessionAudit::new(peer_addr.map(|ip| ip.to_string())),
        }))
    }
}

/// Cached frame data for idle IDR re-encoding.
enum CachedFrame {
    Bgra {
        data: Bytes,
        width: u32,
        height: u32,
        stride: usize,
    },
    BgraPixelBuffer(SafePixelBuffer, u32, u32, usize), // buffer, width, height, stride
    PixelBuffer(SafePixelBuffer, u32, u32),            // buffer, width, height
}

impl CachedFrame {
    fn width(&self) -> u32 {
        match self {
            CachedFrame::Bgra { width, .. }
            | CachedFrame::BgraPixelBuffer(_, width, _, _)
            | CachedFrame::PixelBuffer(_, width, _) => *width,
        }
    }

    fn height(&self) -> u32 {
        match self {
            CachedFrame::Bgra { height, .. }
            | CachedFrame::BgraPixelBuffer(_, _, height, _)
            | CachedFrame::PixelBuffer(_, _, height) => *height,
        }
    }
}

/// IDR keepalive interval during idle scenes (seconds).
const IDLE_IDR_INTERVAL: Duration = Duration::from_secs(2);
const CURSOR_POLL_INTERVAL: Duration = Duration::from_millis(75);

struct MacDisplayUpdates {
    capturer: ScreenCapturer,
    capture_config: CaptureConfig,
    encoder: Option<Box<dyn VideoEncoder>>,
    gfx_state: Arc<Mutex<GfxState>>,
    coord_mapper: MouseCoordMapper,
    encoder_pref: macrdp_encode::EncoderPreference,
    mode_444: bool,
    shared_audio_tx: Option<SharedAudioTx>,
    display_frame_count: u64,
    skip_next_frame: bool,
    /// Counter for rate-limiting encode overload warnings
    overload_count: u64,
    /// Consecutive idle events from SCK (desktop unchanged).
    idle_frame_count: u32,
    /// When the last IDR keyframe was sent (natural or keepalive).
    last_idr_time: Instant,
    /// Cached last frame for idle IDR re-encoding.
    last_frame_cache: Option<CachedFrame>,
    /// Fingerprint of the last cursor shape emitted to the client.
    last_cursor_fingerprint: Option<u64>,
    /// Deadline for the next host cursor-shape poll.
    next_cursor_poll_at: Instant,
    /// Last uncompressed rect set sent, used to suppress duplicate bursts.
    last_uncompressed_rects: Option<Vec<ClampedRect>>,
    /// Timestamp of the last uncompressed update emission.
    last_uncompressed_sent: Option<Instant>,
    /// Timestamp of the last full-frame uncompressed emission.
    last_full_frame_sent: Option<Instant>,
    /// Fingerprint of the last large uncompressed payload we actually sent.
    last_large_uncompressed_fingerprint: Option<u64>,
    /// Timestamp of the last large uncompressed payload emission.
    last_large_uncompressed_sent: Option<Instant>,
    /// Geometry of the last large uncompressed update, tracked independently so
    /// tiny interleaved UI rects do not reset hotspot suppression.
    last_large_uncompressed_rects: Option<Vec<ClampedRect>>,
    /// Adaptive bitrate/fps controller.
    bitrate_ctrl: BitrateController,
    /// Last fps value applied to the capturer (to avoid redundant updates).
    last_applied_fps: u32,
    /// Whether a pipelined encode has been submitted and not yet collected.
    has_pending_submit: bool,
    /// Visible frame size for the submitted pipelined encode.
    pending_visible_size: Option<(u16, u16)>,
    /// Timestamp of the pending pipelined submit (for encode-time measurement).
    pending_frame_time: Option<Instant>,
    /// Force one full-frame bitmap update before using dirty-rect deltas again.
    needs_full_bitmap_seed: bool,
    /// Number of literal full-frame bitmap updates still required before sparse
    /// dirty-region mode may resume. This counter is decremented only after an
    /// actual full-frame bitmap update has been emitted.
    forced_full_bitmap_updates_remaining: u8,
    /// Additional bitmap updates derived from the last captured frame when
    /// sparse damage is cheaper to send as multiple smaller regions.
    pending_bitmap_updates: VecDeque<BitmapUpdate>,
    /// Timestamp of the last literal full-frame bitmap update emission.
    last_full_bitmap_sent: Option<Instant>,
    /// Force one full-frame GFX update before relying on incremental/pipelined paths.
    needs_full_gfx_seed: bool,
    logged_gfx_uncompressed_mode: bool,
    logged_gfx_avc_mode: bool,
    logged_bitmap_mode: bool,
    client_may_be_stale: bool,
    /// Shared performance statistics collector (None = disabled)
    perf_stats: Option<SharedPerfStats>,
    /// Per-session transport-path audit for post-run log analysis.
    session_audit: SessionAudit,
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for MacDisplayUpdates {
    async fn next_update(&mut self) -> Result<Option<DisplayUpdate>> {
        loop {
            if let Some(bitmap) = self.pending_bitmap_updates.pop_front() {
                self.record_bitmap_update_metrics(bitmap.data.len() as u32);
                self.record_bitmap_audit(&bitmap, "queued-bitmap-region");
                return Ok(Some(DisplayUpdate::Bitmap(bitmap)));
            }

            self.refresh_capture_transport_if_needed().await?;

            if let Some(update) = self.poll_cursor_update_if_due() {
                return Ok(Some(update));
            }

            // Drain stale frames — always use the latest available frame.
            // If SCK capturer stops (e.g. screen locked), fall back to CGDisplayCreateImage
            // which works at the display level (including lock screen).
            let frame = loop {
                // Calculate timeout for idle IDR keepalive.
                // If we have a cached frame and are in idle state, wait at most until
                // the next IDR keepalive is due instead of blocking forever.
                let idle_deadline = if self.idle_frame_count > 0 && self.last_frame_cache.is_some()
                {
                    let elapsed = self.last_idr_time.elapsed();
                    if elapsed >= IDLE_IDR_INTERVAL {
                        // IDR keepalive is overdue — send it immediately
                        if let Some(update) = self.send_idle_idr() {
                            return Ok(Some(update));
                        }
                        // If send_idle_idr returned None (no encoder/gfx), just continue waiting
                        None
                    } else {
                        Some(IDLE_IDR_INTERVAL - elapsed)
                    }
                } else {
                    None
                };

                let cursor_deadline = self.cursor_poll_timeout();
                let (wait_timeout, timeout_should_send_idle_idr) =
                    match (idle_deadline, cursor_deadline) {
                        (Some(idle), Some(cursor)) if idle <= cursor => (Some(idle), true),
                        (Some(_idle), Some(cursor)) => (Some(cursor), false),
                        (Some(idle), None) => (Some(idle), true),
                        (None, Some(cursor)) => (Some(cursor), false),
                        (None, None) => (None, false),
                    };

                let event = if let Some(timeout) = wait_timeout {
                    match tokio::time::timeout(timeout, self.capturer.next_frame()).await {
                        Ok(Some(e)) => e,
                        Ok(None) => {
                            if let Some(update) =
                                self.run_coregraphics_fallback_until_recovered().await?
                            {
                                return Ok(Some(update));
                            }
                            continue;
                        }
                        Err(_elapsed) => {
                            if timeout_should_send_idle_idr {
                                // Timeout — time to send an idle IDR keepalive
                                if let Some(update) = self.send_idle_idr() {
                                    return Ok(Some(update));
                                }
                            }
                            // Cursor poll window elapsed, or no encoder/GFX is available
                            // for an idle keepalive. Continue the display loop.
                            continue;
                        }
                    }
                } else {
                    match self.capturer.next_frame().await {
                        Some(e) => e,
                        None => {
                            if let Some(update) =
                                self.run_coregraphics_fallback_until_recovered().await?
                            {
                                return Ok(Some(update));
                            }
                            continue;
                        }
                    }
                };

                // Handle idle events (desktop unchanged)
                let frame = match event {
                    macrdp_capture::CaptureEvent::Frame(f) => {
                        // Desktop became active — reset idle counter
                        if self.idle_frame_count > 0 {
                            // Collect any pending pipelined frame before recovery
                            if self.has_pending_submit {
                                let _ = self.collect_and_send_pending();
                            }

                            tracing::debug!(
                                idle_frames = self.idle_frame_count,
                                "Desktop active again — forcing keyframe for clean recovery"
                            );
                            if let Some(enc) = &mut self.encoder {
                                enc.force_keyframe();
                            }
                            self.request_full_bitmap_updates(1);
                            self.needs_full_gfx_seed = true;

                            // Reset BitrateController and restore initial bitrate/fps
                            self.bitrate_ctrl.on_idle_recovery();
                            if let Some(enc) = &mut self.encoder {
                                enc.set_bitrate(self.bitrate_ctrl.current_bitrate());
                            }
                            let target_fps = self.bitrate_ctrl.target_fps();
                            self.apply_capture_fps(target_fps, "idle-recovery");
                        }
                        self.idle_frame_count = 0;
                        f
                    }
                    macrdp_capture::CaptureEvent::Idle => {
                        if self.client_may_be_stale {
                            if let Some(update) = self.send_cached_full_frame_recovery() {
                                return Ok(Some(update));
                            }
                        }
                        self.idle_frame_count += 1;
                        continue;
                    }
                    macrdp_capture::CaptureEvent::Unavailable => {
                        if let Some(update) =
                            self.run_coregraphics_fallback_until_recovered().await?
                        {
                            return Ok(Some(update));
                        }
                        continue;
                    }
                };

                // Drain buffered frames so we always encode the newest available frame.
                let mut latest = frame;
                loop {
                    match self.capturer.try_next_frame() {
                        Some(macrdp_capture::CaptureEvent::Frame(newer)) => latest = newer,
                        Some(macrdp_capture::CaptureEvent::Idle)
                        | Some(macrdp_capture::CaptureEvent::Unavailable)
                        | None => break,
                    }
                }
                self.refine_capture_dirty_rects(&mut latest);
                break latest;
            };

            match self.encode_and_send(frame)? {
                Some(update) => return Ok(Some(update)),
                None => {
                    tracing::debug!(
                        display_frame = self.display_frame_count,
                        "Frame produced no display update; continuing display loop"
                    );
                }
            }
        }
    }
}

impl MacDisplayUpdates {
    async fn run_coregraphics_fallback_until_recovered(&mut self) -> Result<Option<DisplayUpdate>> {
        self.idle_frame_count = 0;
        self.request_full_bitmap_updates(1);
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
                match self.encode_and_send(cg_frame)? {
                    Some(update) => return Ok(Some(update)),
                    None => {
                        tracing::debug!(
                            "CoreGraphics fallback frame produced no display update; waiting for next frame"
                        );
                    }
                }
            }
            tokio::time::sleep(fallback.frame_interval()).await;
        }
    }

    fn refine_capture_dirty_rects(&self, frame: &mut CapturedFrame) {
        if frame.dirty_rects.is_empty() {
            return;
        }

        let previous = match self.last_frame_cache.as_ref() {
            Some(previous) => previous,
            None => return,
        };

        let original_count = frame.dirty_rects.len();

        let refined = match (&frame.data, previous) {
            (
                FrameData::Raw(current),
                CachedFrame::Bgra {
                    data: prev,
                    stride,
                    width,
                    height,
                },
            ) if *width == frame.width && *height == frame.height => {
                validate_dirty_rects_against_previous_bgra(
                    frame.width,
                    frame.height,
                    &frame.dirty_rects,
                    current,
                    frame.stride,
                    prev,
                    *stride,
                )
            }
            (FrameData::Raw(current), CachedFrame::BgraPixelBuffer(prev, width, height, _))
                if *width == frame.width && *height == frame.height =>
            {
                prev.with_locked_bgra(frame.width, frame.height, |prev_bgra, prev_stride| {
                    validate_dirty_rects_against_previous_bgra(
                        frame.width,
                        frame.height,
                        &frame.dirty_rects,
                        current,
                        frame.stride,
                        prev_bgra,
                        prev_stride,
                    )
                })
                .unwrap_or_else(|| frame.dirty_rects.clone())
            }
            (
                FrameData::BgraPixelBuffer(current),
                CachedFrame::Bgra {
                    data: prev,
                    stride,
                    width,
                    height,
                },
            ) if *width == frame.width && *height == frame.height => current
                .with_locked_bgra(frame.width, frame.height, |current_bgra, current_stride| {
                    validate_dirty_rects_against_previous_bgra(
                        frame.width,
                        frame.height,
                        &frame.dirty_rects,
                        current_bgra,
                        current_stride,
                        prev,
                        *stride,
                    )
                })
                .unwrap_or_else(|| frame.dirty_rects.clone()),
            (
                FrameData::BgraPixelBuffer(current),
                CachedFrame::BgraPixelBuffer(prev, width, height, _),
            ) if *width == frame.width && *height == frame.height => current
                .with_locked_bgra(frame.width, frame.height, |current_bgra, current_stride| {
                    prev.with_locked_bgra(frame.width, frame.height, |prev_bgra, prev_stride| {
                        validate_dirty_rects_against_previous_bgra(
                            frame.width,
                            frame.height,
                            &frame.dirty_rects,
                            current_bgra,
                            current_stride,
                            prev_bgra,
                            prev_stride,
                        )
                    })
                    .unwrap_or_else(|| frame.dirty_rects.clone())
                })
                .unwrap_or_else(|| frame.dirty_rects.clone()),
            _ => return,
        };

        if refined.len() != original_count {
            tracing::debug!(
                original_dirty_rects = original_count,
                refined_dirty_rects = refined.len(),
                "Refined capture dirty rects against previous frame"
            );
        }

        frame.dirty_rects = refined;
    }

    fn request_full_bitmap_updates(&mut self, count: u8) {
        self.needs_full_bitmap_seed = true;
        self.forced_full_bitmap_updates_remaining =
            self.forced_full_bitmap_updates_remaining.max(count.max(1));
    }

    fn cache_latest_frame(&mut self, frame: &CapturedFrame) {
        self.last_frame_cache = Some(match &frame.data {
            FrameData::Raw(bytes) => CachedFrame::Bgra {
                data: bytes.clone(),
                width: frame.width,
                height: frame.height,
                stride: frame.stride,
            },
            FrameData::BgraPixelBuffer(buf) => CachedFrame::BgraPixelBuffer(
                buf.clone_ref(),
                frame.width,
                frame.height,
                frame.stride,
            ),
            FrameData::PixelBuffer(buf) => {
                CachedFrame::PixelBuffer(buf.clone_ref(), frame.width, frame.height)
            }
        });
    }

    fn poll_cursor_update_if_due(&mut self) -> Option<DisplayUpdate> {
        // `show_cursor` controls whether ScreenCaptureKit burns the host cursor
        // into captured frames. When it is off, we need RDP pointer updates.
        if self.capture_config.show_cursor {
            self.last_cursor_fingerprint = None;
            return None;
        }

        let now = Instant::now();
        if now < self.next_cursor_poll_at {
            return None;
        }

        self.next_cursor_poll_at = now + CURSOR_POLL_INTERVAL;

        match capture_current_cursor_shape() {
            Ok(Some(shape)) => {
                let fingerprint = fingerprint_cursor_shape(&shape);
                if self.last_cursor_fingerprint == Some(fingerprint) {
                    return None;
                }

                self.last_cursor_fingerprint = Some(fingerprint);
                Some(DisplayUpdate::ColorPointer(cursor_shape_to_pointer(shape)))
            }
            Ok(None) => {
                if self.last_cursor_fingerprint.take().is_some() {
                    Some(DisplayUpdate::DefaultPointer)
                } else {
                    None
                }
            }
            Err(error) => {
                tracing::debug!(%error, "Failed to capture current cursor shape");
                None
            }
        }
    }

    fn cursor_poll_timeout(&self) -> Option<Duration> {
        if self.capture_config.show_cursor {
            return None;
        }

        self.next_cursor_poll_at
            .checked_duration_since(Instant::now())
            .or(Some(Duration::ZERO))
    }

    fn mark_client_may_be_stale(&mut self, reason: &'static str) {
        if !self.client_may_be_stale {
            tracing::debug!(reason, "Marking client state as potentially stale");
        }
        self.client_may_be_stale = true;
    }

    fn clear_client_stale_flag(&mut self, reason: &'static str) {
        if self.client_may_be_stale {
            tracing::debug!(reason, "Cleared client stale state");
        }
        self.client_may_be_stale = false;
    }

    fn pipeline_collect_timeout(&self) -> Duration {
        let fps = self.last_applied_fps.max(1) as f64;
        let frame_ms = 1000.0 / fps;
        Duration::from_millis((frame_ms * 0.5).clamp(4.0, 12.0).round() as u64)
    }

    fn fallback_pointer_update(&self) -> DisplayUpdate {
        if self.capture_config.show_cursor {
            DisplayUpdate::HidePointer
        } else {
            DisplayUpdate::DefaultPointer
        }
    }

    fn record_bitmap_update_metrics(&mut self, bytes_sent: u32) {
        if let Some(ps) = &self.perf_stats {
            ps.lock().unwrap().record_frame(0.0, bytes_sent, false);
        }
    }

    fn cached_full_frame_uncompressed_update(&self) -> Option<GfxUncompressedUpdate> {
        let cache = self.last_frame_cache.as_ref()?;
        let full_rect = ClampedRect {
            x: 0,
            y: 0,
            width: cache.width(),
            height: cache.height(),
        };
        let width = cache.width() as u16;
        let height = cache.height() as u16;

        let rects = match cache {
            CachedFrame::Bgra { data, stride, .. } => extract_bgra_rect(data, *stride, full_rect)
                .map(|pixel_data| {
                    vec![UncompressedRect {
                        x: 0,
                        y: 0,
                        width,
                        height,
                        pixel_data,
                    }]
                }),
            CachedFrame::BgraPixelBuffer(buf, width_u32, height_u32, _) => buf
                .with_locked_bgra(*width_u32, *height_u32, |bgra, stride| {
                    extract_bgra_rect(bgra, stride, full_rect).map(|pixel_data| {
                        vec![UncompressedRect {
                            x: 0,
                            y: 0,
                            width,
                            height,
                            pixel_data,
                        }]
                    })
                })
                .flatten(),
            CachedFrame::PixelBuffer(_, _, _) => None,
        }?;

        Some(GfxUncompressedUpdate {
            rects,
            width,
            height,
        })
    }

    fn cached_full_frame_bitmap_update(&self) -> Option<BitmapUpdate> {
        let cache = self.last_frame_cache.as_ref()?;
        let width = NonZeroU16::new(cache.width() as u16)?;
        let height = NonZeroU16::new(cache.height() as u16)?;

        match cache {
            CachedFrame::Bgra { data, stride, .. } => Some(BitmapUpdate {
                x: 0,
                y: 0,
                width,
                height,
                format: RdpPixelFormat::BgrA32,
                data: data.clone(),
                stride: NonZeroUsize::new(*stride)?,
                dirty_rects: Vec::new(),
            }),
            CachedFrame::BgraPixelBuffer(buf, width_u32, height_u32, _) => buf
                .with_locked_bgra(*width_u32, *height_u32, |bgra, stride| {
                    Some(BitmapUpdate {
                        x: 0,
                        y: 0,
                        width,
                        height,
                        format: RdpPixelFormat::BgrA32,
                        data: Bytes::copy_from_slice(bgra),
                        stride: NonZeroUsize::new(stride)?,
                        dirty_rects: Vec::new(),
                    })
                })
                .flatten(),
            CachedFrame::PixelBuffer(_, _, _) => None,
        }
    }

    fn send_cached_full_frame_recovery(&mut self) -> Option<DisplayUpdate> {
        let (session_uses_gfx, gfx_channel_ready, gfx_avc_ready) = {
            let state = self.gfx_state.lock().unwrap();
            let session_uses_gfx = state.session_uses_gfx && !state.bitmap_fallback_mode;
            let gfx_channel_ready = session_uses_gfx && state.has_gfx_channel();
            let gfx_avc_ready =
                gfx_channel_ready && state.avc420_supported && self.encoder.is_some();
            (session_uses_gfx, gfx_channel_ready, gfx_avc_ready)
        };

        if gfx_avc_ready {
            if let Some(update) = self.send_idle_idr() {
                self.clear_client_stale_flag("cached-avc-convergence");
                return Some(update);
            }
        }

        if gfx_channel_ready {
            if let Some(update) = self.cached_full_frame_uncompressed_update() {
                let total_bytes: usize =
                    update.rects.iter().map(|rect| rect.pixel_data.len()).sum();
                self.display_frame_count += 1;
                {
                    let mut st = self.gfx_state.lock().unwrap();
                    st.last_encode_ms = 0.0;
                    st.last_frame_bytes = total_bytes as u32;
                }
                self.record_uncompressed_audit(
                    &update.rects,
                    update.width as u32 * update.height as u32,
                    "gfx-cached-full-frame-recovery",
                );
                self.last_uncompressed_rects = Some(vec![ClampedRect {
                    x: 0,
                    y: 0,
                    width: update.width as u32,
                    height: update.height as u32,
                }]);
                self.last_uncompressed_sent = Some(Instant::now());
                self.last_full_frame_sent = self.last_uncompressed_sent;
                self.clear_client_stale_flag("cached-uncompressed-convergence");
                return Some(DisplayUpdate::GfxUncompressed(update));
            }
        } else if !session_uses_gfx {
            if let Some(bitmap) = self.cached_full_frame_bitmap_update() {
                self.record_bitmap_update_metrics(bitmap.data.len() as u32);
                self.record_bitmap_audit(&bitmap, "bitmap-cached-full-frame-recovery");
                self.last_full_bitmap_sent = Some(Instant::now());
                self.needs_full_bitmap_seed = false;
                self.clear_client_stale_flag("cached-bitmap-convergence");
                return Some(DisplayUpdate::Bitmap(bitmap));
            }
        }

        None
    }

    fn record_uncompressed_audit(
        &mut self,
        rects: &[UncompressedRect],
        dirty_pixels: u32,
        detail: &'static str,
    ) {
        let total_bytes = rects.iter().map(|rect| rect.pixel_data.len() as u32).sum();
        self.session_audit.record(
            SessionPathKind::GfxUncompressed,
            total_bytes,
            None,
            rects.len(),
            dirty_pixels,
            detail,
        );
    }

    fn record_avc_audit(
        &mut self,
        bytes_sent: u32,
        encode_ms: f64,
        dirty_rects: usize,
        dirty_pixels: u32,
        detail: &'static str,
    ) {
        self.session_audit.record(
            SessionPathKind::GfxAvc,
            bytes_sent,
            Some(encode_ms),
            dirty_rects,
            dirty_pixels,
            detail,
        );
    }

    fn record_bitmap_audit(&mut self, bitmap: &BitmapUpdate, detail: &'static str) {
        let dirty_pixels = if bitmap.dirty_rects.is_empty() {
            bitmap.width.get() as u32 * bitmap.height.get() as u32
        } else {
            bitmap
                .dirty_rects
                .iter()
                .map(|rect| (rect.width * rect.height) as u32)
                .sum()
        };
        self.session_audit.record(
            SessionPathKind::Bitmap,
            bitmap.data.len() as u32,
            None,
            bitmap.dirty_rects.len(),
            dirty_pixels,
            detail,
        );
    }

    fn suppress_duplicate_bitmap_updates(
        &mut self,
        updates: Vec<BitmapUpdate>,
    ) -> Vec<BitmapUpdate> {
        // Guacamole repaint correctness is more important than shaving off a
        // few duplicate bitmap bursts. Even exact-payload suppression has
        // proven unsafe in live tests because repeated damage can still be
        // semantically necessary to converge the visible screen state.
        updates
    }

    fn desired_capture_pixel_format(&self) -> CapturePixelFormat {
        let gfx = self.gfx_state.lock().unwrap();
        if should_use_nv12_capture_for_gfx(
            gfx.session_uses_gfx,
            gfx.is_ready(),
            gfx.avc420_supported,
            gfx.last_ack_frame,
            gfx.gfx_validation_mode,
            gfx.avc_fallback_to_uncompressed,
            gfx.bitmap_fallback_mode,
            self.encoder_pref,
            self.mode_444,
            self.needs_full_gfx_seed,
        ) || (!gfx.session_uses_gfx
            && gfx.is_ready()
            && self.encoder_pref == macrdp_encode::EncoderPreference::Hardware
            && !self.mode_444
            && !self.needs_full_gfx_seed)
        {
            CapturePixelFormat::Nv12
        } else {
            CapturePixelFormat::Bgra
        }
    }

    fn apply_capture_fps(&mut self, requested_fps: u32, reason: &'static str) {
        let target_fps = requested_fps.max(1);
        if target_fps == self.last_applied_fps {
            return;
        }

        if let Err(e) = self.capturer.set_frame_rate(target_fps) {
            tracing::warn!("Failed to update frame rate: {e}");
            return;
        }

        self.last_applied_fps = target_fps;
        self.capture_config.frame_rate = target_fps;
        self.gfx_state.lock().unwrap().applied_frame_rate = Some(target_fps);
        tracing::info!(
            requested_fps,
            applied_fps = target_fps,
            reason,
            "Capture frame rate updated"
        );
    }

    fn sync_runtime_frame_rate_override(&mut self) {
        let configured_fps = self
            .gfx_state
            .lock()
            .unwrap()
            .target_frame_rate
            .unwrap_or(self.bitrate_ctrl.target_fps())
            .max(1);

        if configured_fps == self.bitrate_ctrl.target_fps() {
            return;
        }

        self.bitrate_ctrl.set_target_fps(configured_fps);
        self.apply_capture_fps(configured_fps, "config-hot-update");
    }

    async fn refresh_capture_transport_if_needed(&mut self) -> Result<()> {
        let desired_format = self.desired_capture_pixel_format();
        if self.capture_config.pixel_format == desired_format {
            return Ok(());
        }

        tracing::info!(
            old_format = ?self.capture_config.pixel_format,
            new_format = ?desired_format,
            "Refreshing capture transport for negotiated graphics mode"
        );

        let mut new_config = self.capture_config.clone();
        new_config.pixel_format = desired_format;
        let audio_tx = self
            .shared_audio_tx
            .as_ref()
            .and_then(|shared| shared.lock().unwrap().clone());
        let new_capturer = ScreenCapturer::new(new_config.clone(), audio_tx).await?;

        self.capturer = new_capturer;
        self.capture_config = new_config;
        self.has_pending_submit = false;
        self.pending_visible_size = None;
        self.pending_frame_time = None;
        self.last_frame_cache = None;
        self.last_uncompressed_rects = None;
        self.last_uncompressed_sent = None;
        self.last_full_frame_sent = None;
        self.last_large_uncompressed_fingerprint = None;
        self.last_large_uncompressed_sent = None;
        self.last_large_uncompressed_rects = None;
        self.idle_frame_count = 0;
        self.skip_next_frame = false;
        self.request_full_bitmap_updates(1);
        self.needs_full_gfx_seed = true;

        if let Some(enc) = &mut self.encoder {
            enc.force_keyframe();
            enc.set_bitrate(self.bitrate_ctrl.current_bitrate());
        }

        Ok(())
    }

    /// Re-encode the cached last frame as an IDR keyframe (keepalive during idle).
    /// Returns `None` if no cached frame or GFX is not ready.
    fn send_idle_idr(&mut self) -> Option<DisplayUpdate> {
        let cache = self.last_frame_cache.as_ref()?;
        let encoder = self.encoder.as_mut()?;

        // Check GFX readiness
        {
            let state = self.gfx_state.lock().unwrap();
            if !state.is_ready() {
                return None;
            }
        }

        let t0 = Instant::now();
        let result = match cache {
            CachedFrame::Bgra {
                data,
                width,
                height,
                stride,
            } => {
                encoder.force_keyframe();
                encoder.encode_bgra(data, *width, *height, *stride)
            }
            CachedFrame::BgraPixelBuffer(buf, width, height, _stride) => {
                encoder.force_keyframe();
                match buf.with_locked_bgra(*width, *height, |bgra, stride| {
                    encoder.encode_bgra(bgra, *width, *height, stride)
                }) {
                    Some(result) => result,
                    None => {
                        tracing::warn!("Idle IDR BGRA pixel buffer lock failed");
                        return None;
                    }
                }
            }
            CachedFrame::PixelBuffer(buf, _w, _h) => {
                encoder.encode_pixel_buffer(buf.as_ptr(), true)
            }
        };

        match result {
            Ok(encoded) if !encoded.data.is_empty() => {
                let encode_ms = t0.elapsed().as_secs_f64() * 1000.0;
                tracing::debug!(
                    h264_bytes = encoded.data.len(),
                    encode_ms = format!("{:.1}", encode_ms),
                    idle_frames = self.idle_frame_count,
                    "Sent idle IDR keepalive"
                );
                self.last_idr_time = Instant::now();
                let (vis_w, vis_h) = match cache {
                    CachedFrame::Bgra { width, height, .. } => (*width as u16, *height as u16),
                    CachedFrame::BgraPixelBuffer(_, w, h, _) => (*w as u16, *h as u16),
                    CachedFrame::PixelBuffer(_, w, h) => (*w as u16, *h as u16),
                };
                Some(DisplayUpdate::GfxFrame(GfxFrameUpdate {
                    h264_data: encoded.data,
                    width: vis_w,
                    height: vis_h,
                    enc_width: encoded.width as u16,
                    enc_height: encoded.height as u16,
                    is_keyframe: true,
                    h264_aux: None,
                }))
            }
            Ok(_) => {
                tracing::warn!("Idle IDR encode returned empty data");
                None
            }
            Err(e) => {
                tracing::warn!("Idle IDR encode failed: {e}");
                None
            }
        }
    }

    /// Evaluate the BitrateController window and apply any adaptive bitrate/fps changes.
    fn apply_adaptive_decision(&mut self) {
        if !self.bitrate_ctrl.should_evaluate() {
            return;
        }

        // Phase 2 RTT check
        {
            let gfx = self.gfx_state.lock().unwrap();
            if gfx.rtt_ewma_ms > 0.0 {
                self.bitrate_ctrl.update_network_rtt(gfx.rtt_ewma_ms);
                if let Some(ps) = &self.perf_stats {
                    ps.lock().unwrap().record_rtt(gfx.rtt_ewma_ms);
                }
            }
        }

        let decision = self.bitrate_ctrl.evaluate();

        if let Some(ref mut enc) = self.encoder {
            enc.set_bitrate(decision.bitrate_bps);
        }

        if decision.fps != self.last_applied_fps {
            self.apply_capture_fps(decision.fps, "adaptive");
            tracing::info!(
                requested_fps = decision.fps,
                applied_fps = decision.fps,
                bitrate_mbps = decision.bitrate_bps as f64 / 1_000_000.0,
                "Adaptive: fps/bitrate updated"
            );
        }
    }

    /// Collect a previously submitted async (pipelined) encode result and build a GFX DisplayUpdate.
    fn collect_and_send_pending(&mut self) -> Result<Option<DisplayUpdate>> {
        let timeout = self.pipeline_collect_timeout();
        let encoder = match &mut self.encoder {
            Some(e) if e.supports_pipelining() => e,
            _ => return Ok(None),
        };

        match encoder.collect_encoded(timeout)? {
            Some(encoded) if !encoded.data.is_empty() => {
                let encode_ms = self
                    .pending_frame_time
                    .map(|t| t.elapsed().as_secs_f64() * 1000.0)
                    .unwrap_or(0.0);
                let is_keyframe = encoded.is_keyframe;

                tracing::debug!(
                    h264_bytes = encoded.data.len(),
                    is_keyframe,
                    encode_ms = format!("{:.1}", encode_ms),
                    "Display: collected pipelined GFX frame"
                );
                {
                    let mut st = self.gfx_state.lock().unwrap();
                    st.last_encode_ms = encode_ms;
                    st.last_frame_bytes = encoded.data.len() as u32;
                }
                self.bitrate_ctrl
                    .record_frame(crate::bitrate_controller::FrameStats {
                        encode_ms,
                        frame_bytes: encoded.data.len() as u32,
                        is_keyframe,
                    });
                if let Some(ps) = &self.perf_stats {
                    ps.lock().unwrap().record_frame(
                        encode_ms,
                        encoded.data.len() as u32,
                        is_keyframe,
                    );
                }
                self.record_avc_audit(
                    encoded.data.len() as u32,
                    encode_ms,
                    0,
                    0,
                    "gfx-avc-pipelined-collect",
                );
                self.apply_adaptive_decision();

                if is_keyframe {
                    self.last_idr_time = Instant::now();
                }
                self.needs_full_gfx_seed = false;

                self.has_pending_submit = false;
                let (frame_width, frame_height) = self
                    .pending_visible_size
                    .take()
                    .unwrap_or((encoded.width as u16, encoded.height as u16));
                self.pending_frame_time = None;
                self.clear_client_stale_flag("gfx-avc-pipelined-collect");

                Ok(Some(DisplayUpdate::GfxFrame(GfxFrameUpdate {
                    h264_data: encoded.data,
                    width: frame_width,
                    height: frame_height,
                    enc_width: encoded.width as u16,
                    enc_height: encoded.height as u16,
                    is_keyframe,
                    h264_aux: None,
                })))
            }
            Some(_) => {
                self.has_pending_submit = false;
                self.pending_visible_size = None;
                self.pending_frame_time = None;
                self.mark_client_may_be_stale("pipelined-empty-encode");
                Ok(None)
            }
            None => {
                tracing::warn!("Pipelined collect timed out — frame lost");
                self.has_pending_submit = false;
                self.pending_visible_size = None;
                self.pending_frame_time = None;
                self.mark_client_may_be_stale("pipelined-collect-timeout");
                Ok(None)
            }
        }
    }

    fn encode_and_send(&mut self, frame: CapturedFrame) -> Result<Option<DisplayUpdate>> {
        self.coord_mapper.update_frame_content_rect(
            frame.width,
            frame.height,
            frame.content_rect.as_ref(),
        );
        self.cache_latest_frame(&frame);

        // Encode overload protection: skip this frame if previous encode took too long
        if self.skip_next_frame {
            self.skip_next_frame = false;
            self.mark_client_may_be_stale("encode-overload-skip");
            return Ok(None);
        }

        // Check GFX state and AVC444 negotiation
        let (
            session_uses_gfx,
            gfx_channel_ready,
            gfx_avc_ready,
            use_444,
            force_full_frame_uncompressed_gfx,
            request_bitmap_fallback_full_frames,
        ) = {
            let mut state = self.gfx_state.lock().unwrap();
            let session_uses_gfx = state.session_uses_gfx && !state.bitmap_fallback_mode;
            let gfx_channel_ready = session_uses_gfx && state.has_gfx_channel();
            let gfx_avc_ready =
                gfx_channel_ready && state.avc420_supported && self.encoder.is_some();
            let use_444 = self.mode_444 && state.avc444_supported && state.avc444_enabled;
            let mut request_bitmap_fallback_full_frames = false;
            let oldest_pending_age = state
                .frame_send_times
                .values()
                .min()
                .map(|sent_at| sent_at.elapsed());
            if gfx_avc_ready
                && !state.gfx_validation_mode
                && !state.bitmap_fallback_mode
                && should_fallback_from_gfx_avc_no_ack(
                    state.frame_id,
                    state.last_ack_frame,
                    state.pending_acks,
                    oldest_pending_age,
                )
            {
                let oldest_pending_ms = oldest_pending_age
                    .map(|age| age.as_millis() as u64)
                    .unwrap_or(0);
                let sent_avc_frames = state.frame_id;
                let pending_acks = state.pending_acks;
                let last_ack_frame = state.last_ack_frame;
                state.bitmap_fallback_mode = true;
                state.avc_fallback_to_uncompressed = false;
                state.surface_created = false;
                state.pending_acks = 0;
                state.last_ack_frame = 0;
                state.frame_send_times.clear();
                self.has_pending_submit = false;
                self.needs_full_gfx_seed = false;
                request_bitmap_fallback_full_frames = true;
                self.logged_gfx_avc_mode = false;
                self.logged_gfx_uncompressed_mode = false;
                self.logged_bitmap_mode = false;
                self.session_audit
                    .note_fallback_reason("avc-no-ack-bitmap-fallback");
                tracing::warn!(
                    sent_avc_frames,
                    pending_acks,
                    last_ack_frame,
                    oldest_pending_ms,
                    "GFX AVC negotiated but client never acknowledged a frame; abandoning GFX and falling back to classic bitmap updates for this session"
                );
            }
            let force_full_frame_uncompressed_gfx = state.gfx_validation_mode
                || state.avc_fallback_to_uncompressed
                || DEFAULT_FORCE_FULL_FRAME_UNCOMPRESSED_GFX;
            (
                session_uses_gfx,
                gfx_channel_ready,
                gfx_avc_ready,
                use_444,
                force_full_frame_uncompressed_gfx,
                request_bitmap_fallback_full_frames,
            )
        };
        if request_bitmap_fallback_full_frames {
            self.request_full_bitmap_updates(BITMAP_FORCED_FULL_UPDATES_AFTER_GFX_FALLBACK);
        }
        self.sync_runtime_frame_rate_override();

        if gfx_channel_ready {
            if self.needs_full_gfx_seed {
                let full_rect = ClampedRect {
                    x: 0,
                    y: 0,
                    width: frame.width,
                    height: frame.height,
                };
                let rects: Vec<UncompressedRect> = match &frame.data {
                    FrameData::Raw(bgra) => extract_bgra_rect(bgra, frame.stride, full_rect)
                        .map(|pixel_data| {
                            vec![UncompressedRect {
                                x: 0,
                                y: 0,
                                width: frame.width as u16,
                                height: frame.height as u16,
                                pixel_data,
                            }]
                        })
                        .unwrap_or_default(),
                    FrameData::BgraPixelBuffer(buf) => buf
                        .with_locked_bgra(frame.width, frame.height, |bgra, stride| {
                            extract_bgra_rect(bgra, stride, full_rect).map(|pixel_data| {
                                vec![UncompressedRect {
                                    x: 0,
                                    y: 0,
                                    width: frame.width as u16,
                                    height: frame.height as u16,
                                    pixel_data,
                                }]
                            })
                        })
                        .flatten()
                        .unwrap_or_default(),
                    FrameData::PixelBuffer(_) => Vec::new(),
                };

                if !rects.is_empty() {
                    if !self.logged_gfx_uncompressed_mode {
                        tracing::info!(
                            mode = "gfx-uncompressed-full-frame-seed",
                            pixel_format = ?self.capture_config.pixel_format,
                            encoder_pref = ?self.encoder_pref,
                            session_uses_gfx,
                            gfx_avc_ready,
                            "Negotiated graphics transport active"
                        );
                        self.logged_gfx_uncompressed_mode = true;
                    }
                    self.display_frame_count += 1;
                    self.needs_full_gfx_seed = false;
                    let total_bytes: usize = rects.iter().map(|r| r.pixel_data.len()).sum();
                    {
                        let mut st = self.gfx_state.lock().unwrap();
                        st.last_encode_ms = 0.0;
                        st.last_frame_bytes = total_bytes as u32;
                    }
                    tracing::debug!(
                        display_frame = self.display_frame_count,
                        total_bytes,
                        "Display: sending initial full-frame GFX seed"
                    );
                    self.record_uncompressed_audit(
                        &rects,
                        frame.width as u32 * frame.height as u32,
                        "gfx-seed-full-frame",
                    );
                    self.clear_client_stale_flag("gfx-seed-full-frame");
                    return Ok(Some(DisplayUpdate::GfxUncompressed(
                        GfxUncompressedUpdate {
                            rects,
                            width: frame.width as u16,
                            height: frame.height as u16,
                        },
                    )));
                }
            }

            // Validation mode for Guacamole GFX compatibility: after the initial
            // seed, keep the whole negotiated GFX session on uncompressed
            // updates so we do not silently fall back into AVC.
            if force_full_frame_uncompressed_gfx {
                let total_area: u32 = frame.dirty_rects.iter().map(|r| r.width * r.height).sum();
                let use_dirty_rects = !frame.dirty_rects.is_empty()
                    && total_area > 0
                    && total_area <= UNCOMPRESSED_MAX_PIXELS
                    && !self.client_may_be_stale;
                let rect_signature: Vec<ClampedRect>;

                let rects: Vec<UncompressedRect> = if use_dirty_rects {
                    let prepared_rects = prepare_uncompressed_dirty_rects(
                        frame.width,
                        frame.height,
                        &frame.dirty_rects,
                    );
                    rect_signature = prepared_rects.clone();
                    match &frame.data {
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
                    }
                } else {
                    rect_signature = vec![ClampedRect {
                        x: 0,
                        y: 0,
                        width: frame.width,
                        height: frame.height,
                    }];

                    if !frame.dirty_rects.is_empty()
                        && self
                            .last_full_frame_sent
                            .is_some_and(|t| t.elapsed() < UNCOMPRESSED_FULL_FRAME_MIN_INTERVAL)
                    {
                        tracing::debug!(
                            total_pixels = total_area,
                            dirty_rects = frame.dirty_rects.len(),
                            "Skipping eager full-frame uncompressed fallback"
                        );
                        self.mark_client_may_be_stale("full-frame-uncompressed-throttled");
                        return Ok(None);
                    }

                    let full_rect = ClampedRect {
                        x: 0,
                        y: 0,
                        width: frame.width,
                        height: frame.height,
                    };
                    match &frame.data {
                        FrameData::Raw(bgra) => extract_bgra_rect(bgra, frame.stride, full_rect)
                            .map(|pixel_data| {
                                vec![UncompressedRect {
                                    x: 0,
                                    y: 0,
                                    width: frame.width as u16,
                                    height: frame.height as u16,
                                    pixel_data,
                                }]
                            })
                            .unwrap_or_default(),
                        FrameData::BgraPixelBuffer(buf) => buf
                            .with_locked_bgra(frame.width, frame.height, |bgra, stride| {
                                extract_bgra_rect(bgra, stride, full_rect).map(|pixel_data| {
                                    vec![UncompressedRect {
                                        x: 0,
                                        y: 0,
                                        width: frame.width as u16,
                                        height: frame.height as u16,
                                        pixel_data,
                                    }]
                                })
                            })
                            .flatten()
                            .unwrap_or_default(),
                        FrameData::PixelBuffer(_) => Vec::new(),
                    }
                };

                if !rects.is_empty() {
                    let total_bytes: usize = rects.iter().map(|r| r.pixel_data.len()).sum();
                    let large_payload_fingerprint = (total_bytes
                        >= UNCOMPRESSED_LARGE_UPDATE_BYTES)
                        .then(|| fingerprint_uncompressed_rects(&rects));

                    if self
                        .last_uncompressed_sent
                        .is_some_and(|t| t.elapsed() < UNCOMPRESSED_DUPLICATE_MIN_INTERVAL)
                        && self
                            .last_uncompressed_rects
                            .as_ref()
                            .is_some_and(|prev| prev == &rect_signature)
                    {
                        tracing::debug!(
                            rects = rect_signature.len(),
                            update_kind = if use_dirty_rects {
                                "dirty-rects"
                            } else {
                                "full-frame"
                            },
                            "Suppressing duplicate uncompressed GFX update burst"
                        );
                        self.mark_client_may_be_stale("duplicate-uncompressed-burst");
                        return Ok(None);
                    }

                    if large_payload_fingerprint.is_some()
                        && self
                            .last_large_uncompressed_rects
                            .as_ref()
                            .is_some_and(|prev| prev == &rect_signature)
                        && self.last_large_uncompressed_sent.is_some_and(|t| {
                            t.elapsed() < UNCOMPRESSED_LARGE_GEOMETRY_REPEAT_MIN_INTERVAL
                        })
                    {
                        tracing::debug!(
                            rects = rect_signature.len(),
                            total_bytes,
                            fingerprint = ?large_payload_fingerprint,
                            update_kind = if use_dirty_rects { "dirty-rects" } else { "full-frame" },
                            "Suppressing repeated large uncompressed GFX update with same geometry"
                        );
                        self.mark_client_may_be_stale("large-uncompressed-geometry-repeat");
                        return Ok(None);
                    }

                    if large_payload_fingerprint.is_some()
                        && self
                            .last_large_uncompressed_sent
                            .is_some_and(|t| t.elapsed() < UNCOMPRESSED_LARGE_REPEAT_MIN_INTERVAL)
                        && self.last_large_uncompressed_fingerprint == large_payload_fingerprint
                    {
                        tracing::debug!(
                            rects = rect_signature.len(),
                            total_bytes,
                            fingerprint = ?large_payload_fingerprint,
                            update_kind = if use_dirty_rects { "dirty-rects" } else { "full-frame" },
                            "Suppressing repeated large uncompressed GFX update with identical payload"
                        );
                        self.mark_client_may_be_stale("large-uncompressed-payload-repeat");
                        return Ok(None);
                    }

                    if !self.logged_gfx_uncompressed_mode {
                        tracing::info!(
                            mode = "gfx-uncompressed-only",
                            pixel_format = ?self.capture_config.pixel_format,
                            encoder_pref = ?self.encoder_pref,
                            session_uses_gfx,
                            gfx_avc_ready,
                            "Negotiated graphics transport active"
                        );
                        self.logged_gfx_uncompressed_mode = true;
                    }
                    self.display_frame_count += 1;
                    self.last_uncompressed_rects = Some(rect_signature.clone());
                    self.last_uncompressed_sent = Some(Instant::now());
                    if !use_dirty_rects {
                        self.last_full_frame_sent = self.last_uncompressed_sent;
                    }
                    if let Some(fingerprint) = large_payload_fingerprint {
                        self.last_large_uncompressed_fingerprint = Some(fingerprint);
                        self.last_large_uncompressed_sent = self.last_uncompressed_sent;
                        self.last_large_uncompressed_rects = Some(rect_signature.clone());
                    } else {
                        self.last_large_uncompressed_fingerprint = None;
                        self.last_large_uncompressed_sent = None;
                        self.last_large_uncompressed_rects = None;
                    }
                    {
                        let mut st = self.gfx_state.lock().unwrap();
                        st.last_encode_ms = 0.0;
                        st.last_frame_bytes = total_bytes as u32;
                    }
                    tracing::debug!(
                        display_frame = self.display_frame_count,
                        rects = rects.len(),
                        total_pixels = total_area,
                        total_bytes,
                        update_kind = if use_dirty_rects {
                            "dirty-rects"
                        } else {
                            "full-frame"
                        },
                        "Display: sending uncompressed GFX update (validation mode)"
                    );
                    self.record_uncompressed_audit(
                        &rects,
                        total_area,
                        if use_dirty_rects {
                            "gfx-validation-dirty-rects"
                        } else {
                            "gfx-validation-full-frame"
                        },
                    );
                    if !use_dirty_rects {
                        self.clear_client_stale_flag("gfx-validation-full-frame");
                    }
                    return Ok(Some(DisplayUpdate::GfxUncompressed(
                        GfxUncompressedUpdate {
                            rects,
                            width: frame.width as u16,
                            height: frame.height as u16,
                        },
                    )));
                }
            }

            // GFX uncompressed path — keep only tiny UI/cursor rects on this
            // immediate path once AVC is available. Larger dirty regions should
            // prefer AVC so the negotiated hardware/software encoder actually
            // gets used for meaningful screen content updates.
            if !force_full_frame_uncompressed_gfx && !frame.dirty_rects.is_empty() {
                let total_area: u32 = frame.dirty_rects.iter().map(|r| r.width * r.height).sum();

                let allow_uncompressed_dirty_rects = if gfx_avc_ready {
                    total_area > 0
                        && total_area <= UNCOMPRESSED_TINY_UI_MAX_PIXELS
                        && frame.dirty_rects.len() <= 2
                        && !self.client_may_be_stale
                } else {
                    total_area > 0
                        && total_area <= UNCOMPRESSED_MAX_PIXELS
                        && !self.client_may_be_stale
                };

                if allow_uncompressed_dirty_rects {
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
                        if !self.logged_gfx_uncompressed_mode {
                            tracing::info!(
                                mode = "gfx-uncompressed-dirty-rects",
                                pixel_format = ?self.capture_config.pixel_format,
                                encoder_pref = ?self.encoder_pref,
                                session_uses_gfx,
                                gfx_avc_ready,
                                "Negotiated graphics transport active"
                            );
                            self.logged_gfx_uncompressed_mode = true;
                        }
                        self.display_frame_count += 1;
                        let total_bytes: usize = rects.iter().map(|r| r.pixel_data.len()).sum();
                        {
                            let mut st = self.gfx_state.lock().unwrap();
                            st.last_encode_ms = 0.0;
                            st.last_frame_bytes = total_bytes as u32;
                        }
                        tracing::debug!(
                            display_frame = self.display_frame_count,
                            rects = rects.len(),
                            total_pixels = total_area,
                            total_bytes,
                            "Display: sending GFX uncompressed dirty rects"
                        );
                        self.record_uncompressed_audit(
                            &rects,
                            total_area,
                            "gfx-uncompressed-dirty-rects",
                        );
                        return Ok(Some(DisplayUpdate::GfxUncompressed(
                            GfxUncompressedUpdate {
                                rects,
                                width: frame.width as u16,
                                height: frame.height as u16,
                            },
                        )));
                    }
                }
            }

            // If the GFX channel exists but AVC/H.264 is not negotiated, stay on the
            // GFX path by sending a full uncompressed frame instead of dropping back to
            // classic bitmap updates.
            if !gfx_avc_ready {
                let full_rect = ClampedRect {
                    x: 0,
                    y: 0,
                    width: frame.width,
                    height: frame.height,
                };
                let rects: Vec<UncompressedRect> = match &frame.data {
                    FrameData::Raw(bgra) => extract_bgra_rect(bgra, frame.stride, full_rect)
                        .map(|pixel_data| {
                            vec![UncompressedRect {
                                x: 0,
                                y: 0,
                                width: frame.width as u16,
                                height: frame.height as u16,
                                pixel_data,
                            }]
                        })
                        .unwrap_or_default(),
                    FrameData::BgraPixelBuffer(buf) => buf
                        .with_locked_bgra(frame.width, frame.height, |bgra, stride| {
                            extract_bgra_rect(bgra, stride, full_rect).map(|pixel_data| {
                                vec![UncompressedRect {
                                    x: 0,
                                    y: 0,
                                    width: frame.width as u16,
                                    height: frame.height as u16,
                                    pixel_data,
                                }]
                            })
                        })
                        .flatten()
                        .unwrap_or_default(),
                    FrameData::PixelBuffer(_) => Vec::new(),
                };

                if !rects.is_empty() {
                    if !self.logged_gfx_uncompressed_mode {
                        tracing::info!(
                            mode = "gfx-uncompressed-full-frame",
                            pixel_format = ?self.capture_config.pixel_format,
                            encoder_pref = ?self.encoder_pref,
                            session_uses_gfx,
                            gfx_avc_ready,
                            "Negotiated graphics transport active"
                        );
                        self.logged_gfx_uncompressed_mode = true;
                    }
                    self.display_frame_count += 1;
                    let total_bytes: usize = rects.iter().map(|r| r.pixel_data.len()).sum();
                    {
                        let mut st = self.gfx_state.lock().unwrap();
                        st.last_encode_ms = 0.0;
                        st.last_frame_bytes = total_bytes as u32;
                    }
                    tracing::debug!(
                        display_frame = self.display_frame_count,
                        total_bytes,
                        "Display: sending full-frame uncompressed GFX update"
                    );
                    self.record_uncompressed_audit(
                        &rects,
                        frame.width as u32 * frame.height as u32,
                        "gfx-uncompressed-full-frame",
                    );
                    self.clear_client_stale_flag("gfx-uncompressed-full-frame");
                    return Ok(Some(DisplayUpdate::GfxUncompressed(
                        GfxUncompressedUpdate {
                            rects,
                            width: frame.width as u16,
                            height: frame.height as u16,
                        },
                    )));
                }
            }

            // ── Pipelining fast-path (checked BEFORE borrowing encoder long-term) ──
            // Check pipelining support with a short borrow, then handle collect/submit
            // using &mut self methods that don't conflict with the encoder borrow.
            let supports_pipeline = self
                .encoder
                .as_ref()
                .map_or(false, |e| e.supports_pipelining());

            if gfx_avc_ready && supports_pipeline {
                match &frame.data {
                    FrameData::PixelBuffer(buf) => {
                        // Pipeline mode: collect previous + submit current
                        let prev_update = if self.has_pending_submit {
                            self.collect_and_send_pending()?
                        } else {
                            None
                        };

                        let t0_pipe = Instant::now();
                        self.encoder
                            .as_mut()
                            .unwrap()
                            .submit_pixel_buffer(buf.as_ptr())?;
                        self.has_pending_submit = true;
                        self.pending_visible_size = Some((frame.width as u16, frame.height as u16));
                        self.pending_frame_time = Some(t0_pipe);
                        self.display_frame_count += 1;

                        // Cache for idle IDR
                        self.last_frame_cache = Some(CachedFrame::PixelBuffer(
                            buf.clone_ref(),
                            frame.width,
                            frame.height,
                        ));

                        // Return the previous frame's result; the first pipelined submit
                        // intentionally emits nothing until a real frame is ready.
                        return Ok(prev_update);
                    }
                    FrameData::BgraPixelBuffer(buf) => {
                        if !use_444 || !self.encoder.as_ref().unwrap().supports_444() {
                            let prev_update = if self.has_pending_submit {
                                self.collect_and_send_pending()?
                            } else {
                                None
                            };

                            let t0_pipe = Instant::now();
                            let submit_ok = buf
                                .with_locked_bgra(frame.width, frame.height, |bgra, stride| {
                                    self.encoder.as_mut().unwrap().submit_bgra(
                                        bgra,
                                        frame.width,
                                        frame.height,
                                        stride,
                                    )
                                })
                                .transpose()?;
                            if submit_ok.is_none() {
                                tracing::warn!(
                                    "BGRA pixel buffer lock failed during pipeline submit"
                                );
                                return Ok(Some(self.fallback_pointer_update()));
                            }

                            self.has_pending_submit = true;
                            self.pending_visible_size =
                                Some((frame.width as u16, frame.height as u16));
                            self.pending_frame_time = Some(t0_pipe);
                            self.display_frame_count += 1;

                            self.last_frame_cache = Some(CachedFrame::BgraPixelBuffer(
                                buf.clone_ref(),
                                frame.width,
                                frame.height,
                                frame.stride,
                            ));

                            return Ok(prev_update);
                        }
                    }
                    FrameData::Raw(bytes) => {
                        // AVC444 stays synchronous — only pipeline AVC420.
                        // If use_444, fall through to the synchronous block below.
                        if !use_444 || !self.encoder.as_ref().unwrap().supports_444() {
                            let bgra = frame.data.as_bgra_bytes().unwrap();

                            // AVC420 pipeline: collect previous + submit current
                            let prev_update = if self.has_pending_submit {
                                self.collect_and_send_pending()?
                            } else {
                                None
                            };

                            let t0_pipe = Instant::now();
                            self.encoder.as_mut().unwrap().submit_bgra(
                                bgra,
                                frame.width,
                                frame.height,
                                frame.stride,
                            )?;
                            self.has_pending_submit = true;
                            self.pending_visible_size =
                                Some((frame.width as u16, frame.height as u16));
                            self.pending_frame_time = Some(t0_pipe);
                            self.display_frame_count += 1;

                            self.last_frame_cache = Some(CachedFrame::Bgra {
                                data: bytes.clone(),
                                width: frame.width,
                                height: frame.height,
                                stride: frame.stride,
                            });

                            return Ok(prev_update);
                        }
                        // else: AVC444 requested — fall through to synchronous block
                    }
                }
            }

            // ── Synchronous path (original code, also fallback when !supports_pipeline) ──
            if gfx_avc_ready {
                if !self.logged_gfx_avc_mode {
                    tracing::info!(
                        mode = if use_444 { "gfx-avc444" } else { "gfx-avc420" },
                        pixel_format = ?self.capture_config.pixel_format,
                        encoder_pref = ?self.encoder_pref,
                        session_uses_gfx,
                        gfx_avc_ready,
                        "Negotiated graphics transport active"
                    );
                    self.logged_gfx_avc_mode = true;
                }
                let Some(encoder) = &mut self.encoder else {
                    return Ok(Some(self.fallback_pointer_update()));
                };
                self.display_frame_count += 1;
                let t0 = std::time::Instant::now();

                // Route based on frame data type
                match &frame.data {
                    FrameData::PixelBuffer(buf) => {
                        // Zero-copy VT path — synchronous encode
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
                                self.bitrate_ctrl.record_frame(FrameStats {
                                    encode_ms,
                                    frame_bytes: encoded.data.len() as u32,
                                    is_keyframe,
                                });
                                if let Some(ps) = &self.perf_stats {
                                    ps.lock().unwrap().record_frame(
                                        encode_ms,
                                        encoded.data.len() as u32,
                                        is_keyframe,
                                    );
                                }
                                self.record_avc_audit(
                                    encoded.data.len() as u32,
                                    encode_ms,
                                    frame.dirty_rects.len(),
                                    frame
                                        .dirty_rects
                                        .iter()
                                        .map(|rect| rect.width * rect.height)
                                        .sum(),
                                    "gfx-avc-zero-copy",
                                );
                                self.apply_adaptive_decision();
                                let frame_interval_ms =
                                    1000.0 / self.capture_config.frame_rate as f64;
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
                                // Cache frame for idle IDR re-encoding
                                self.last_frame_cache = Some(CachedFrame::PixelBuffer(
                                    buf.clone_ref(),
                                    frame.width,
                                    frame.height,
                                ));
                                if is_keyframe {
                                    self.last_idr_time = Instant::now();
                                }
                                self.clear_client_stale_flag("gfx-avc-zero-copy");
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
                                self.mark_client_may_be_stale("zero-copy-empty-encode");
                                return Ok(None);
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Zero-copy encode failed: {e}, falling back to pointer policy"
                                );
                                return Ok(Some(self.fallback_pointer_update()));
                            }
                        }
                    }
                    FrameData::BgraPixelBuffer(buf) => {
                        let encoded = match buf.with_locked_bgra(
                            frame.width,
                            frame.height,
                            |bgra, stride| {
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
                                                "AVC444 encode returned EMPTY data — falling back to AVC420"
                                            );
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
                            },
                        ) {
                            Some(result) => result?,
                            None => {
                                tracing::warn!("BGRA pixel buffer lock failed during encode");
                                return Ok(Some(self.fallback_pointer_update()));
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
                            self.bitrate_ctrl.record_frame(FrameStats {
                                encode_ms,
                                frame_bytes: total_bytes as u32,
                                is_keyframe,
                            });
                            if let Some(ps) = &self.perf_stats {
                                ps.lock().unwrap().record_frame(
                                    encode_ms,
                                    total_bytes as u32,
                                    is_keyframe,
                                );
                            }
                            self.record_avc_audit(
                                total_bytes as u32,
                                encode_ms,
                                frame.dirty_rects.len(),
                                frame
                                    .dirty_rects
                                    .iter()
                                    .map(|rect| rect.width * rect.height)
                                    .sum(),
                                "gfx-avc-bgra-pixel-buffer",
                            );
                            self.apply_adaptive_decision();
                            let frame_interval_ms = 1000.0 / self.capture_config.frame_rate as f64;
                            if encode_ms > frame_interval_ms * 0.8 {
                                self.skip_next_frame = true;
                                tracing::warn!(
                                    encode_ms = format!("{:.1}", encode_ms),
                                    frame_interval_ms = format!("{:.1}", frame_interval_ms),
                                    "encode overload — skipping next frame"
                                );
                            }
                            self.last_frame_cache = Some(CachedFrame::BgraPixelBuffer(
                                buf.clone_ref(),
                                frame.width,
                                frame.height,
                                frame.stride,
                            ));
                            if is_keyframe {
                                self.last_idr_time = Instant::now();
                            }
                            self.clear_client_stale_flag("gfx-avc-bgra-pixel-buffer");
                            return Ok(Some(DisplayUpdate::GfxFrame(update)));
                        }

                        self.mark_client_may_be_stale("bgra-pixel-buffer-empty-encode");
                        return Ok(None);
                    }
                    FrameData::Raw(bytes) => {
                        // Existing BGRA encode path — continues below
                        let bgra = bytes.as_ref();

                        let min_stride = frame.width as usize * 4;
                        let required_len = (frame.height as usize)
                            .saturating_sub(1)
                            .saturating_mul(frame.stride)
                            .saturating_add(min_stride);
                        if frame.stride < min_stride || bgra.len() < required_len {
                            tracing::warn!(
                                display_frame = self.display_frame_count,
                                width = frame.width,
                                height = frame.height,
                                stride = frame.stride,
                                bgra_len = bgra.len(),
                                required_len,
                                min_stride,
                                "Invalid BGRA frame layout — dropping frame"
                            );
                            return Ok(Some(self.fallback_pointer_update()));
                        }

                        // AVC444 dual-stream path
                        if use_444 && encoder.supports_444() {
                            match encoder.encode_bgra_444(
                                bgra,
                                frame.width,
                                frame.height,
                                frame.stride,
                            ) {
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
                                    self.bitrate_ctrl.record_frame(FrameStats {
                                        encode_ms,
                                        frame_bytes: total_bytes as u32,
                                        is_keyframe,
                                    });
                                    if let Some(ps) = &self.perf_stats {
                                        ps.lock().unwrap().record_frame(
                                            encode_ms,
                                            total_bytes as u32,
                                            is_keyframe,
                                        );
                                    }
                                    self.record_avc_audit(
                                        total_bytes as u32,
                                        encode_ms,
                                        frame.dirty_rects.len(),
                                        frame
                                            .dirty_rects
                                            .iter()
                                            .map(|rect| rect.width * rect.height)
                                            .sum(),
                                        "gfx-avc444",
                                    );
                                    self.apply_adaptive_decision();
                                    let frame_interval_ms =
                                        1000.0 / self.capture_config.frame_rate as f64;
                                    if encode_ms > frame_interval_ms * 0.8 {
                                        self.skip_next_frame = true;
                                        tracing::warn!(
                                            encode_ms = format!("{:.1}", encode_ms),
                                            frame_interval_ms = format!("{:.1}", frame_interval_ms),
                                            "encode overload — skipping next frame"
                                        );
                                    }
                                    self.last_frame_cache = Some(CachedFrame::Bgra {
                                        data: bytes.clone(),
                                        width: frame.width,
                                        height: frame.height,
                                        stride: frame.stride,
                                    });
                                    if is_keyframe {
                                        self.last_idr_time = Instant::now();
                                    }
                                    self.clear_client_stale_flag("gfx-avc444-raw");
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
                                        "AVC444 encode returned EMPTY data — falling back to AVC420"
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        display_frame = self.display_frame_count,
                                        "AVC444 encode failed: {e}, falling back to AVC420"
                                    );
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
                                self.bitrate_ctrl.record_frame(FrameStats {
                                    encode_ms,
                                    frame_bytes: encoded.data.len() as u32,
                                    is_keyframe,
                                });
                                if let Some(ps) = &self.perf_stats {
                                    ps.lock().unwrap().record_frame(
                                        encode_ms,
                                        encoded.data.len() as u32,
                                        is_keyframe,
                                    );
                                }
                                self.record_avc_audit(
                                    encoded.data.len() as u32,
                                    encode_ms,
                                    frame.dirty_rects.len(),
                                    frame
                                        .dirty_rects
                                        .iter()
                                        .map(|rect| rect.width * rect.height)
                                        .sum(),
                                    "gfx-avc420",
                                );
                                self.apply_adaptive_decision();
                                let frame_interval_ms =
                                    1000.0 / self.capture_config.frame_rate as f64;
                                if encode_ms > frame_interval_ms * 0.8 {
                                    self.skip_next_frame = true;
                                    tracing::warn!(
                                        encode_ms = format!("{:.1}", encode_ms),
                                        frame_interval_ms = format!("{:.1}", frame_interval_ms),
                                        "encode overload — skipping next frame"
                                    );
                                }
                                self.last_frame_cache = Some(CachedFrame::Bgra {
                                    data: bytes.clone(),
                                    width: frame.width,
                                    height: frame.height,
                                    stride: frame.stride,
                                });
                                if is_keyframe {
                                    self.last_idr_time = Instant::now();
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
                                self.mark_client_may_be_stale("raw-avc420-empty-encode");
                                return Ok(None);
                            }
                            Err(e) => {
                                tracing::warn!(
                                    display_frame = self.display_frame_count,
                                    "H.264 encode failed: {e}"
                                );
                                self.mark_client_may_be_stale("raw-avc420-encode-error");
                            }
                        }
                    }
                }
            }
        } else if self.encoder.is_some() && session_uses_gfx {
            // H.264 encoder exists and this session negotiated GFX — do not mix
            // bitmap updates into a live GFX session, or reconnects can fail with
            // DECOMPRESSION_FAILED.
            return Ok(Some(self.fallback_pointer_update()));
        }

        // Bitmap path. This is required for clients such as Guacamole when the
        // session did not negotiate the GFX/DVC channel at all.
        if !self.logged_bitmap_mode {
            tracing::info!(
                mode = "bitmap-fallback",
                pixel_format = ?self.capture_config.pixel_format,
                encoder_pref = ?self.encoder_pref,
                session_uses_gfx,
                gfx_avc_ready,
                "Negotiated graphics transport active"
            );
            self.logged_bitmap_mode = true;
        }

        let force_full_bitmap_seed = self.needs_full_bitmap_seed;
        if force_full_bitmap_seed {
            tracing::debug!(
                width = frame.width,
                height = frame.height,
                dirty_rects = frame.dirty_rects.len(),
                "Sending full-frame bitmap seed and deferring incremental diffing to ironrdp-server"
            );
        }

        let force_full_bitmap_reason = if self.client_may_be_stale {
            Some("stale-recovery")
        } else if self.forced_full_bitmap_updates_remaining > 0 {
            Some("seed")
        } else if frame.dirty_rects.is_empty() {
            Some("no-dirty-rects")
        } else if should_force_full_bitmap_update(
            frame.width,
            frame.height,
            &frame.dirty_rects,
            self.last_full_bitmap_sent.map(|sent_at| sent_at.elapsed()),
        ) {
            Some("chaotic-damage")
        } else {
            None
        };
        let force_full_bitmap = force_full_bitmap_reason.is_some();

        let build_bitmap = |bgra_bitmap: &[u8],
                            stride_usize: usize|
         -> Option<(Vec<BitmapUpdate>, bool)> {
            if !force_full_bitmap && !frame.dirty_rects.is_empty() {
                let bpp = 4usize;
                let regions =
                    prepare_bitmap_dirty_regions(frame.width, frame.height, &frame.dirty_rects);
                let mut updates = Vec::with_capacity(regions.len());

                for region in regions {
                    let width = NonZeroU16::new(region.width as u16)?;
                    let height = NonZeroU16::new(region.height as u16)?;
                    let dirty_stride = region.width as usize * bpp;
                    let mut dirty_data = Vec::with_capacity(dirty_stride * region.height as usize);

                    for row in region.y..region.y + region.height {
                        let src_offset = row as usize * stride_usize + region.x as usize * bpp;
                        let src_end = src_offset + dirty_stride;
                        if src_end > bgra_bitmap.len() {
                            return None;
                        }
                        dirty_data.extend_from_slice(&bgra_bitmap[src_offset..src_end]);
                    }

                    let stride = NonZeroUsize::new(dirty_stride)?;
                    let dirty_hints: Vec<DiffRect> = frame
                        .dirty_rects
                        .iter()
                        .filter(|rect| rect.width > 0 && rect.height > 0)
                        .filter_map(|rect| {
                            let left = rect.x.max(region.x);
                            let top = rect.y.max(region.y);
                            let right = rect
                                .x
                                .saturating_add(rect.width)
                                .min(region.x.saturating_add(region.width));
                            let bottom = rect
                                .y
                                .saturating_add(rect.height)
                                .min(region.y.saturating_add(region.height));
                            (right > left && bottom > top).then_some(DiffRect {
                                x: left as usize,
                                y: top as usize,
                                width: (right - left) as usize,
                                height: (bottom - top) as usize,
                            })
                        })
                        .collect();

                    tracing::debug!(
                        x = region.x,
                        y = region.y,
                        width = region.width,
                        height = region.height,
                        dirty_rects = dirty_hints.len(),
                        bytes = dirty_data.len(),
                        "Sending bitmap dirty region"
                    );

                    updates.push(BitmapUpdate {
                        x: region.x as u16,
                        y: region.y as u16,
                        width,
                        height,
                        format: RdpPixelFormat::BgrA32,
                        data: Bytes::from(dirty_data),
                        stride,
                        dirty_rects: dirty_hints,
                    });
                }

                if !updates.is_empty() {
                    return Some((updates, false));
                }
            }

            let width = NonZeroU16::new(frame.width as u16)?;
            let height = NonZeroU16::new(frame.height as u16)?;
            let stride = NonZeroUsize::new(stride_usize)?;
            tracing::debug!(
                width = frame.width,
                height = frame.height,
                bytes = bgra_bitmap.len(),
                reason = force_full_bitmap_reason.unwrap_or("fallback"),
                "Sending full-frame bitmap update"
            );

            Some((
                vec![BitmapUpdate {
                    x: 0,
                    y: 0,
                    width,
                    height,
                    format: RdpPixelFormat::BgrA32,
                    data: Bytes::copy_from_slice(bgra_bitmap),
                    stride,
                    dirty_rects: Vec::new(),
                }],
                true,
            ))
        };

        let bitmap_updates = match &frame.data {
            FrameData::Raw(bgra_bitmap) => build_bitmap(bgra_bitmap, frame.stride),
            FrameData::BgraPixelBuffer(buf) => buf
                .with_locked_bgra(frame.width, frame.height, |bgra, stride| {
                    build_bitmap(bgra, stride)
                })
                .flatten(),
            FrameData::PixelBuffer(_) => {
                tracing::warn!("PixelBuffer frame in bitmap path — should not happen");
                None
            }
        };

        if let Some((mut updates, emitted_full_frame)) = bitmap_updates {
            updates = self.suppress_duplicate_bitmap_updates(updates);
            self.needs_full_bitmap_seed = false;
            if emitted_full_frame {
                self.last_full_bitmap_sent = Some(Instant::now());
                self.forced_full_bitmap_updates_remaining =
                    self.forced_full_bitmap_updates_remaining.saturating_sub(1);
            }
            if updates.is_empty() {
                if emitted_full_frame {
                    self.mark_client_may_be_stale("bitmap-full-frame-suppressed");
                }
                return Ok(None);
            }
            if updates.len() > 1 {
                self.pending_bitmap_updates.extend(updates.drain(1..));
            }

            if let Some(bitmap) = updates.into_iter().next() {
                self.record_bitmap_update_metrics(bitmap.data.len() as u32);
                self.record_bitmap_audit(
                    &bitmap,
                    if emitted_full_frame {
                        "bitmap-full-frame"
                    } else {
                        "bitmap-dirty-region"
                    },
                );
                if emitted_full_frame {
                    self.clear_client_stale_flag("bitmap-full-frame");
                }
                return Ok(Some(DisplayUpdate::Bitmap(bitmap)));
            }
        }

        Ok(Some(self.fallback_pointer_update()))
    }
}

impl Drop for MacDisplayUpdates {
    fn drop(&mut self) {
        self.session_audit.bitmap_bytes = self.gfx_state.lock().unwrap().bitmap_wire_bytes;
        self.session_audit.log_summary();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cursor_shape_to_pointer, extract_bgra_rect, fingerprint_bitmap_update,
        fingerprint_cursor_shape, fingerprint_uncompressed_rects, prepare_uncompressed_dirty_rects,
        should_fallback_from_gfx_avc_no_ack, should_force_full_bitmap_update,
        should_suppress_recent_fingerprint, should_use_nv12_capture_for_gfx,
        validate_dirty_rects_against_previous_bgra, ClampedRect, DIRTY_RECT_VALIDATION_TILE,
    };
    use bytes::Bytes;
    use ironrdp_server::{BitmapUpdate, DiffRect, PixelFormat as RdpPixelFormat, UncompressedRect};
    use macrdp_capture::{CursorShape, Rect};
    use std::collections::VecDeque;
    use std::num::{NonZeroU16, NonZeroUsize};
    use std::time::{Duration, Instant};

    #[test]
    fn compacts_close_dirty_rects_to_bounding_box() {
        let rects = vec![
            Rect {
                x: 10,
                y: 10,
                width: 12,
                height: 8,
            },
            Rect {
                x: 24,
                y: 12,
                width: 10,
                height: 6,
            },
        ];

        let prepared = prepare_uncompressed_dirty_rects(100, 100, &rects);
        assert_eq!(
            prepared,
            vec![ClampedRect {
                x: 10,
                y: 10,
                width: 24,
                height: 8,
            }]
        );
    }

    #[test]
    fn keeps_far_apart_dirty_rects_separate() {
        let rects = vec![
            Rect {
                x: 0,
                y: 0,
                width: 8,
                height: 8,
            },
            Rect {
                x: 80,
                y: 80,
                width: 8,
                height: 8,
            },
        ];

        let prepared = prepare_uncompressed_dirty_rects(100, 100, &rects);
        assert_eq!(prepared.len(), 2);
    }

    #[test]
    fn validates_dirty_rects_against_previous_frame_pixels() {
        let width = 240u32;
        let height = 180u32;
        let stride = width as usize * 4;
        let previous = vec![0u8; stride * height as usize];
        let mut current = previous.clone();

        let changed = Rect {
            x: DIRTY_RECT_VALIDATION_TILE,
            y: DIRTY_RECT_VALIDATION_TILE,
            width: 24,
            height: 24,
        };

        for row in changed.y as usize..(changed.y + changed.height) as usize {
            for col in changed.x as usize..(changed.x + changed.width) as usize {
                let idx = row * stride + col * 4;
                current[idx] = 255;
                current[idx + 1] = 64;
                current[idx + 2] = 32;
                current[idx + 3] = 255;
            }
        }

        let reported = vec![Rect {
            x: 0,
            y: 0,
            width: DIRTY_RECT_VALIDATION_TILE * 2,
            height: DIRTY_RECT_VALIDATION_TILE * 2,
        }];

        let refined = validate_dirty_rects_against_previous_bgra(
            width, height, &reported, &current, stride, &previous, stride,
        );

        assert_eq!(refined.len(), 1);
        assert_eq!(refined[0].x, DIRTY_RECT_VALIDATION_TILE);
        assert_eq!(refined[0].y, DIRTY_RECT_VALIDATION_TILE);
        assert_eq!(refined[0].width, DIRTY_RECT_VALIDATION_TILE);
        assert_eq!(refined[0].height, height - DIRTY_RECT_VALIDATION_TILE);
    }

    #[test]
    fn extracts_bgra_rect_with_exact_row_copy() {
        let stride = 16usize;
        let bgra: Vec<u8> = (0..32).collect();
        let rect = ClampedRect {
            x: 1,
            y: 0,
            width: 2,
            height: 2,
        };

        let bytes = extract_bgra_rect(&bgra, stride, rect).expect("rect extraction failed");
        assert_eq!(
            bytes.as_ref(),
            &[4, 5, 6, 7, 8, 9, 10, 11, 20, 21, 22, 23, 24, 25, 26, 27]
        );
    }

    #[test]
    fn fingerprints_change_when_payload_changes() {
        let rect_a = UncompressedRect {
            x: 5,
            y: 6,
            width: 2,
            height: 2,
            pixel_data: Bytes::from_static(&[1, 2, 3, 4]),
        };
        let rect_b = UncompressedRect {
            x: 5,
            y: 6,
            width: 2,
            height: 2,
            pixel_data: Bytes::from_static(&[1, 2, 3, 9]),
        };

        assert_ne!(
            fingerprint_uncompressed_rects(&[rect_a]),
            fingerprint_uncompressed_rects(&[rect_b])
        );
    }

    #[test]
    fn bitmap_fingerprint_changes_when_geometry_changes() {
        let mk = |x, width| BitmapUpdate {
            x,
            y: 6,
            width: NonZeroU16::new(width).unwrap(),
            height: NonZeroU16::new(2).unwrap(),
            format: RdpPixelFormat::BgrA32,
            data: Bytes::from_static(&[1, 2, 3, 4, 5, 6, 7, 8]),
            stride: NonZeroUsize::new(8).unwrap(),
            dirty_rects: vec![DiffRect {
                x: x as usize,
                y: 6,
                width: width as usize,
                height: 2,
            }],
        };

        assert_ne!(
            fingerprint_bitmap_update(&mk(5, 2)),
            fingerprint_bitmap_update(&mk(6, 2))
        );
    }

    #[test]
    fn bitmap_geometry_fingerprint_ignores_payload_changes() {
        let mk = |payload: &'static [u8]| BitmapUpdate {
            x: 12,
            y: 34,
            width: NonZeroU16::new(128).unwrap(),
            height: NonZeroU16::new(64).unwrap(),
            format: RdpPixelFormat::BgrA32,
            data: Bytes::from_static(payload),
            stride: NonZeroUsize::new(512).unwrap(),
            dirty_rects: vec![DiffRect {
                x: 12,
                y: 34,
                width: 128,
                height: 64,
            }],
        };

        assert_ne!(
            fingerprint_bitmap_update(&mk(&[1, 2, 3, 4])),
            fingerprint_bitmap_update(&mk(&[9, 8, 7, 6]))
        );
    }

    #[test]
    fn cursor_fingerprint_changes_when_shape_changes() {
        let mk = |hot_x: u16, rgba: Vec<u8>| CursorShape {
            width: 16,
            height: 16,
            hot_x,
            hot_y: 3,
            rgba,
        };

        assert_ne!(
            fingerprint_cursor_shape(&mk(2, vec![0, 1, 2, 3])),
            fingerprint_cursor_shape(&mk(4, vec![0, 1, 2, 3]))
        );
        assert_ne!(
            fingerprint_cursor_shape(&mk(2, vec![0, 1, 2, 3])),
            fingerprint_cursor_shape(&mk(2, vec![9, 1, 2, 3]))
        );
    }

    #[test]
    fn converts_cursor_shape_to_color_pointer() {
        let shape = CursorShape {
            width: 2,
            height: 2,
            hot_x: 5,
            hot_y: 7,
            rgba: vec![
                1, 2, 3, 4, 10, 20, 30, 0, // top row RGBA
                50, 60, 70, 80, 90, 100, 110, 120, // bottom row RGBA
            ],
        };

        let pointer = cursor_shape_to_pointer(shape);
        assert_eq!(pointer.width, 2);
        assert_eq!(pointer.height, 2);
        assert_eq!(pointer.hot_x, 5);
        assert_eq!(pointer.hot_y, 7);
        assert_eq!(
            pointer.xor_mask,
            vec![
                70, 60, 50, 110, 100, 90, // bottom row, BGR
                0, 0, 0, 0, 0, 0, // top row, transparent pixels cleared to black
            ]
        );
        assert_eq!(pointer.and_mask, vec![0x00, 0x00, 0xC0, 0x00]);
    }

    #[test]
    fn suppresses_recent_duplicate_fingerprints_within_window() {
        let mut recent = VecDeque::new();
        let base = Instant::now();

        assert!(!should_suppress_recent_fingerprint(
            &mut recent,
            42,
            base,
            Duration::from_millis(120),
            4,
        ));
        assert!(should_suppress_recent_fingerprint(
            &mut recent,
            42,
            base + Duration::from_millis(50),
            Duration::from_millis(120),
            4,
        ));
        assert!(!should_suppress_recent_fingerprint(
            &mut recent,
            42,
            base + Duration::from_millis(200),
            Duration::from_millis(120),
            4,
        ));
    }

    #[test]
    fn falls_back_when_avc_frames_are_never_acknowledged() {
        assert!(!should_fallback_from_gfx_avc_no_ack(3, 0, 3, None));
        assert!(should_fallback_from_gfx_avc_no_ack(4, 0, 4, None));
        assert!(!should_fallback_from_gfx_avc_no_ack(4, 1, 3, None));
    }

    #[test]
    fn falls_back_when_oldest_unacked_avc_frame_stalls_too_long() {
        assert!(should_fallback_from_gfx_avc_no_ack(
            1,
            0,
            1,
            Some(Duration::from_millis(1_300)),
        ));
        assert!(!should_fallback_from_gfx_avc_no_ack(
            1,
            0,
            1,
            Some(Duration::from_millis(800)),
        ));
    }

    #[test]
    fn enables_nv12_after_initial_gfx_seed_for_healthy_avc420_sessions() {
        assert!(should_use_nv12_capture_for_gfx(
            true,
            true,
            true,
            1,
            false,
            false,
            false,
            macrdp_encode::EncoderPreference::Hardware,
            false,
            false,
        ));
    }

    #[test]
    fn keeps_bgra_until_gfx_frames_are_actually_acked() {
        assert!(!should_use_nv12_capture_for_gfx(
            true,
            true,
            true,
            0,
            false,
            false,
            false,
            macrdp_encode::EncoderPreference::Hardware,
            false,
            false,
        ));
    }

    #[test]
    fn keeps_bgra_for_validation_and_fallback_gfx_sessions() {
        assert!(!should_use_nv12_capture_for_gfx(
            true,
            true,
            true,
            1,
            true,
            false,
            false,
            macrdp_encode::EncoderPreference::Hardware,
            false,
            false,
        ));
        assert!(!should_use_nv12_capture_for_gfx(
            true,
            true,
            true,
            1,
            false,
            true,
            false,
            macrdp_encode::EncoderPreference::Hardware,
            false,
            false,
        ));
        assert!(!should_use_nv12_capture_for_gfx(
            true,
            true,
            true,
            1,
            false,
            false,
            true,
            macrdp_encode::EncoderPreference::Hardware,
            false,
            false,
        ));
    }

    #[test]
    fn forces_full_bitmap_for_large_expose_event() {
        let rects = vec![
            Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 30,
            },
            Rect {
                x: 391,
                y: 314,
                width: 1200,
                height: 803,
            },
        ];

        assert!(should_force_full_bitmap_update(1920, 1200, &rects, None));
    }

    #[test]
    fn avoids_full_bitmap_for_sparse_hotspot_plus_top_strip() {
        let rects = vec![
            Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 30,
            },
            Rect {
                x: 570,
                y: 765,
                width: 417,
                height: 235,
            },
        ];

        assert!(!should_force_full_bitmap_update(1920, 1200, &rects, None));
    }

    #[test]
    fn avoids_repeated_full_bitmap_bursts_during_cooldown() {
        let rects = vec![
            Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 30,
            },
            Rect {
                x: 391,
                y: 314,
                width: 1200,
                height: 803,
            },
        ];

        assert!(!should_force_full_bitmap_update(
            1920,
            1200,
            &rects,
            Some(Duration::from_millis(800)),
        ));
        assert!(!should_force_full_bitmap_update(
            1920,
            1200,
            &rects,
            Some(Duration::from_millis(2_500)),
        ));
    }
}

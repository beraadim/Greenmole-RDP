//! Keep fresh and cached startup paths on the same complete RGB baseline policy.

use super::{
    extract_bgra_rect, CachedFrame, ClampedRect, DisplayUpdate, GfxUncompressedUpdate,
    MacDisplayUpdates, UncompressedRect,
};

fn raw_seed_required(
    needs_seed: bool,
    browser_video: bool,
    avc_ready: bool,
    opaque_cache: bool,
    raw_baseline_prepared: bool,
) -> bool {
    needs_seed
        && !(browser_video && avc_ready)
        // Hardware capture may switch to NV12 after the initial RGB baseline
        // was acknowledged. An opaque recovery frame remains on the video
        // path, instead of repeatedly switching capture back to BGRA.
        && !(opaque_cache && raw_baseline_prepared && avc_ready)
}

impl MacDisplayUpdates {
    pub(super) fn needs_initial_rgb_baseline(&self, browser_video: bool) -> bool {
        self.needs_full_gfx_seed && !self.raw_gfx_baseline_prepared && !browser_video
    }

    pub(super) fn raw_gfx_seed_required(&self, browser_video: bool, avc_ready: bool) -> bool {
        raw_seed_required(
            self.needs_full_gfx_seed,
            browser_video,
            avc_ready,
            matches!(self.last_frame_cache, Some(CachedFrame::PixelBuffer(..))),
            self.raw_gfx_baseline_prepared,
        )
    }

    pub(super) fn prepare_cached_raw_gfx_seed(&mut self, avc_ready: bool) -> Option<DisplayUpdate> {
        if self.has_pending_submit || self.pending_avc444.is_some() {
            return None;
        }
        // Materialize once. A missing/invalid/locked buffer must not fall
        // through to AVC and silently consume the pending RGB baseline.
        let update = self.cached_full_frame_uncompressed_update()?;
        let total_bytes: usize = update.rects.iter().map(|rect| rect.pixel_data.len()).sum();
        if !self.logged_gfx_uncompressed_mode {
            tracing::info!(
                mode = "gfx-uncompressed-full-frame-seed",
                pixel_format = ?self.capture_config.pixel_format,
                encoder_pref = ?self.encoder_pref,
                session_uses_gfx = true,
                gfx_avc_ready = avc_ready,
                "Negotiated graphics transport active"
            );
            self.logged_gfx_uncompressed_mode = true;
        }
        // The raw update does not advance the video reference chain. Preserve
        // a self-contained first video picture after this complete RGB image.
        if let Some(encoder) = &mut self.encoder {
            encoder.force_keyframe();
        }
        self.display_frame_count += 1;
        self.needs_full_gfx_seed = false;
        self.raw_gfx_baseline_prepared = true;
        {
            let mut state = self.gfx_state.lock().unwrap();
            state.last_encode_ms = 0.0;
            state.last_frame_bytes = total_bytes as u32;
        }
        tracing::debug!(
            display_frame = self.display_frame_count,
            total_bytes,
            "Prepared complete RGB graphics baseline before AVC"
        );
        self.record_uncompressed_audit(
            &update.rects,
            u32::from(update.width) * u32::from(update.height),
            "gfx-seed-full-frame",
        );
        self.clear_client_stale_flag("gfx-seed-full-frame");
        Some(DisplayUpdate::GfxUncompressed(update))
    }

    pub(super) fn cached_full_frame_uncompressed_update(&self) -> Option<GfxUncompressedUpdate> {
        let cache = self.last_frame_cache.as_ref()?;
        match cache {
            CachedFrame::Bgra {
                data,
                width,
                height,
                stride,
            } => full_bgra_update(data, *width, *height, *stride),
            CachedFrame::BgraPixelBuffer(buffer, width, height, _) => buffer
                .with_locked_bgra(*width, *height, |pixels, stride| {
                    full_bgra_update(pixels, *width, *height, stride)
                })
                .flatten(),
            CachedFrame::PixelBuffer(..) => None,
        }
    }
}

fn full_bgra_update(
    pixels: &[u8],
    width: u32,
    height: u32,
    stride: usize,
) -> Option<GfxUncompressedUpdate> {
    let surface_width = u16::try_from(width).ok().filter(|width| *width != 0)?;
    let surface_height = u16::try_from(height).ok().filter(|height| *height != 0)?;
    let row_bytes = usize::try_from(width).ok()?.checked_mul(4)?;
    let required = usize::try_from(height)
        .ok()?
        .checked_sub(1)?
        .checked_mul(stride)?
        .checked_add(row_bytes)?;
    if stride < row_bytes || pixels.len() < required {
        return None;
    }
    let pixel_data = extract_bgra_rect(
        pixels,
        stride,
        ClampedRect {
            x: 0,
            y: 0,
            width,
            height,
        },
    )?;
    Some(GfxUncompressedUpdate {
        rects: vec![UncompressedRect {
            x: 0,
            y: 0,
            width: surface_width,
            height: surface_height,
            pixel_data,
        }],
        width: surface_width,
        height: surface_height,
    })
}

#[cfg(test)]
#[path = "gfx_seed_tests.rs"]
mod tests;

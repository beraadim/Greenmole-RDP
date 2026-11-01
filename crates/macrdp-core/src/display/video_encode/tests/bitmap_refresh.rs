//! Baseline delivery intent is separate from full-source materialization.
//! Synthetic captures exercise core dispatch without opening a native stream.

use super::fresh_after_slow_encode::{capture, fixture};
use crate::display::*;

fn bitmap_display() -> MacDisplayUpdates {
    let mut display = fixture(false);
    display.encoder_pref = macrdp_encode::EncoderPreference::Bitmap;
    display.gfx_state.lock().unwrap().manual_bitmap_mode = true;
    display
}

fn bitmap(update: Option<DisplayUpdate>) -> BitmapUpdate {
    let Some(DisplayUpdate::Bitmap(bitmap)) = update else {
        panic!("Expected synthetic classic bitmap dispatch");
    };
    bitmap
}

#[test]
fn mandatory_seed_forced_count_and_stale_recovery_force_complete_delivery() {
    for (seed, count, stale) in [
        (true, 0, false),
        (false, 2, false),
        (false, 0, true),
        (true, 1, true),
    ] {
        let mut display = bitmap_display();
        display.needs_full_bitmap_seed = seed;
        display.forced_full_bitmap_updates_remaining = count;
        display.client_may_be_stale = stale;
        // Unchanged host pixels must not suppress a mandatory repair.
        let update = bitmap(
            display
                .encode_and_send_with_damage(capture(42), FrameDamage::Unchanged)
                .unwrap(),
        );
        assert!(update.force_refresh);
        assert_eq!(
            (update.x, update.y, update.width.get(), update.height.get()),
            (0, 0, 16, 16)
        );
        assert!(update.dirty_rects.is_empty());
        assert!(!display.needs_full_bitmap_seed && !display.client_may_be_stale);
        assert_eq!(
            display.forced_full_bitmap_updates_remaining,
            count.saturating_sub(1)
        );
    }
}

#[test]
fn unknown_chaotic_and_incremental_damage_keep_ordinary_diff_semantics() {
    for mode in 0..3 {
        let mut display = bitmap_display();
        let mut frame = capture(42);
        match mode {
            0 => frame.dirty_rects.clear(),
            1 => {
                frame.dirty_rects = vec![Rect {
                    x: 0,
                    y: 0,
                    width: 16,
                    height: 16,
                }]
            }
            _ => {}
        }
        let update = bitmap(display.encode_and_send(frame).unwrap());
        assert!(!update.force_refresh);
        if mode < 2 {
            assert_eq!((update.width.get(), update.height.get()), (16, 16));
            assert!(update.dirty_rects.is_empty());
        }
    }
}

#[test]
fn cached_recovery_forces_delivery_and_consumed_requests_do_not_repeat() {
    let mut display = bitmap_display();
    display.cache_latest_frame(&capture(42));
    display.client_may_be_stale = true;
    let update = bitmap(display.send_cached_full_frame_recovery());
    assert!(update.force_refresh);
    assert_eq!((update.width.get(), update.height.get()), (16, 16));
    assert!(!display.client_may_be_stale);

    display.request_full_bitmap_updates(2);
    for expected in [true, true, false, false] {
        let update = bitmap(display.encode_and_send(capture(42)).unwrap());
        assert_eq!(update.force_refresh, expected);
    }
    assert_eq!(display.forced_full_bitmap_updates_remaining, 0);
    assert!(display
        .encode_and_send_with_damage(capture(42), FrameDamage::Unchanged)
        .unwrap()
        .is_none());
}

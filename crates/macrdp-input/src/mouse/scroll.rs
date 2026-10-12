use anyhow::Result;

const FIXED_ONE: i64 = 1 << 16;
const WHEEL_DELTA: i64 = 120 * FIXED_ONE;

/// Preserve high-resolution wheel fractions separately on each axis. Small
/// values are fractions of a notch, never whole lines. A failed post rolls back
/// the event so an allocation failure cannot leave a huge deferred scroll.
#[derive(Default)]
pub(super) struct ScrollState {
    horizontal: i64,
    vertical: i64,
}

impl ScrollState {
    pub(super) fn scroll(
        &mut self,
        x: i32,
        y: i32,
        post: impl FnMut(i32, i32) -> Result<()>,
    ) -> Result<()> {
        self.scroll_fixed(i64::from(x) * FIXED_ONE, i64::from(y) * FIXED_ONE, post)
    }

    pub(super) fn scroll_fixed(
        &mut self,
        x: i64,
        y: i64,
        mut post: impl FnMut(i32, i32) -> Result<()>,
    ) -> Result<()> {
        // Native positive horizontal wheel values scroll left; RDP positive
        // horizontal deltas scroll right. Vertical deltas have matching signs.
        let x = self.horizontal - x;
        let y = self.vertical + y;
        let lines_x = (x / WHEEL_DELTA) as i32;
        let lines_y = (y / WHEEL_DELTA) as i32;
        if lines_x != 0 || lines_y != 0 {
            post(lines_x, lines_y)?;
        }
        self.horizontal = x % WHEEL_DELTA;
        self.vertical = y % WHEEL_DELTA;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advanced_input_fixed_point_keeps_fractional_units_and_matches_normal_wheel() {
        let mut state = ScrollState::default();
        let mut events = Vec::new();
        for _ in 0..240 {
            state
                .scroll_fixed(0, FIXED_ONE / 2, |x, y| {
                    events.push((x, y));
                    Ok(())
                })
                .unwrap();
        }
        state
            .scroll_fixed(-120 * FIXED_ONE, 0, |x, y| {
                events.push((x, y));
                Ok(())
            })
            .unwrap();
        state
            .scroll(0, -120, |x, y| {
                events.push((x, y));
                Ok(())
            })
            .unwrap();
        assert_eq!(events, [(0, 1), (1, 0), (0, -1)]);
        assert_eq!((state.horizontal, state.vertical), (0, 0));
    }

    #[test]
    fn guacamole_and_mstsc_detents_match_and_small_deltas_accumulate() {
        // Guacamole sends 0x78 for +120 and WHEEL_NEGATIVE | 0x88 for -120.
        // mstsc and finer-resolution wheels use the same 120-unit scale.
        let mut state = ScrollState::default();
        let mut events = Vec::new();
        for delta in [119, 1, -119, -1, 120, -120, 240] {
            state
                .scroll(0, delta, |x, y| {
                    events.push((x, y));
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(events, [(0, 1), (0, -1), (0, 1), (0, -1), (0, 2)]);
        assert_eq!(state.vertical, 0);
    }

    #[test]
    fn axes_preserve_independent_remainders_and_direction_changes_cancel() {
        let mut state = ScrollState::default();
        let mut events = Vec::new();
        for (x, y) in [(119, 60), (-59, -30), (60, 90), (-120, 0)] {
            state
                .scroll(x, y, |x, y| {
                    events.push((x, y));
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(events, [(-1, 1), (1, 0)]);
        assert_eq!((state.horizontal, state.vertical), (0, 0));
    }

    #[test]
    fn failed_post_is_not_replayed_and_extreme_deltas_do_not_overflow() {
        let mut state = ScrollState::default();
        state
            .scroll(0, 60, |_, _| panic!("partial detent"))
            .unwrap();
        assert!(state
            .scroll(120, 60, |_, _| anyhow::bail!("allocation failed"))
            .is_err());
        assert_eq!((state.horizontal, state.vertical), (0, 60 * FIXED_ONE));
        state
            .scroll(i32::MIN, i32::MAX, |x, y| {
                assert_eq!(x, 17_895_697);
                assert_eq!(y, 17_895_697);
                Ok(())
            })
            .unwrap();
        assert!(state.horizontal.abs() < WHEEL_DELTA && state.vertical.abs() < WHEEL_DELTA);
    }
}

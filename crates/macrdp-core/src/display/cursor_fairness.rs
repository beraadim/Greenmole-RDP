//! Interleave cursor shape/hide changes with a queued bitmap batch.

#[derive(Default)]
pub(super) struct CursorFairness {
    graphics_owed: bool,
}

impl CursorFairness {
    pub(super) fn may_poll(&mut self, graphics_pending: bool) -> bool {
        if !graphics_pending {
            self.graphics_owed = false;
        }
        !self.graphics_owed
    }

    pub(super) fn pointer_sent(&mut self, graphics_pending: bool) {
        self.graphics_owed = graphics_pending;
    }

    pub(super) fn graphics_sent(&mut self) {
        self.graphics_owed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn slow_bitmap_batch_interleaves_shape_and_hide_without_starving_regions() {
        let mut fairness = CursorFairness::default();
        let mut regions = VecDeque::from(["region1", "region2", "region3"]);
        // Every write is slow enough for the next cursor poll to be due.
        let mut pointers = VecDeque::from(["shape", "hidden", "shape2"]);
        let mut sent = Vec::new();
        while !regions.is_empty() || !pointers.is_empty() {
            let pending = !regions.is_empty();
            if fairness.may_poll(pending) {
                if let Some(pointer) = pointers.pop_front() {
                    sent.push(pointer);
                    fairness.pointer_sent(pending);
                    continue;
                }
            }
            if let Some(region) = regions.pop_front() {
                sent.push(region);
                fairness.graphics_sent();
            }
        }
        assert_eq!(
            sent,
            ["shape", "region1", "hidden", "region2", "shape2", "region3"]
        );
    }

    #[test]
    fn unchanged_cursor_does_not_consume_a_graphics_turn() {
        let mut fairness = CursorFairness::default();
        assert!(fairness.may_poll(true));
        // No pointer was emitted; the next due shape can still lead the batch.
        assert!(fairness.may_poll(true));
        fairness.pointer_sent(true);
        assert!(!fairness.may_poll(true));
        // Cancellation/empty batch must not suppress future idle cursor updates.
        assert!(fairness.may_poll(false));
        fairness.pointer_sent(false);
        assert!(fairness.may_poll(false));
    }
}

use std::time::{Duration, Instant};

/// CoreGraphics positions are logical points, including on Retina displays.
/// This server tolerance permits hand jitter while bounding a sequence to its first click.
const CLICK_RADIUS_POINTS: f64 = 4.0;

#[derive(Clone, Copy)]
struct Press {
    position: (f64, f64),
    sequence_position: (f64, f64),
    at: Instant,
    count: i64,
    eligible: bool,
}

#[derive(Clone, Copy, Default)]
pub(super) struct ClickState {
    active: [Option<Press>; 5],
    completed: Option<(usize, Press)>,
}

impl ClickState {
    pub fn move_to(&mut self, position: (f64, f64)) {
        for press in self.active.iter_mut().flatten() {
            press.eligible &=
                close_to(press.position, position) && close_to(press.sequence_position, position);
        }
        if self
            .completed
            .is_some_and(|(_, press)| !close_to(press.sequence_position, position))
        {
            self.completed = None;
        }
    }

    pub fn count_for_drag(&self, button: usize) -> i64 {
        self.active[button].map_or(0, |press| press.count)
    }

    /// Called on a copy: only commit the returned sequence after event creation/posting succeeds.
    pub fn transition(
        &mut self,
        button: usize,
        down: bool,
        position: (f64, f64),
        now: Instant,
        interval: Duration,
    ) -> i64 {
        self.move_to(position);
        if down {
            let chord = self.active.iter().any(Option::is_some);
            let previous = self.completed.filter(|(previous_button, press)| {
                !chord
                    && *previous_button == button
                    && close_to(press.sequence_position, position)
                    && now
                        .checked_duration_since(press.at)
                        .is_some_and(|age| age <= interval)
            });
            let count = previous.map_or(1, |(_, press)| press.count.saturating_add(1));
            let sequence_position = previous.map_or(position, |(_, press)| press.sequence_position);
            // Another button interrupts the sequence, including overlapping chords.
            self.cancel_sequence();
            self.active[button] = Some(Press {
                position,
                sequence_position,
                at: now,
                count,
                eligible: !chord,
            });
            count
        } else {
            let Some(press) = self.active[button].take() else {
                return 1;
            };
            // AppKit reports zero on an up outside the multi-click time/space
            // threshold, including a long hold. The down/drag retain their count.
            let count = if now
                .checked_duration_since(press.at)
                .is_some_and(|age| age <= interval)
                && close_to(press.sequence_position, position)
            {
                press.count
            } else {
                0
            };
            self.completed = (press.eligible && count > 0).then_some((button, press));
            count
        }
    }

    /// Keep counts for any failed cleanup releases, but never carry a sequence into a new session.
    pub fn cancel_sequence(&mut self) {
        self.completed = None;
        for press in self.active.iter_mut().flatten() {
            press.eligible = false;
        }
    }
}

fn close_to(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).hypot(a.1 - b.1) <= CLICK_RADIUS_POINTS
}

use std::time::{Duration, Instant};

/// Four native attempts per request, with interruptible 100/200/400 ms backoff.
/// Exhaustion leaves the applied FPS truthful until a new target or stream
/// arrives; it never creates an unbounded loop of native configuration calls.
const MAX_ATTEMPTS: u32 = 4;

pub(super) struct FrameRateUpdate {
    pub fps: u32,
    attempts: u32,
    due: Instant,
}

impl FrameRateUpdate {
    pub fn new(fps: u32) -> Self {
        Self {
            fps,
            attempts: 0,
            due: Instant::now(),
        }
    }

    pub fn attempt(&self) -> u32 {
        self.attempts + 1
    }

    pub fn remaining_delay(&self) -> Duration {
        self.due.saturating_duration_since(Instant::now())
    }

    pub fn failed(&mut self) -> bool {
        self.attempts += 1;
        if self.attempts >= MAX_ATTEMPTS {
            return false;
        }
        self.due = Instant::now() + Duration::from_millis(100 << (self.attempts - 1));
        true
    }
}

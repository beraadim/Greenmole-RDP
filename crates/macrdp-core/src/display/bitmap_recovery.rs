//! Bounded metadata for explicit bitmap repaint preparation, before wire output.

use ironrdp_server::BitmapUpdate;

const MAX_OBSERVATIONS: u8 = 16;

pub(super) enum Source {
    Captured,
    Cached,
}

impl Source {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Captured => "captured",
            Self::Cached => "cached",
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct Diagnostics {
    observations: u8,
}

impl Diagnostics {
    fn sample(&mut self, force_refresh: bool) -> Option<u8> {
        if !force_refresh || self.observations >= MAX_OBSERVATIONS {
            return None;
        }
        self.observations += 1;
        Some(self.observations)
    }

    pub(super) fn record(&mut self, bitmap: &BitmapUpdate, source: Source, session_id: u64) {
        let Some(sample_id) = self.sample(bitmap.force_refresh) else {
            return;
        };
        tracing::info!(target: "macrdp_transition",
            component = "bitmap_recovery", phase = "refresh", state = "prepared",
            worker_pid = std::process::id(), session_id, sample_id,
            source = source.as_str(), force_refresh = true,
            x = bitmap.x, y = bitmap.y,
            width = bitmap.width.get(), height = bitmap.height.get(),
            "Bitmap repaint prepared"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_frames_do_not_consume_the_bounded_repaint_observations() {
        let mut diagnostics = Diagnostics::default();
        for _ in 0..1_000 {
            assert_eq!(diagnostics.sample(false), None);
        }
        for expected in 1..=MAX_OBSERVATIONS {
            assert_eq!(diagnostics.sample(true), Some(expected));
        }
        for _ in 0..1_000 {
            assert_eq!(diagnostics.sample(true), None);
        }
        assert_eq!(diagnostics.observations, MAX_OBSERVATIONS);
    }
}

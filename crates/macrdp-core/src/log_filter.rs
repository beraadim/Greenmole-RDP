//! Keep bounded session-transition evidence available without enabling frame logs.

use tracing_subscriber::EnvFilter;

/// Lifecycle events are infrequent and needed when diagnosing problems after a
/// session has ended. Preserve an explicit global `off` preference.
pub fn with_transition_diagnostics(filter: EnvFilter) -> EnvFilter {
    if filter
        .to_string()
        .split(',')
        .any(|directive| directive == "off" || directive.starts_with("macrdp_transition="))
    {
        filter
    } else {
        filter.add_directive(
            "macrdp_transition=info"
                .parse()
                .expect("constant directive"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::{layer::Context, prelude::*, Layer};

    struct Seen(Arc<Mutex<Vec<String>>>);

    impl<S: tracing::Subscriber> Layer<S> for Seen {
        fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
            self.0
                .lock()
                .unwrap()
                .push(event.metadata().target().into());
        }
    }

    fn observed(level: &str) -> Vec<String> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry()
            .with(with_transition_diagnostics(EnvFilter::new(level)))
            .with(Seen(seen.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "macrdp_transition", "transition fixture");
            tracing::info!(target: "ordinary", "frame fixture");
            tracing::warn!(target: "ordinary", "warning fixture");
        });
        let result = seen.lock().unwrap().clone();
        result
    }

    #[test]
    fn warning_filter_keeps_transition_evidence_without_ordinary_info() {
        assert_eq!(observed("warn"), ["macrdp_transition", "ordinary"]);
    }

    #[test]
    fn explicit_off_remains_off() {
        assert!(observed("off").is_empty());
        assert!(observed("macrdp_ui=off,macrdp_core=off,off").is_empty());
        assert_eq!(observed("warn,macrdp_transition=off"), ["ordinary"]);
        assert_eq!(observed("warn,macrdp_transition=error"), ["ordinary"]);
    }
}

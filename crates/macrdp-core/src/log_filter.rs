//! Keep bounded session-transition evidence available without enabling frame logs.

use tracing::{Level, Metadata};
use tracing_subscriber::{filter::FilterFn, EnvFilter};

/// SSPI's debug/trace instrumentation includes token buffers and serialized
/// credentials. Apply this as a global layer after the reloadable EnvFilter so
/// no logging directive can expose those event or span fields. Metadata is
/// inspected before a subscriber records any field values.
pub fn auth_log_filter() -> FilterFn<fn(&Metadata<'_>) -> bool> {
    tracing_subscriber::filter::filter_fn(auth_metadata_allowed as fn(&Metadata<'_>) -> bool)
}

fn auth_metadata_allowed(metadata: &Metadata<'_>) -> bool {
    let target = metadata.target();
    let authentication = target == "sspi" || target.starts_with("sspi::");
    !authentication || *metadata.level() <= Level::INFO
}

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
    use std::{
        fmt,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };
    use tracing::{field::Visit, span::Attributes};
    use tracing_subscriber::{layer::Context, prelude::*, reload, Layer, Registry};

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

    #[derive(Default)]
    struct AuthenticationRecords {
        events: Vec<(String, Level)>,
        spans: Vec<(String, Level)>,
        fields: Vec<String>,
    }

    struct AuthenticationCapture(Arc<Mutex<AuthenticationRecords>>);

    struct Fields<'a>(&'a mut Vec<String>);

    impl Visit for Fields<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
            self.0.push(format!("{}={value:?}", field.name()));
        }
    }

    impl<S: tracing::Subscriber> Layer<S> for AuthenticationCapture {
        fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
            let mut records = self.0.lock().unwrap();
            records.events.push((
                event.metadata().target().to_owned(),
                *event.metadata().level(),
            ));
            event.record(&mut Fields(&mut records.fields));
        }

        fn on_new_span(&self, attributes: &Attributes<'_>, _: &tracing::Id, _: Context<'_, S>) {
            let mut records = self.0.lock().unwrap();
            records.spans.push((
                attributes.metadata().target().to_owned(),
                *attributes.metadata().level(),
            ));
            attributes.record(&mut Fields(&mut records.fields));
        }

        fn on_record(
            &self,
            _: &tracing::Id,
            values: &tracing::span::Record<'_>,
            _: Context<'_, S>,
        ) {
            values.record(&mut Fields(&mut self.0.lock().unwrap().fields));
        }
    }

    struct SecretProbe(Arc<AtomicUsize>);

    impl fmt::Debug for SecretProbe {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            self.0.fetch_add(1, Ordering::Relaxed);
            formatter.write_str("authentication-secret-marker")
        }
    }

    fn sensitive_authentication_events(probe: &SecretProbe) {
        tracing::debug!(target: "sspi", proof = ?probe, "root authentication target");
        tracing::trace!(target: "sspi::credssp::ts_request", credentials = ?probe, "serialized credential bytes");
        let span =
            tracing::debug_span!(target: "sspi::credssp", "encrypt_message", plaintext = ?probe);
        assert!(span.is_disabled());
        span.record("plaintext", tracing::field::debug(probe));
        let trace_span = tracing::trace_span!(target: "sspi::ntlm", "derive_keys", key = ?probe);
        assert!(trace_span.is_disabled());
    }

    #[test]
    fn authentication_cap_overrides_specific_directives_before_recording_fields() {
        let records = Arc::new(Mutex::new(AuthenticationRecords::default()));
        let formatted = Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new(
                "trace,sspi=trace,sspi::credssp=trace,sspi::credssp::ts_request=trace",
            ))
            .with(auth_log_filter())
            .with(AuthenticationCapture(records.clone()));
        tracing::subscriber::with_default(subscriber, || {
            sensitive_authentication_events(&SecretProbe(formatted.clone()));
            tracing::info!(target: "sspi::credssp", phase = "authentication", "safe state");
            tracing::warn!(target: "sspi::ntlm", "safe warning");
            tracing::error!(target: "sspi", "safe failure");
            tracing::debug!(target: "ordinary", "useful application diagnostic");
            tracing::debug!(target: "sspi_helper", "distinct target remains available");
            let _safe_span =
                tracing::info_span!(target: "sspi::credssp", "process", phase = "binding");
        });
        assert_eq!(formatted.load(Ordering::Relaxed), 0);
        let records = records.lock().unwrap();
        assert_eq!(records.events.len(), 5);
        assert_eq!(records.spans, [("sspi::credssp".into(), Level::INFO)]);
        assert!(!records
            .fields
            .iter()
            .any(|field| field.contains("secret-marker")));
    }

    #[test]
    fn reloading_verbose_authentication_diagnostics_keeps_the_mandatory_cap() {
        let records = Arc::new(Mutex::new(AuthenticationRecords::default()));
        let formatted = Arc::new(AtomicUsize::new(0));
        // Keep the same handle type used by the GUI and supervised worker.
        let (filter, handle): (_, reload::Handle<EnvFilter, Registry>) =
            reload::Layer::new(EnvFilter::new("info"));
        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(auth_log_filter())
            .with(AuthenticationCapture(records.clone()));
        tracing::subscriber::with_default(subscriber, || {
            let probe = SecretProbe(formatted.clone());
            sensitive_authentication_events(&probe);
            tracing::info!(target: "sspi::credssp", "safe state before reload");
            handle
                .reload(EnvFilter::new("trace,sspi::credssp::ts_request=trace"))
                .unwrap();
            sensitive_authentication_events(&probe);
            tracing::info!(target: "sspi::credssp", "safe state after reload");
            tracing::trace!(target: "ordinary", "application trace after reload");
        });
        assert_eq!(formatted.load(Ordering::Relaxed), 0);
        let records = records.lock().unwrap();
        assert_eq!(records.events.len(), 3);
        assert!(records.spans.is_empty());
        assert!(!records
            .fields
            .iter()
            .any(|field| field.contains("secret-marker")));
    }
}

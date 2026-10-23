//! Bounded worker diagnostics and reliable operational evidence-loss markers.
//! Producers never wait; only the existing pipe writer emits aggregated loss.
use super::{protocol, Event};
use anyhow::Result;
use std::{
    io::Write,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc,
    },
};
use tracing_subscriber::Layer;

pub(super) fn record_loss(dropped: &AtomicU64) {
    // Monotonic for the worker lifetime. Saturation must never wrap a loss
    // counter back to zero and make a later observer believe nothing was lost.
    let _ = dropped.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
        Some(count.saturating_add(1))
    });
}

pub(super) struct DiagnosticLayer {
    pub(super) events: mpsc::SyncSender<Event>,
    pub(super) queued: Arc<AtomicUsize>,
    pub(super) dropped: Arc<AtomicU64>,
}
impl DiagnosticLayer {
    fn record_loss(&self) {
        record_loss(&self.dropped);
    }

    fn forward(&self, encoded: serde_json::Result<Vec<u8>>) {
        let Ok(bytes) = encoded else {
            self.record_loss();
            return;
        };
        if bytes.len() > 32 * 1024 {
            self.record_loss();
            return;
        }
        let Ok(record) = String::from_utf8(bytes) else {
            self.record_loss();
            return;
        };
        // Leave most pipe capacity for health, settings replies and terminal
        // events even when debug logging is very busy. Producers never wait.
        if self
            .queued
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < 64).then_some(count + 1)
            })
            .is_err()
        {
            self.record_loss();
            return;
        }
        if self.events.try_send(Event::Diagnostic { record }).is_err() {
            self.queued.fetch_sub(1, Ordering::AcqRel);
            self.record_loss();
        }
    }
}
impl<S: tracing::Subscriber> Layer<S> for DiagnosticLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        self.forward(crate::log_bridge::serialize_event(event));
    }
}

/// Only the existing pipe writer emits loss markers. Sending one through the
/// diagnostic queue could lose the marker in the same saturation it describes.
/// Aggregate into at most one bounded record before each ordinary queued event.
pub(super) fn write_event_with_diagnostic_loss(
    event: &Event,
    output: &mut impl Write,
    dropped: &AtomicU64,
    reported: &mut u64,
) -> Result<()> {
    let observed = dropped.load(Ordering::Acquire);
    if observed > *reported {
        let mut record = serde_json::json!({
            "level": "warn",
            "target": "macrdp_transition",
            "message": "Server worker diagnostic records dropped",
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "fields": {
                "component": "diagnostics", "phase": "queue", "state": "records_dropped",
                "worker_pid": std::process::id(), "dropped_records": observed - *reported
            }
        })
        .to_string();
        record.push('\n');
        protocol::write(&Event::Diagnostic { record }, output)?;
        *reported = observed;
    }
    protocol::write(event, output)
}

#[cfg(test)]
mod tests {
    use super::super::Events;
    use super::*;
    use crate::{ConnectionEvent, ServerEventHandler};
    use std::time::Duration;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn diagnostic_saturation_reserves_capacity_for_health_and_control_replies() {
        let (events, received) = mpsc::sync_channel(256);
        let queued = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicU64::new(0));
        let subscriber = tracing_subscriber::registry().with(DiagnosticLayer {
            events: events.clone(),
            queued: queued.clone(),
            dropped: dropped.clone(),
        });
        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..1_000 {
                tracing::info!("Diagnostic saturation fixture");
            }
        });
        assert_eq!(queued.load(Ordering::Acquire), 64);
        assert_eq!(dropped.load(Ordering::Acquire), 936);
        assert!(events
            .try_send(Event::Snapshot {
                snapshot: Box::default()
            })
            .is_ok());
        for request in 0..191 {
            assert!(events
                .try_send(Event::Updated {
                    request,
                    result: Ok(())
                })
                .is_ok());
        }
        let mut output = Vec::new();
        let mut reported = 0;
        for event in received.try_iter() {
            write_event_with_diagnostic_loss(&event, &mut output, &dropped, &mut reported).unwrap();
        }
        let mut input = std::io::Cursor::new(output);
        let mut diagnostics = 0;
        let mut gaps = 0;
        let mut health = 0;
        let mut replies = 0;
        while let Some(event) = protocol::read::<Event>(&mut input).unwrap() {
            match event {
                Event::Diagnostic { record } => {
                    let value: serde_json::Value = serde_json::from_str(&record).unwrap();
                    if value["fields"]["component"] == "diagnostics" {
                        assert_eq!(diagnostics, 0, "loss must precede the next queued event");
                        assert_eq!(value["fields"]["dropped_records"], 936);
                        assert_eq!(value["fields"]["phase"], "queue");
                        assert_eq!(value["fields"]["state"], "records_dropped");
                        assert_eq!(value["fields"]["worker_pid"], std::process::id());
                        assert_eq!(value["target"], "macrdp_transition");
                        assert!(record.ends_with('\n'));
                        assert!(record.len() < 1024);
                        gaps += 1;
                    } else {
                        diagnostics += 1;
                    }
                }
                Event::Snapshot { .. } => health += 1,
                Event::Updated { result, .. } => {
                    assert!(result.is_ok());
                    replies += 1;
                }
                _ => {}
            }
        }
        assert_eq!(diagnostics, 64);
        assert_eq!(
            gaps, 1,
            "one direct marker must not consume the diagnostic budget"
        );
        assert_eq!(health, 1);
        assert_eq!(replies, 191);
        assert_eq!(reported, 936);
    }

    #[test]
    fn diagnostic_encoding_size_and_pipe_failures_are_counted_without_capacity_leaks() {
        let (events, received) = mpsc::sync_channel(1);
        let layer = DiagnosticLayer {
            events: events.clone(),
            queued: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicU64::new(0)),
        };
        layer.forward(Err(serde_json::Error::io(std::io::Error::other("fixture"))));
        layer.forward(Ok(vec![b'x'; 32 * 1024 + 1]));
        layer.forward(Ok(vec![0xff]));
        assert_eq!(layer.dropped.load(Ordering::Acquire), 3);
        assert_eq!(layer.queued.load(Ordering::Acquire), 0);
        assert!(received.try_recv().is_err());

        events.try_send(Event::Stopped).unwrap();
        layer.forward(Ok(b"{}\n".to_vec()));
        assert_eq!(layer.dropped.load(Ordering::Acquire), 4);
        assert_eq!(layer.queued.load(Ordering::Acquire), 0);
        assert!(matches!(received.try_recv().unwrap(), Event::Stopped));
        drop(received);
        layer.forward(Ok(b"{}\n".to_vec()));
        assert_eq!(layer.dropped.load(Ordering::Acquire), 5);
        assert_eq!(layer.queued.load(Ordering::Acquire), 0);

        layer.dropped.store(u64::MAX - 1, Ordering::Release);
        layer.record_loss();
        layer.record_loss();
        assert_eq!(layer.dropped.load(Ordering::Acquire), u64::MAX);
    }

    #[test]
    fn diagnostic_loss_markers_aggregate_new_loss_before_control_without_recursion() {
        let dropped = AtomicU64::new(5);
        let mut reported = 0;
        let mut output = Vec::new();
        write_event_with_diagnostic_loss(
            &Event::Snapshot {
                snapshot: Box::default(),
            },
            &mut output,
            &dropped,
            &mut reported,
        )
        .unwrap();
        write_event_with_diagnostic_loss(
            &Event::Updated {
                request: 42,
                result: Ok(()),
            },
            &mut output,
            &dropped,
            &mut reported,
        )
        .unwrap();
        dropped.store(8, Ordering::Release);
        write_event_with_diagnostic_loss(&Event::Stopped, &mut output, &dropped, &mut reported)
            .unwrap();
        let mut input = std::io::Cursor::new(output);
        let first = protocol::read::<Event>(&mut input).unwrap().unwrap();
        assert!(
            matches!(first, Event::Diagnostic { record } if serde_json::from_str::<serde_json::Value>(&record).unwrap()["fields"]["dropped_records"] == 5)
        );
        assert!(matches!(
            protocol::read::<Event>(&mut input).unwrap().unwrap(),
            Event::Snapshot { .. }
        ));
        assert!(matches!(
            protocol::read::<Event>(&mut input).unwrap().unwrap(),
            Event::Updated {
                request: 42,
                result: Ok(())
            }
        ));
        let second = protocol::read::<Event>(&mut input).unwrap().unwrap();
        assert!(
            matches!(second, Event::Diagnostic { record } if serde_json::from_str::<serde_json::Value>(&record).unwrap()["fields"]["dropped_records"] == 3)
        );
        assert!(matches!(
            protocol::read::<Event>(&mut input).unwrap().unwrap(),
            Event::Stopped
        ));
        assert!(protocol::read::<Event>(&mut input).unwrap().is_none());
        assert_eq!(reported, 8);
    }

    #[test]
    fn a_full_connection_queue_reports_loss_without_blocking_or_displacing_control() {
        let (events, received) = mpsc::sync_channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        events.try_send(Event::Stopped).unwrap();
        let callback = Events {
            events,
            dropped: dropped.clone(),
        };
        let (finished, completion) = mpsc::sync_channel(1);
        let producer = std::thread::spawn(move || {
            callback.on_connection(ConnectionEvent::Connected(crate::ConnectionInfo {
                client_ip: "fixture".into(),
                client_name: String::new(),
                connected_at: "2026-01-01T00:00:00Z".into(),
            }));
            finished.send(()).unwrap();
        });
        completion.recv_timeout(Duration::from_secs(1)).unwrap();
        producer.join().unwrap();
        assert_eq!(dropped.load(Ordering::Acquire), 1);
        assert!(matches!(received.try_recv().unwrap(), Event::Stopped));
        assert!(received.try_recv().is_err());

        let mut output = Vec::new();
        let mut reported = 0;
        write_event_with_diagnostic_loss(&Event::Stopped, &mut output, &dropped, &mut reported)
            .unwrap();
        let mut input = std::io::Cursor::new(output);
        assert!(
            matches!(protocol::read::<Event>(&mut input).unwrap().unwrap(), Event::Diagnostic { record } if serde_json::from_str::<serde_json::Value>(&record).unwrap()["fields"]["dropped_records"] == 1)
        );
        assert!(matches!(
            protocol::read::<Event>(&mut input).unwrap().unwrap(),
            Event::Stopped
        ));
    }
}

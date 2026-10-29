//! Session-scoped client microphone -> macOS HAL input bridge.
//!
//! RDP callbacks only enqueue validated 10 ms PCM packets. A bounded worker owns
//! resampling and Core Audio IPC; the real-time driver has no network or file I/O.

mod convert;
mod native;
mod wire;
mod worker;
use worker::run_worker;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use ironrdp_server::{MicrophonePcmFormat, MicrophoneServerBackend, MicrophoneServerFactory};

const QUEUE_PACKETS: usize = 8;
const MAX_PACKET_AGE: Duration = Duration::from_millis(100);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(75);
static WORKER_ACTIVE: AtomicBool = AtomicBool::new(false);
static STATUS_PROBE_ACTIVE: AtomicBool = AtomicBool::new(false);

struct Admission<'a>(&'a AtomicBool);

impl<'a> Admission<'a> {
    fn acquire(active: &'a AtomicBool) -> Option<Self> {
        active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(active))
    }
}

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Read-only discovery of the loaded driver and its compatible protocol.
pub fn driver_available() -> bool {
    let Some(_admission) = Admission::acquire(&STATUS_PROBE_ACTIVE) else {
        return false;
    };
    native::Driver::connect().is_ok()
}

/// New connections receive independent queues, sample clocks and generations.
#[derive(Default)]
pub struct MacMicrophoneFactory;

impl MicrophoneServerFactory for MacMicrophoneFactory {
    fn build_backend(&self) -> Box<dyn MicrophoneServerBackend> {
        Box::<Backend>::default()
    }
}

struct Packet {
    sequence: u64,
    received_at: Instant,
    pcm: Vec<u8>,
}

#[derive(Default)]
struct WorkerState {
    cancelled: AtomicBool,
    failed: AtomicBool,
    dropped: AtomicU64,
}

struct Worker {
    sender: mpsc::SyncSender<Packet>,
    state: Arc<WorkerState>,
    completed: mpsc::Receiver<()>,
    packet_bytes: usize,
    frame_bytes: usize,
    next_sequence: u64,
}

#[derive(Default)]
struct Backend {
    worker: Option<Worker>,
}

impl MicrophoneServerBackend for Backend {
    fn start(&mut self, format: MicrophonePcmFormat) -> anyhow::Result<()> {
        anyhow::ensure!(self.worker.is_none(), "Microphone session already started");
        // Core Audio is outside our cancellation boundary. Keep a process-wide
        // slot until the OS call actually returns, even after timeout/disconnect,
        // so repeated connections cannot accumulate stuck worker threads.
        let admission = Admission::acquire(&WORKER_ACTIVE).ok_or_else(|| {
            anyhow::anyhow!("A previous microphone worker is still stopping; retry after cleanup")
        })?;
        let converter = convert::Converter::new(format)?;
        let packet_bytes = (format.sample_rate as usize / 100) * usize::from(format.channels) * 2;
        let (sender, receiver) = mpsc::sync_channel(QUEUE_PACKETS);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (completed_tx, completed) = mpsc::sync_channel(1);
        let state = Arc::new(WorkerState::default());
        let worker_state = Arc::clone(&state);
        std::thread::Builder::new()
            .name("rdp-microphone".into())
            .spawn(move || {
                let _admission = admission;
                let result = run_worker(receiver, converter, &worker_state, ready_tx);
                if let Err(error) = result {
                    worker_state.failed.store(true, Ordering::Release);
                    tracing::warn!(%error, "Client microphone bridge stopped");
                }
                let _ = completed_tx.send(());
            })?;
        let result = ready_rx.recv_timeout(Duration::from_secs(2));
        if !matches!(result, Ok(true)) {
            state.cancelled.store(true, Ordering::Release);
            anyhow::bail!(
                "Greenmole microphone driver could not start; check Microphone settings and logs"
            );
        }
        self.worker = Some(Worker {
            sender,
            state,
            completed,
            packet_bytes,
            frame_bytes: usize::from(format.channels) * 2,
            next_sequence: 0,
        });
        tracing::info!(
            sample_rate = format.sample_rate,
            channels = format.channels,
            driver_sample_rate = 48_000,
            queue_limit_ms = QUEUE_PACKETS * 10,
            "Client microphone bridge started for the active connection"
        );
        Ok(())
    }

    fn audio_data(&mut self, data: &[u8]) -> anyhow::Result<()> {
        let worker = self
            .worker
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Microphone session is not started"))?;
        anyhow::ensure!(
            !worker.state.failed.load(Ordering::Acquire),
            "Microphone driver worker failed"
        );
        anyhow::ensure!(
            !data.is_empty()
                && data.len() <= worker.packet_bytes
                && data.len() % worker.frame_bytes == 0,
            "Invalid microphone packet size"
        );
        let packet = Packet {
            sequence: worker.next_sequence,
            received_at: Instant::now(),
            pcm: data.to_vec(),
        };
        worker.next_sequence = worker.next_sequence.wrapping_add(1);
        match worker.sender.try_send(packet) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => {
                worker.state.dropped.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                anyhow::bail!("Microphone driver worker disconnected")
            }
        }
    }

    fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.state.cancelled.store(true, Ordering::Release);
            drop(worker.sender);
            // Normal END is acknowledged before a new connection can claim the
            // device. A stuck HAL call cannot block session teardown indefinitely;
            // the driver's independent 250 ms lease also rejects stale samples.
            let ended = worker
                .completed
                .recv_timeout(Duration::from_millis(250))
                .is_ok();
            tracing::info!(
                driver_worker_ended = ended,
                dropped_packets = worker.state.dropped.load(Ordering::Relaxed),
                "Client microphone bridge ended"
            );
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests;

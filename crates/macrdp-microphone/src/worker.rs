//! Bounded off-thread conversion and session ownership of the HAL producer.

use super::{convert, native, wire, Packet, WorkerState, MAX_PACKET_AGE, STOP_POLL_INTERVAL};
use std::sync::{atomic::Ordering, mpsc};
use std::time::{Duration, Instant};

pub(super) trait DriverTransport {
    fn send(&self, bytes: &[u8]) -> anyhow::Result<()>;
}

impl DriverTransport for native::Driver {
    fn send(&self, bytes: &[u8]) -> anyhow::Result<()> {
        self.send(bytes)
    }
}

pub(super) struct DriverSession<D: DriverTransport = native::Driver> {
    pub(super) driver: D,
    pub(super) generation: u64,
}

impl DriverSession {
    fn new() -> anyhow::Result<Self> {
        let driver = native::Driver::connect()?;
        let mut random = [0_u8; 8];
        getrandom::getrandom(&mut random)
            .map_err(|_| anyhow::anyhow!("Could not create microphone session identifier"))?;
        let generation = u64::from_le_bytes(random).max(1);
        let session = Self { driver, generation };
        session.send(wire::BEGIN, &[])?;
        Ok(session)
    }
}

impl<D: DriverTransport> DriverSession<D> {
    pub(super) fn send(&self, opcode: u32, samples: &[f32]) -> anyhow::Result<()> {
        self.driver
            .send(&wire::message(opcode, self.generation, samples)?)
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        // A capture pause may already have expired the old lease. BEGIN still
        // checks producer ownership, so a failed END cannot bypass another
        // producer's live lease or revive old buffered samples.
        let _ = self.send(wire::END, &[]);
        self.generation = self.generation.wrapping_add(1).max(1);
        self.send(wire::BEGIN, &[])
    }
}

impl<D: DriverTransport> Drop for DriverSession<D> {
    fn drop(&mut self) {
        if let Err(error) = self.send(wire::END, &[]) {
            tracing::debug!(%error, "Microphone end could not be delivered; driver lease will expire");
        }
    }
}

pub(super) fn run_worker(
    receiver: mpsc::Receiver<Packet>,
    mut converter: convert::Converter,
    state: &WorkerState,
    ready: mpsc::SyncSender<bool>,
) -> anyhow::Result<()> {
    let mut session = DriverSession::new()?;
    if ready.send(true).is_err() {
        return Ok(());
    }
    pump(receiver, &mut converter, state, &mut session)
}

pub(super) fn pump<D: DriverTransport>(
    receiver: mpsc::Receiver<Packet>,
    converter: &mut convert::Converter,
    state: &WorkerState,
    session: &mut DriverSession<D>,
) -> anyhow::Result<()> {
    let mut expected_sequence = 0;
    let mut stale = 0_u64;
    let mut discontinuities = 0_u64;
    let mut delivered = 0_u64;
    let mut last_log = Instant::now();
    let mut last_delivery = Instant::now();
    let mut last_activity = Instant::now();
    let mut last_sequence_at: Option<Instant> = None;
    while !state.cancelled.load(Ordering::Acquire) {
        let packet = match receiver.recv_timeout(STOP_POLL_INTERVAL) {
            Ok(packet) => packet,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Never keep a producer lease alive without fresh PCM. The
                // driver independently becomes silent when capture stalls.
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if state.cancelled.load(Ordering::Acquire) {
            break;
        }
        if packet.received_at.elapsed() > MAX_PACKET_AGE {
            stale += 1;
            continue;
        }
        if packet.sequence != expected_sequence
            || last_activity.elapsed() > MAX_PACKET_AGE
            || last_sequence_at.is_some_and(|previous| {
                packet.received_at.saturating_duration_since(previous) > MAX_PACKET_AGE
            })
        {
            converter.reset();
            session.reset()?;
            discontinuities += 1;
        }
        expected_sequence = packet.sequence.wrapping_add(1);
        last_activity = Instant::now();
        last_sequence_at = Some(packet.received_at);
        let samples = converter.convert(&packet.pcm)?;
        if !samples.is_empty() {
            if state.cancelled.load(Ordering::Acquire) {
                break;
            }
            session.send(wire::PCM, samples)?;
            delivered += 1;
            last_delivery = Instant::now();
        }
        if last_log.elapsed() >= Duration::from_secs(5) {
            tracing::info!(
                delivered_packets = delivered,
                stale_packets = stale,
                dropped_packets = state.dropped.load(Ordering::Relaxed),
                discontinuities,
                queue_age_ms = packet.received_at.elapsed().as_millis() as u64,
                "Client microphone bridge health"
            );
            last_log = Instant::now();
        }
    }
    tracing::info!(
        delivered_packets = delivered,
        stale_packets = stale,
        discontinuities,
        last_delivery_age_ms = last_delivery.elapsed().as_millis() as u64,
        "Client microphone worker finished; buffered audio discarded"
    );
    Ok(())
}

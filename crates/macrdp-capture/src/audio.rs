//! Bounded, fresh audio delivery from the native capture callback.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use screencapturekit::prelude::*;
use tokio::sync::Notify;

pub const MAX_AUDIO_AGE: Duration = Duration::from_millis(200);
const MAX_QUEUED_FRAMES: usize = 64;

/// Interleaved Float32 PCM with both presentation and local capture clocks.
#[derive(Clone)]
pub struct AudioFrame {
    pub data: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    pub num_samples: usize,
    pub timestamp_ms: u64,
    pub captured_at: Instant,
}

impl AudioFrame {
    pub fn duration(&self) -> Option<Duration> {
        if self.sample_rate == 0
            || self.channels == 0
            || self.num_samples == 0
            || self.num_samples.checked_mul(usize::from(self.channels))? != self.data.len()
        {
            return None;
        }
        let nanos =
            (self.num_samples as u128 * 1_000_000_000).div_ceil(u128::from(self.sample_rate));
        Some(Duration::from_nanos(u64::try_from(nanos).ok()?))
    }
}

#[derive(Default)]
pub struct AudioCaptureStats {
    dropped: AtomicU64,
    stale: AtomicU64,
    invalid: AtomicU64,
    queue_age_us: AtomicU64,
    queued_us: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AudioCaptureSnapshot {
    pub dropped_frames: u64,
    pub stale_frames: u64,
    pub invalid_frames: u64,
    pub queue_age_ms: f64,
    pub queued_ms: f64,
}

impl AudioCaptureStats {
    pub fn snapshot(&self) -> AudioCaptureSnapshot {
        AudioCaptureSnapshot {
            dropped_frames: self.dropped.load(Ordering::Relaxed),
            stale_frames: self.stale.load(Ordering::Relaxed),
            invalid_frames: self.invalid.load(Ordering::Relaxed),
            queue_age_ms: self.queue_age_us.load(Ordering::Relaxed) as f64 / 1000.0,
            queued_ms: self.queued_us.load(Ordering::Relaxed) as f64 / 1000.0,
        }
    }

    pub fn record_invalid(&self) {
        self.invalid.fetch_add(1, Ordering::Relaxed);
    }
}

struct Pending {
    frames: VecDeque<(AudioFrame, Duration)>,
    duration: Duration,
    senders: usize,
    receiver_closed: bool,
}

struct Mailbox {
    pending: Mutex<Pending>,
    ready: Notify,
    stats: Arc<AudioCaptureStats>,
}

pub struct AudioFrameSender(Arc<Mailbox>);
pub struct AudioFrameReceiver(Arc<Mailbox>);

pub fn audio_channel(stats: Arc<AudioCaptureStats>) -> (AudioFrameSender, AudioFrameReceiver) {
    let mailbox = Arc::new(Mailbox {
        pending: Mutex::new(Pending {
            frames: VecDeque::new(),
            duration: Duration::ZERO,
            senders: 1,
            receiver_closed: false,
        }),
        ready: Notify::new(),
        stats,
    });
    (
        AudioFrameSender(mailbox.clone()),
        AudioFrameReceiver(mailbox),
    )
}

impl Clone for AudioFrameSender {
    fn clone(&self) -> Self {
        self.0.pending.lock().unwrap().senders += 1;
        Self(self.0.clone())
    }
}

impl Drop for AudioFrameSender {
    fn drop(&mut self) {
        self.0.pending.lock().unwrap().senders -= 1;
        self.0.ready.notify_one();
    }
}

impl AudioFrameSender {
    /// Never waits for the consumer. Under pressure preserve the newest sound,
    /// unlike reliable clipboard messages which must never be discarded.
    pub fn send(&self, frame: AudioFrame) {
        let Some(duration) = frame
            .duration()
            .filter(|duration| *duration <= MAX_AUDIO_AGE)
        else {
            self.0.stats.record_invalid();
            return;
        };
        let mut pending = self.0.pending.lock().unwrap();
        if pending.receiver_closed {
            return;
        }
        while pending.duration + duration > MAX_AUDIO_AGE
            || pending.frames.len() >= MAX_QUEUED_FRAMES
        {
            if let Some((_, removed)) = pending.frames.pop_front() {
                pending.duration -= removed;
                self.0.stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
        pending.duration += duration;
        pending.frames.push_back((frame, duration));
        self.0
            .stats
            .queued_us
            .store(pending.duration.as_micros() as u64, Ordering::Relaxed);
        drop(pending);
        self.0.ready.notify_one();
    }
}

impl AudioFrameReceiver {
    fn try_recv_at(&self, now: Instant) -> Option<AudioFrame> {
        let mut pending = self.0.pending.lock().unwrap();
        while let Some((frame, duration)) = pending.frames.pop_front() {
            pending.duration -= duration;
            self.0
                .stats
                .queued_us
                .store(pending.duration.as_micros() as u64, Ordering::Relaxed);
            let age = now.saturating_duration_since(frame.captured_at);
            self.0.stats.queue_age_us.store(
                age.as_micros().min(u128::from(u64::MAX)) as u64,
                Ordering::Relaxed,
            );
            if age > MAX_AUDIO_AGE {
                self.0.stats.stale.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            return Some(frame);
        }
        None
    }

    pub async fn recv(&mut self) -> Option<AudioFrame> {
        loop {
            let ready = self.0.ready.notified();
            if let Some(frame) = self.try_recv_at(Instant::now()) {
                return Some(frame);
            }
            {
                let pending = self.0.pending.lock().unwrap();
                if pending.senders == 0 && pending.frames.is_empty() {
                    return None;
                }
            }
            ready.await;
        }
    }
}

impl Drop for AudioFrameReceiver {
    fn drop(&mut self) {
        let mut pending = self.0.pending.lock().unwrap();
        pending.receiver_closed = true;
        pending.frames.clear();
        pending.duration = Duration::ZERO;
        self.0.stats.queued_us.store(0, Ordering::Relaxed);
    }
}

pub(crate) struct AudioOutputHandler {
    pub audio_tx: AudioFrameSender,
}

impl SCStreamOutputTrait for AudioOutputHandler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
        if of_type != SCStreamOutputType::Audio {
            return;
        }
        let captured_at = Instant::now();
        let frame = extract_audio(&sample, captured_at);
        match frame {
            Some(frame) => self.audio_tx.send(frame),
            None => self.audio_tx.0.stats.record_invalid(),
        }
    }
}

fn extract_audio(sample: &CMSampleBuffer, captured_at: Instant) -> Option<AudioFrame> {
    let buffers = sample.audio_buffer_list()?;
    let timestamp = sample.presentation_timestamp();
    if timestamp.timescale <= 0 || timestamp.value < 0 {
        return None;
    }
    let timestamp_ms =
        u64::try_from(timestamp.value as u128 * 1000 / timestamp.timescale as u128).ok()?;
    let format = sample.format_description()?;
    let rate = format.audio_sample_rate()?;
    let channels = u16::try_from(format.audio_channel_count()?).ok()?;
    // ScreenCaptureKit is explicitly configured for this format. Reject a
    // surprise format instead of reading bytes with incorrect sample geometry.
    if rate != 48000.0 || channels != 2 {
        return None;
    }
    let channel_count = usize::from(channels);
    let num_buffers = buffers.num_buffers();
    if num_buffers != 1 && num_buffers != channel_count {
        return None;
    }
    let first = buffers.get(0)?;
    let first_data = first.data();
    if first_data.is_empty() || first_data.len() % 4 != 0 {
        return None;
    }
    let num_samples = first_data.len() / 4 / if num_buffers == 1 { channel_count } else { 1 };
    if num_samples == 0 || num_samples > 9600 {
        return None;
    }
    let mut data = Vec::with_capacity(num_samples * channel_count);
    if num_buffers == 1 {
        if first_data.len() != num_samples * channel_count * 4 {
            return None;
        }
        // AudioBuffer byte slices do not guarantee f32 alignment. Decode native
        // bytes safely rather than casting their pointer to a typed slice.
        data.extend(
            first_data
                .chunks_exact(4)
                .map(|bytes| f32::from_ne_bytes(bytes.try_into().unwrap())),
        );
    } else {
        let other = buffers.get(1)?;
        let other_data = other.data();
        if other_data.len() != first_data.len() {
            return None;
        }
        for (left, right) in first_data.chunks_exact(4).zip(other_data.chunks_exact(4)) {
            data.push(f32::from_ne_bytes(left.try_into().unwrap()));
            data.push(f32::from_ne_bytes(right.try_into().unwrap()));
        }
    }
    Some(AudioFrame {
        data,
        sample_rate: 48000,
        channels,
        num_samples,
        timestamp_ms,
        captured_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(at: Instant, timestamp: u64, ms: usize) -> AudioFrame {
        AudioFrame {
            data: vec![0.0; ms * 48 * 2],
            sample_rate: 48000,
            channels: 2,
            num_samples: ms * 48,
            timestamp_ms: timestamp,
            captured_at: at,
        }
    }

    #[test]
    fn pressure_bounds_duration_and_delivers_newest_sound() {
        let stats = Arc::new(AudioCaptureStats::default());
        let (tx, rx) = audio_channel(stats.clone());
        let now = Instant::now();
        for i in 0..30 {
            tx.send(frame(now, i * 20, 20));
        }
        assert_eq!(stats.snapshot().queued_ms, 200.0);
        assert_eq!(stats.snapshot().dropped_frames, 20);
        assert_eq!(rx.try_recv_at(now).unwrap().timestamp_ms, 400);
        assert_eq!(stats.snapshot().queued_ms, 180.0);
    }

    #[test]
    fn expired_capture_is_discarded_even_without_a_new_frame() {
        let stats = Arc::new(AudioCaptureStats::default());
        let (tx, rx) = audio_channel(stats.clone());
        let now = Instant::now();
        tx.send(frame(now, 0, 20));
        assert!(rx.try_recv_at(now + Duration::from_millis(201)).is_none());
        assert_eq!(stats.snapshot().stale_frames, 1);
        assert_eq!(stats.snapshot().queue_age_ms, 201.0);
        assert_eq!(stats.snapshot().queued_ms, 0.0);
    }

    #[tokio::test]
    async fn sender_clones_keep_receiver_alive_and_cancellation_retains_frames() {
        let (tx, mut rx) = audio_channel(Arc::default());
        let second = tx.clone();
        drop(tx);
        assert!(tokio::time::timeout(Duration::ZERO, rx.recv())
            .await
            .is_err());
        second.send(frame(Instant::now(), 42, 20));
        assert_eq!(rx.recv().await.unwrap().timestamp_ms, 42);
        drop(second);
        assert!(rx.recv().await.is_none());
    }

    #[test]
    fn invalid_sample_geometry_and_oversize_frames_never_enter_queue() {
        let stats = Arc::new(AudioCaptureStats::default());
        let (tx, rx) = audio_channel(stats.clone());
        let now = Instant::now();
        let mut broken = frame(now, 0, 20);
        broken.data.pop();
        tx.send(broken);
        tx.send(frame(now, 0, 201));
        assert!(rx.try_recv_at(now).is_none());
        assert_eq!(stats.snapshot().invalid_frames, 2);
    }
}

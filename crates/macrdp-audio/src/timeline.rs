//! Packetize captured sound without carrying old samples across timeline gaps.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use macrdp_capture::{AudioFrame, MAX_AUDIO_AGE};

const PTS_TOLERANCE_MS: u128 = 3;

pub(crate) struct AudioPacket {
    pub data: Vec<u8>,
    pub timestamp_ms: u32,
    pub captured_at: Instant,
    pub duration: Duration,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PushOutcome {
    pub discontinuity: bool,
    pub dropped_partial: bool,
    pub stale: bool,
    pub invalid: bool,
}

pub(crate) struct AudioTimeline {
    samples: VecDeque<f32>,
    sample_rate: u32,
    channels: u16,
    packet_samples: usize,
    base_ms: Option<u64>,
    sent_samples: u64,
    expected_pts: Option<u128>,
    capture_spans: VecDeque<(usize, Instant)>,
}

impl AudioTimeline {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        let packet_samples = (sample_rate as usize / 50) * usize::from(channels);
        Self {
            samples: VecDeque::with_capacity(packet_samples * 2),
            sample_rate,
            channels,
            packet_samples,
            base_ms: None,
            sent_samples: 0,
            expected_pts: None,
            capture_spans: VecDeque::new(),
        }
    }

    pub fn reset(&mut self) {
        self.samples.clear();
        self.base_ms = None;
        self.sent_samples = 0;
        self.expected_pts = None;
        self.capture_spans.clear();
    }

    pub fn push(&mut self, frame: AudioFrame, now: Instant) -> PushOutcome {
        let mut outcome = PushOutcome::default();
        if frame.sample_rate != self.sample_rate
            || frame.channels != self.channels
            || self.packet_samples == 0
            || frame.duration().is_none_or(|d| d > MAX_AUDIO_AGE)
        {
            outcome.invalid = true;
            return outcome;
        }
        if now.saturating_duration_since(frame.captured_at) > MAX_AUDIO_AGE {
            outcome.stale = true;
            return outcome;
        }
        let pts = u128::from(frame.timestamp_ms) * u128::from(self.sample_rate);
        let discontinuity = self.expected_pts.is_some_and(|expected| {
            expected.abs_diff(pts) > PTS_TOLERANCE_MS * u128::from(self.sample_rate)
        });
        let stale_partial = self
            .capture_spans
            .front()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) > MAX_AUDIO_AGE);
        if discontinuity || stale_partial {
            outcome.discontinuity = discontinuity;
            outcome.stale = stale_partial;
            outcome.dropped_partial = !self.samples.is_empty();
            self.reset();
        }
        if self.base_ms.is_none() {
            self.base_ms = Some(frame.timestamp_ms);
        }
        self.expected_pts = Some(pts + frame.num_samples as u128 * 1000);
        self.capture_spans
            .push_back((frame.data.len(), frame.captured_at));
        self.samples.extend(frame.data);
        outcome
    }

    pub fn pop_packet(&mut self) -> Option<AudioPacket> {
        if self.samples.len() < self.packet_samples || self.packet_samples == 0 {
            return None;
        }
        let per_channel = self.packet_samples / usize::from(self.channels);
        let timestamp = (self.base_ms? as u32).wrapping_add(
            (u128::from(self.sent_samples) * 1000 / u128::from(self.sample_rate)) as u32,
        );
        let captured_at = self.capture_spans.front()?.1;
        // Consume the ring directly; no intermediate Float32 chunk allocation.
        let mut data = Vec::with_capacity(self.packet_samples * 2);
        for sample in self.samples.drain(..self.packet_samples) {
            let value = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
            data.extend_from_slice(&value.to_le_bytes());
        }
        self.sent_samples += per_channel as u64;
        let mut consumed = self.packet_samples;
        while consumed > 0 {
            let (count, _) = self.capture_spans.front_mut().unwrap();
            let taken = consumed.min(*count);
            *count -= taken;
            consumed -= taken;
            if *count == 0 {
                self.capture_spans.pop_front();
            }
        }
        Some(AudioPacket {
            data,
            timestamp_ms: timestamp,
            captured_at,
            duration: Duration::from_nanos(
                (per_channel as u64 * 1_000_000_000).div_ceil(u64::from(self.sample_rate)),
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(at: Instant, pts: u64, ms: usize, value: f32) -> AudioFrame {
        AudioFrame {
            data: vec![value; ms * 48 * 2],
            sample_rate: 48000,
            channels: 2,
            num_samples: ms * 48,
            timestamp_ms: pts,
            captured_at: at,
        }
    }

    #[test]
    fn continuous_partial_frames_keep_sample_clock_without_rounding_drift() {
        let now = Instant::now();
        let mut timeline = AudioTimeline::new(48000, 2);
        assert_eq!(
            timeline.push(frame(now, 1000, 10, 0.5), now),
            PushOutcome::default()
        );
        assert!(timeline.pop_packet().is_none());
        timeline.push(frame(now, 1010, 10, -0.5), now);
        let packet = timeline.pop_packet().unwrap();
        assert_eq!(packet.timestamp_ms, 1000);
        assert_eq!(packet.duration, Duration::from_millis(20));
        assert_eq!(packet.data.len(), 3840);
        for i in 1..100 {
            assert!(
                !timeline
                    .push(frame(now, 1000 + i * 20, 20, 0.0), now)
                    .discontinuity
            );
            assert_eq!(
                timeline.pop_packet().unwrap().timestamp_ms,
                1000 + i as u32 * 20
            );
        }
    }

    #[test]
    fn forward_gap_discards_partial_packet_and_anchors_to_new_capture_pts() {
        let now = Instant::now();
        let mut timeline = AudioTimeline::new(48000, 2);
        timeline.push(frame(now, 0, 10, 1.0), now);
        let outcome = timeline.push(frame(now, 300, 20, 0.0), now);
        assert!(outcome.discontinuity && outcome.dropped_partial);
        let packet = timeline.pop_packet().unwrap();
        assert_eq!(packet.timestamp_ms, 300);
        assert!(packet.data.iter().all(|b| *b == 0));
    }

    #[test]
    fn backwards_capture_clock_resynchronizes_and_rdp_clock_wraps() {
        let now = Instant::now();
        let mut timeline = AudioTimeline::new(48000, 2);
        timeline.push(frame(now, u64::from(u32::MAX) + 5, 20, 0.0), now);
        assert_eq!(timeline.pop_packet().unwrap().timestamp_ms, 4);
        assert!(timeline.push(frame(now, 25, 20, 0.0), now).discontinuity);
        assert_eq!(timeline.pop_packet().unwrap().timestamp_ms, 25);
    }

    #[test]
    fn stale_audio_and_unexpected_formats_are_rejected() {
        let now = Instant::now();
        let mut timeline = AudioTimeline::new(48000, 2);
        assert!(
            timeline
                .push(frame(now, 0, 20, 0.0), now + Duration::from_millis(201))
                .stale
        );
        assert!(timeline.pop_packet().is_none());
        let mut bad = frame(now, 0, 20, 0.0);
        bad.sample_rate = 44100;
        assert!(timeline.push(bad, now).invalid);
        assert!(timeline.pop_packet().is_none());
    }

    #[test]
    fn native_millisecond_timestamp_quantization_does_not_trigger_resync() {
        let now = Instant::now();
        let mut timeline = AudioTimeline::new(48000, 2);
        for i in 0..1000 {
            let samples = 512;
            let frame = AudioFrame {
                data: vec![0.0; samples * 2],
                sample_rate: 48000,
                channels: 2,
                num_samples: samples,
                timestamp_ms: i * 512 * 1000 / 48000,
                captured_at: now,
            };
            assert!(!timeline.push(frame, now).discontinuity);
            while timeline.pop_packet().is_some() {}
        }
    }

    #[test]
    fn leftover_samples_keep_their_own_capture_age_not_the_first_callback_age() {
        let now = Instant::now();
        let mut timeline = AudioTimeline::new(48000, 2);
        for i in 0..100 {
            let at = now + Duration::from_millis(i * 11);
            let frame = AudioFrame {
                data: vec![0.0; 512 * 2],
                sample_rate: 48000,
                channels: 2,
                num_samples: 512,
                timestamp_ms: i * 512 * 1000 / 48000,
                captured_at: at,
            };
            let outcome = timeline.push(frame, at);
            assert!(!outcome.stale && !outcome.discontinuity);
            while let Some(packet) = timeline.pop_packet() {
                assert!(
                    at.saturating_duration_since(packet.captured_at) <= Duration::from_millis(22)
                );
            }
        }
    }
}

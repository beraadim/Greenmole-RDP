pub mod converter;
mod stats;
mod timeline;
pub use stats::{AudioStats, AudioStatsSnapshot};

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ironrdp_rdpsnd::pdu::{AudioFormat, WaveFormat};
use ironrdp_rdpsnd::server::RdpsndServerHandler;
use ironrdp_server::{ServerEventSender, ServerEventTx, SoundServerFactory};
use macrdp_capture::{audio_channel, AudioFrameReceiver, AudioFrameSender};

/// Packetize live capture and reset the sample clock after missing capture data.
async fn audio_loop(
    mut audio_rx: AudioFrameReceiver,
    event_sender: ServerEventTx,
    channels: u16,
    sample_rate: u32,
    flow_state: Arc<AudioFlowState>,
    stats: Arc<AudioStats>,
) {
    let mut timeline = timeline::AudioTimeline::new(sample_rate, channels);
    let mut generation = 0;
    let mut last_log = Instant::now();
    let mut last_counts = (0, 0, 0, 0);
    tracing::info!(
        sample_rate,
        channels,
        max_queue_ms = 200,
        "Audio loop started"
    );
    'capture: loop {
        let frame = tokio::select! {
            biased;
            _ = event_sender.closed() => break,
            frame = audio_rx.recv() => match frame {
                Some(frame) => frame,
                None => break,
            },
        };
        let current_generation = flow_state.generation.load(Ordering::Acquire);
        if current_generation != generation {
            timeline.reset();
            generation = current_generation;
        }
        if !flow_state.started.load(Ordering::Acquire) {
            timeline.reset();
            continue;
        }
        let now = Instant::now();
        let outcome = timeline.push(frame, now);
        if outcome.discontinuity {
            stats.discontinuities.fetch_add(1, Ordering::Relaxed);
        }
        if outcome.dropped_partial {
            stats.dropped.fetch_add(1, Ordering::Relaxed);
        }
        if outcome.stale {
            stats.stale.fetch_add(1, Ordering::Relaxed);
        }
        if outcome.invalid {
            stats.invalid.fetch_add(1, Ordering::Relaxed);
        }
        while let Some(packet) = timeline.pop_packet() {
            if let Err(error) = event_sender.send_audio_wave(
                packet.data,
                packet.timestamp_ms,
                packet.duration,
                packet.captured_at,
            ) {
                tracing::debug!(%error, "Audio producer stopped with its session");
                break 'capture;
            }
        }
        // Keep congestion diagnostics useful without logging every callback.
        if now.duration_since(last_log) >= Duration::from_secs(5) {
            let snapshot = stats.snapshot();
            let counts = (
                snapshot.dropped_frames,
                snapshot.stale_frames,
                snapshot.discontinuities,
                snapshot.invalid_frames,
            );
            if counts != last_counts {
                tracing::info!(
                    dropped_frames = snapshot.dropped_frames,
                    stale_frames = snapshot.stale_frames,
                    discontinuities = snapshot.discontinuities,
                    invalid_frames = snapshot.invalid_frames,
                    queue_age_ms = snapshot.queue_age_ms,
                    queued_ms = snapshot.queued_ms,
                    "Audio recovery counters"
                );
                last_counts = counts;
            }
            last_log = now;
        }
    }
    tracing::info!(stats = ?stats.snapshot(), "Audio loop ended");
}

#[derive(Default)]
struct AudioFlowState {
    started: AtomicBool,
    generation: AtomicU64,
}

/// Choose by server preference, returning the index in the client's format list.
/// RDPSND stamps this index onto every wave; returning the server's index can
/// silently select the wrong codec when a client advertises several formats.
pub fn find_matching_format(
    server_formats: &[AudioFormat],
    client_formats: &[AudioFormat],
) -> Option<usize> {
    for server_fmt in server_formats.iter().rev() {
        for (idx, client_fmt) in client_formats.iter().enumerate() {
            if server_fmt.format == client_fmt.format
                && server_fmt.n_samples_per_sec == client_fmt.n_samples_per_sec
                && server_fmt.n_channels == client_fmt.n_channels
                && server_fmt.bits_per_sample == client_fmt.bits_per_sample
            {
                return Some(idx);
            }
        }
    }
    None
}

pub struct MacAudioHandler {
    formats: Vec<AudioFormat>,
    selected_format: Option<usize>,
    flow_state: Arc<AudioFlowState>,
}

impl fmt::Debug for MacAudioHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacAudioHandler")
            .field("selected_format", &self.selected_format)
            .finish_non_exhaustive()
    }
}

impl MacAudioHandler {
    fn new(sample_rate: u32, channels: u16, flow_state: Arc<AudioFlowState>) -> Self {
        let formats = vec![AudioFormat {
            format: WaveFormat::PCM,
            n_channels: channels,
            n_samples_per_sec: sample_rate,
            n_avg_bytes_per_sec: sample_rate * (channels as u32) * 2,
            n_block_align: channels * 2,
            bits_per_sample: 16,
            data: None,
        }];

        Self {
            formats,
            selected_format: None,
            flow_state,
        }
    }
}

impl RdpsndServerHandler for MacAudioHandler {
    fn get_formats(&self) -> &[AudioFormat] {
        &self.formats
    }

    fn start(&mut self, client_format: &ironrdp_rdpsnd::pdu::ClientAudioFormatPdu) -> Option<u16> {
        self.flow_state.started.store(false, Ordering::Release);
        self.flow_state.generation.fetch_add(1, Ordering::AcqRel);
        self.selected_format = None;
        let idx = find_matching_format(&self.formats, &client_format.formats)?;
        let wire_idx = u16::try_from(idx).ok()?;
        self.selected_format = Some(idx);
        self.flow_state.started.store(true, Ordering::Release);
        tracing::info!(format_idx = idx, "Audio started");
        Some(wire_idx)
    }

    fn stop(&mut self) {
        tracing::info!("Audio stopped");
        self.selected_format = None;
        self.flow_state.started.store(false, Ordering::Release);
        self.flow_state.generation.fetch_add(1, Ordering::AcqRel);
    }
}

impl Drop for MacAudioHandler {
    fn drop(&mut self) {
        self.flow_state.started.store(false, Ordering::Release);
        self.flow_state.generation.fetch_add(1, Ordering::AcqRel);
    }
}

/// Shared audio sender that can be swapped on each connection.
/// Display reads the current sender; AudioFactory replaces it with a fresh one per connection.
pub type SharedAudioTx = std::sync::Arc<std::sync::Mutex<Option<AudioFrameSender>>>;

/// Create a shared audio sender slot for use by both MacDisplay and MacAudioFactory.
pub fn new_shared_audio_tx() -> SharedAudioTx {
    std::sync::Arc::new(std::sync::Mutex::new(None))
}

/// Factory that creates MacAudioHandler instances.
/// Creates a fresh audio channel for each connection so the server survives reconnects.
pub struct MacAudioFactory {
    shared_tx: SharedAudioTx,
    stats: Arc<AudioStats>,
    event_sender: Option<ServerEventTx>,
    sample_rate: u32,
    channels: u16,
}

impl MacAudioFactory {
    pub fn new(shared_tx: SharedAudioTx, sample_rate: u32, channels: u16) -> Self {
        Self {
            shared_tx,
            stats: Arc::default(),
            event_sender: None,
            sample_rate,
            channels,
        }
    }

    pub fn with_stats(mut self, stats: Arc<AudioStats>) -> Self {
        self.stats = stats;
        self
    }
}

impl SoundServerFactory for MacAudioFactory {
    fn build_backend(&self) -> Box<dyn RdpsndServerHandler> {
        let sender = self.event_sender.clone().expect("event sender not set");

        let channels = self.channels;
        let sample_rate = self.sample_rate;
        let flow_state = Arc::new(AudioFlowState::default());

        // Create a fresh channel for this connection
        let (tx, rx) = audio_channel(self.stats.capture.clone());
        *self.stats.transport.lock().unwrap() = Some(sender.audio_stats());
        // Store the new sender so the display capturer uses it
        *self.shared_tx.lock().unwrap() = Some(tx);

        tokio::spawn(audio_loop(
            rx,
            sender,
            channels,
            sample_rate,
            Arc::clone(&flow_state),
            self.stats.clone(),
        ));

        Box::new(MacAudioHandler::new(sample_rate, channels, flow_state))
    }
}

impl ServerEventSender for MacAudioFactory {
    fn set_sender(&mut self, sender: ServerEventTx) {
        self.event_sender = Some(sender);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm_48k_stereo() -> AudioFormat {
        AudioFormat {
            format: WaveFormat::PCM,
            n_channels: 2,
            n_samples_per_sec: 48000,
            n_avg_bytes_per_sec: 192000,
            n_block_align: 4,
            bits_per_sample: 16,
            data: None,
        }
    }

    #[test]
    fn match_format_pcm_found() {
        let server_formats = vec![pcm_48k_stereo()];
        let client_formats = vec![pcm_48k_stereo()];
        let result = find_matching_format(&server_formats, &client_formats);
        assert_eq!(result, Some(0));
    }

    #[test]
    fn match_format_no_match() {
        let server_formats = vec![pcm_48k_stereo()];
        let client_formats = vec![AudioFormat {
            format: WaveFormat::PCM,
            n_channels: 1,
            n_samples_per_sec: 44100,
            n_avg_bytes_per_sec: 88200,
            n_block_align: 2,
            bits_per_sample: 16,
            data: None,
        }];
        let result = find_matching_format(&server_formats, &client_formats);
        assert_eq!(result, None);
    }

    #[test]
    fn negotiated_wave_index_addresses_client_order_not_server_order() {
        let mut other = pcm_48k_stereo();
        other.n_samples_per_sec = 44100;
        let client = vec![other.clone(), pcm_48k_stereo()];
        assert_eq!(find_matching_format(&[pcm_48k_stereo()], &client), Some(1));
        // Server preference is still honored even if client order differs.
        assert_eq!(
            find_matching_format(&[pcm_48k_stereo(), other], &client),
            Some(0)
        );
    }
}

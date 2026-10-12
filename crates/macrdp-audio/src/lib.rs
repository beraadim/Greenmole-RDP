pub mod converter;

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ironrdp_rdpsnd::pdu::{AudioFormat, WaveFormat};
use ironrdp_rdpsnd::server::{RdpsndServerHandler, RdpsndServerMessage};
use ironrdp_server::{ServerEvent, ServerEventSender, ServerEventTx, SoundServerFactory};
use macrdp_capture::AudioFrame;
use tokio::sync::mpsc;

/// Audio processing loop: receives AudioFrames from SCK, converts to S16LE,
/// and sends RdpsndServerMessage::Wave events.
async fn audio_loop(
    mut audio_rx: mpsc::Receiver<AudioFrame>,
    event_sender: ServerEventTx,
    frame_size_interleaved: usize,
    channels: u16,
    sample_rate: u32,
    flow_state: Arc<AudioFlowState>,
) {
    let mut ring_buffer: VecDeque<f32> = VecDeque::with_capacity(frame_size_interleaved * 4);
    let mut base_timestamp_ms: Option<u64> = None;
    let mut samples_sent: u64 = 0;
    let mut was_started = false;

    tracing::info!(
        frame_size_interleaved,
        "Audio loop started ({}Hz, {}ch)",
        sample_rate,
        channels
    );

    loop {
        let frame = tokio::select! {
            biased;
            _ = event_sender.closed() => break,
            frame = audio_rx.recv() => match frame {
                Some(frame) => frame,
                None => break,
            },
        };
        let started = flow_state.started.load(Ordering::Relaxed);
        if !started {
            if was_started {
                tracing::debug!("Audio loop pausing until RDPSND start");
            }
            was_started = false;
            ring_buffer.clear();
            base_timestamp_ms = None;
            samples_sent = 0;
            continue;
        }
        if !was_started {
            tracing::debug!("Audio loop activating after RDPSND start");
            was_started = true;
        }

        if base_timestamp_ms.is_none() {
            base_timestamp_ms = Some(frame.timestamp_ms);
        }
        ring_buffer.extend(frame.data.iter());

        while ring_buffer.len() >= frame_size_interleaved {
            let chunk: Vec<f32> = ring_buffer.drain(..frame_size_interleaved).collect();
            let wave_data = converter::AudioConverter::float32_to_s16le(&chunk);

            let samples_per_ch = frame_size_interleaved / channels as usize;
            let offset_ms = (samples_sent * 1000) / sample_rate as u64;
            let ts = base_timestamp_ms.unwrap_or(0) + offset_ms;

            if let Err(error) = event_sender.send(ServerEvent::Rdpsnd(RdpsndServerMessage::Wave(
                wave_data, ts as u32,
            ))) {
                tracing::debug!(%error, "Audio producer stopped with its session");
                return;
            }

            samples_sent += samples_per_ch as u64;
        }
    }
    tracing::info!("Audio loop ended");
}

#[derive(Default)]
struct AudioFlowState {
    started: AtomicBool,
}

/// Find best matching server format index given client's supported formats.
pub fn find_matching_format(
    server_formats: &[AudioFormat],
    client_formats: &[AudioFormat],
) -> Option<usize> {
    for (idx, server_fmt) in server_formats.iter().enumerate().rev() {
        for client_fmt in client_formats {
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
        let idx = find_matching_format(&self.formats, &client_format.formats)?;
        self.selected_format = Some(idx);
        self.flow_state.started.store(true, Ordering::Relaxed);
        tracing::info!(format_idx = idx, "Audio started");
        Some(idx as u16)
    }

    fn stop(&mut self) {
        tracing::info!("Audio stopped");
        self.selected_format = None;
        self.flow_state.started.store(false, Ordering::Relaxed);
    }
}

/// Shared audio sender that can be swapped on each connection.
/// Display reads the current sender; AudioFactory replaces it with a fresh one per connection.
pub type SharedAudioTx = std::sync::Arc<std::sync::Mutex<Option<mpsc::Sender<AudioFrame>>>>;

/// Create a shared audio sender slot for use by both MacDisplay and MacAudioFactory.
pub fn new_shared_audio_tx() -> SharedAudioTx {
    std::sync::Arc::new(std::sync::Mutex::new(None))
}

/// Factory that creates MacAudioHandler instances.
/// Creates a fresh audio channel for each connection so the server survives reconnects.
pub struct MacAudioFactory {
    shared_tx: SharedAudioTx,
    event_sender: Option<ServerEventTx>,
    sample_rate: u32,
    channels: u16,
}

impl MacAudioFactory {
    pub fn new(shared_tx: SharedAudioTx, sample_rate: u32, channels: u16) -> Self {
        Self {
            shared_tx,
            event_sender: None,
            sample_rate,
            channels,
        }
    }
}

impl SoundServerFactory for MacAudioFactory {
    fn build_backend(&self) -> Box<dyn RdpsndServerHandler> {
        let sender = self.event_sender.clone().expect("event sender not set");

        let channels = self.channels;
        let sample_rate = self.sample_rate;
        // 20ms frame: sample_rate/50 samples per channel * channels
        let frame_size_interleaved = (sample_rate as usize / 50) * channels as usize;
        let flow_state = Arc::new(AudioFlowState::default());

        // Create a fresh channel for this connection
        let (tx, rx) = mpsc::channel::<AudioFrame>(32);
        // Store the new sender so the display capturer uses it
        *self.shared_tx.lock().unwrap() = Some(tx);

        tokio::spawn(audio_loop(
            rx,
            sender,
            frame_size_interleaved,
            channels,
            sample_rate,
            Arc::clone(&flow_state),
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
}

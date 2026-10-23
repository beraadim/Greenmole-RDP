# Phase 3: Clipboard Sync + Audio Forwarding — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add bidirectional clipboard synchronization (text, image, file) and audio forwarding (PCM + optional Opus) to the macrdp RDP server.

**Architecture:** Two new crates (`macrdp-audio`, `macrdp-clipboard`) implement RDP channel backends for IronRDP's RDPSND and CLIPRDR SVCs. Audio samples come from ScreenCaptureKit (already used for video). Clipboard access uses NSPasteboard via `objc2`. Both integrate into `macrdp-core` through factory patterns already stubbed in `ironrdp-server-gfx`.

**Tech Stack:** Rust, IronRDP (ironrdp-cliprdr 0.5, ironrdp-rdpsnd 0.7), ScreenCaptureKit (screencapturekit 1.5), objc2/objc2-app-kit, image crate

**Spec:** `docs/superpowers/specs/2026-04-07-clipboard-audio-design.md`

---

## File Map

### New Files

| File | Responsibility |
|------|---------------|
| `crates/macrdp-audio/Cargo.toml` | Audio crate manifest with optional `opus` feature |
| `crates/macrdp-audio/src/lib.rs` | `MacAudioHandler` (RdpsndServerHandler), `MacAudioFactory`, audio processing loop |
| `crates/macrdp-audio/src/converter.rs` | `AudioConverter`: Float32→S16LE, channel interleaving |
| `crates/macrdp-audio/src/opus.rs` | Optional Opus encoder (feature-gated) |
| `crates/macrdp-clipboard/Cargo.toml` | Clipboard crate manifest |
| `crates/macrdp-clipboard/src/lib.rs` | `MacClipboardBackend` (CliprdrBackend), `MacClipboardFactory`, polling thread |
| `crates/macrdp-clipboard/src/pasteboard.rs` | `PasteboardBridge`: NSPasteboard objc2 wrapper |
| `crates/macrdp-clipboard/src/formats.rs` | `FormatConverter`: UTI↔RDP format mapping, UTF-16LE, DIB, HTML Format |
| `crates/macrdp-clipboard/src/file.rs` | File clipboard: FileGroupDescriptorW serialization, FileContents I/O |

### Modified Files

| File | Changes |
|------|---------|
| `Cargo.toml` (root) | Add `macrdp-audio`, `macrdp-clipboard` to workspace members |
| `crates/macrdp-capture/src/lib.rs` | Enable SCK audio, add `AudioFrame` struct, `audio_tx` channel in `OutputHandler` |
| `crates/macrdp-core/src/config.rs` | Add `ClipboardConfig`, `AudioConfig` structs |
| `crates/macrdp-core/src/server.rs` | Wire audio + clipboard factories into `RdpServer` builder |
| `crates/macrdp-core/Cargo.toml` | Add `macrdp-audio`, `macrdp-clipboard` dependencies |

---

## Task 1: Audio Converter Module

**Goal:** Create `macrdp-audio` crate with `AudioConverter` (Float32→S16LE + interleave).

**Files:**
- Create: `crates/macrdp-audio/Cargo.toml`
- Create: `crates/macrdp-audio/src/lib.rs`
- Create: `crates/macrdp-audio/src/converter.rs`
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Create crate directory and Cargo.toml**

```toml
# crates/macrdp-audio/Cargo.toml
[package]
name = "macrdp-audio"
version = "0.1.0"
edition = "2021"

[dependencies]
anyhow = { workspace = true }
tracing = { workspace = true }
tokio = { workspace = true, features = ["sync"] }

[dev-dependencies]
approx = "0.5"

[features]
default = []
opus = ["dep:opus"]

[dependencies.opus]
version = "0.3"
optional = true
```

- [ ] **Step 2: Create lib.rs with module declarations**

```rust
// crates/macrdp-audio/src/lib.rs
pub mod converter;

#[cfg(feature = "opus")]
pub mod opus;
```

- [ ] **Step 3: Write AudioConverter tests**

```rust
// crates/macrdp-audio/src/converter.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float32_to_s16le_silence() {
        let input = vec![0.0f32; 4];
        let output = AudioConverter::float32_to_s16le(&input);
        assert_eq!(output, vec![0u8; 8]); // 4 samples * 2 bytes
    }

    #[test]
    fn float32_to_s16le_max_positive() {
        let input = vec![1.0f32];
        let output = AudioConverter::float32_to_s16le(&input);
        let value = i16::from_le_bytes([output[0], output[1]]);
        assert_eq!(value, 32767);
    }

    #[test]
    fn float32_to_s16le_max_negative() {
        let input = vec![-1.0f32];
        let output = AudioConverter::float32_to_s16le(&input);
        let value = i16::from_le_bytes([output[0], output[1]]);
        assert_eq!(value, -32767);
    }

    #[test]
    fn float32_to_s16le_clamps_overflow() {
        let input = vec![2.0f32, -3.0f32];
        let output = AudioConverter::float32_to_s16le(&input);
        let v0 = i16::from_le_bytes([output[0], output[1]]);
        let v1 = i16::from_le_bytes([output[2], output[3]]);
        assert_eq!(v0, 32767);
        assert_eq!(v1, -32767);
    }

    #[test]
    fn interleave_stereo() {
        let left = [1.0f32, 2.0, 3.0];
        let right = [4.0f32, 5.0, 6.0];
        let result = AudioConverter::interleave(&[&left, &right], 3);
        assert_eq!(result, vec![1.0, 4.0, 2.0, 5.0, 3.0, 6.0]);
    }

    #[test]
    fn interleave_mono() {
        let mono = [1.0f32, 2.0, 3.0];
        let result = AudioConverter::interleave(&[&mono], 3);
        assert_eq!(result, vec![1.0, 2.0, 3.0]);
    }
}
```

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test -p macrdp-audio`
Expected: FAIL — `AudioConverter` not defined.

- [ ] **Step 5: Implement AudioConverter**

```rust
// crates/macrdp-audio/src/converter.rs
pub struct AudioConverter;

impl AudioConverter {
    /// Convert Float32 interleaved PCM to S16LE interleaved PCM bytes.
    /// Clamps input to [-1.0, 1.0] before scaling.
    pub fn float32_to_s16le(input: &[f32]) -> Vec<u8> {
        let mut output = Vec::with_capacity(input.len() * 2);
        for &sample in input {
            let clamped = sample.clamp(-1.0, 1.0);
            let scaled = (clamped * 32767.0) as i16;
            output.extend_from_slice(&scaled.to_le_bytes());
        }
        output
    }

    /// Interleave non-interleaved audio buffers.
    /// Input: separate channel buffers [L0,L1,...], [R0,R1,...]
    /// Output: interleaved [L0,R0, L1,R1, ...]
    pub fn interleave(buffers: &[&[f32]], num_samples: usize) -> Vec<f32> {
        let channels = buffers.len();
        let mut output = Vec::with_capacity(num_samples * channels);
        for i in 0..num_samples {
            for ch in buffers {
                output.push(ch[i]);
            }
        }
        output
    }
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p macrdp-audio`
Expected: All 6 tests PASS.

- [ ] **Step 7: Add to workspace members**

In root `Cargo.toml`, add `"crates/macrdp-audio"` to `workspace.members` array.

- [ ] **Step 8: Verify workspace build**

Run: `cargo build -p macrdp-audio`
Expected: Compiles without errors.

- [ ] **Step 9: Commit**

```bash
git add crates/macrdp-audio/ Cargo.toml Cargo.lock
git commit -m "feat(audio): add macrdp-audio crate with AudioConverter"
```

---

## Task 2: SCK Audio Capture Channel

**Goal:** Enable ScreenCaptureKit audio capture and send `AudioFrame` via channel.

**Files:**
- Modify: `crates/macrdp-capture/src/lib.rs`
- Modify: `crates/macrdp-capture/Cargo.toml`

- [ ] **Step 1: Define AudioFrame struct in macrdp-capture**

Add to `crates/macrdp-capture/src/lib.rs`, near the top with other public types:

```rust
/// Raw audio frame from ScreenCaptureKit audio callback.
/// Data is interleaved Float32 PCM (L0,R0, L1,R1, ...).
#[derive(Clone)]
pub struct AudioFrame {
    pub data: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    pub num_samples: usize,
    pub timestamp_ms: u64,
}
```

- [ ] **Step 2: Add audio_tx to OutputHandler**

The `OutputHandler` struct (around line 90) currently has a frame sender. Add an optional audio sender:

```rust
struct OutputHandler {
    sender: mpsc::Sender<CapturedFrame>,
    audio_sender: Option<mpsc::Sender<AudioFrame>>,
    // ... existing fields ...
}
```

- [ ] **Step 3: Handle Audio type in did_output_sample_buffer**

In `OutputHandler::did_output_sample_buffer` (around line 101), the current code filters `SCStreamOutputType::Screen` only. Add the `Audio` branch:

```rust
SCStreamOutputType::Audio => {
    let Some(ref audio_tx) = self.audio_sender else { return };
    if let Some(audio_buffer_list) = sample.audio_buffer_list() {
        let format_desc = sample.format_description();
        let timestamp = sample.presentation_timestamp();
        let timestamp_ms = (timestamp.value as f64
            / timestamp.timescale as f64 * 1000.0) as u64;

        let channels = format_desc.audio_channel_count() as u16;
        let num_buffers = audio_buffer_list.len();

        let interleaved_data = if num_buffers <= 1 {
            // Single buffer = already interleaved
            let buf = &audio_buffer_list[0];
            let raw = buf.data();
            let samples: &[f32] = unsafe {
                std::slice::from_raw_parts(
                    raw.as_ptr() as *const f32,
                    raw.len() / 4,
                )
            };
            samples.to_vec()
        } else {
            // Multiple buffers = non-interleaved, one per channel
            let channel_bufs: Vec<&[f32]> = audio_buffer_list.iter()
                .map(|buf| {
                    let raw = buf.data();
                    unsafe {
                        std::slice::from_raw_parts(
                            raw.as_ptr() as *const f32,
                            raw.len() / 4,
                        )
                    }
                })
                .collect();
            let n = channel_bufs[0].len();
            // Inline interleave (avoid macrdp-audio dependency in capture)
            let mut out = Vec::with_capacity(n * channel_bufs.len());
            for i in 0..n {
                for ch in &channel_bufs {
                    out.push(ch[i]);
                }
            }
            out
        };

        let num_samples = if channels > 0 {
            interleaved_data.len() / channels as usize
        } else {
            interleaved_data.len()
        };

        let frame = AudioFrame {
            data: interleaved_data,
            sample_rate: format_desc.audio_sample_rate() as u32,
            channels,
            num_samples,
            timestamp_ms,
        };
        let _ = audio_tx.try_send(frame);
    }
}
```

- [ ] **Step 4: Enable audio in SCStreamConfiguration**

In the `ScreenCapturer` constructor where `SCStreamConfiguration` is built (around line 280), add audio config:

```rust
let config = SCStreamConfiguration::new()
    // ... existing video config ...
    .with_captures_audio(true)
    .with_sample_rate(screencapturekit::stream::AudioSampleRate::Rate48000)
    .with_channel_count(screencapturekit::stream::AudioChannelCount::Stereo)
    .with_excludes_current_process_audio(true);
```

- [ ] **Step 5: Accept audio_tx parameter in ScreenCapturer constructor**

Modify `ScreenCapturer::new()` (which is `async fn`) to accept an optional audio sender:

```rust
pub async fn new(config: CaptureConfig, audio_tx: Option<mpsc::Sender<AudioFrame>>) -> Result<Self>
```

Pass `audio_tx` to `OutputHandler`. Update all call sites to pass `None` if no audio is needed.

- [ ] **Step 5b: Register Audio output handler with SCStream**

Currently the code only registers `SCStreamOutputType::Screen` (around line 299):
```rust
stream.add_output_handler(handler, SCStreamOutputType::Screen);
```

Add a second registration for audio if `audio_tx` is `Some`:
```rust
stream.add_output_handler(handler.clone(), SCStreamOutputType::Audio);
```

Note: If `SCStream::add_output_handler` doesn't support the same handler for multiple types, create a separate handler instance for audio with shared `audio_tx`. Verify the screencapturekit 1.5 API during implementation.

- [ ] **Step 6: Build and verify compilation**

Run: `cargo build -p macrdp-capture`
Expected: Compiles. (Unit tests for audio require SCK permission; manual testing later.)

- [ ] **Step 7: Commit**

```bash
git add crates/macrdp-capture/
git commit -m "feat(capture): enable SCK audio capture with AudioFrame output channel"
```

---

## Task 3: MacAudioHandler + Factory

**Goal:** Implement `RdpsndServerHandler` trait and factory, with audio processing loop.

**Files:**
- Modify: `crates/macrdp-audio/src/lib.rs`
- Modify: `crates/macrdp-audio/Cargo.toml`

- [ ] **Step 1: Add dependencies to macrdp-audio**

Update `crates/macrdp-audio/Cargo.toml`:

```toml
[dependencies]
anyhow = { workspace = true }
tracing = { workspace = true }
tokio = { workspace = true, features = ["sync", "rt"] }
ironrdp-server = "0.10"
ironrdp-rdpsnd = "0.7"
macrdp-capture = { path = "../macrdp-capture" }
```

Note: `ironrdp-server` will resolve to our `ironrdp-server-gfx` fork via the workspace `[patch]`.

- [ ] **Step 2: Write format matching test**

Add to `crates/macrdp-audio/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_rdpsnd::pdu::{AudioFormat, WaveFormat};

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
            n_channels: 1,  // mono — doesn't match stereo
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
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p macrdp-audio`
Expected: FAIL — `find_matching_format` not defined.

- [ ] **Step 4: Implement MacAudioHandler, MacAudioFactory, and format matching**

Write the full implementation in `crates/macrdp-audio/src/lib.rs`:

```rust
pub mod converter;
#[cfg(feature = "opus")]
pub mod opus;

use std::collections::VecDeque;
use std::fmt;
use std::sync::Mutex;

use ironrdp_rdpsnd::pdu::{AudioFormat, WaveFormat};
use ironrdp_rdpsnd::server::RdpsndServerHandler;
use ironrdp_server::ServerEvent;
use ironrdp_server::RdpsndServerMessage;
use ironrdp_server::SoundServerFactory;
use ironrdp_server::ServerEventSender;
use macrdp_capture::AudioFrame;
use tokio::sync::mpsc;

use converter::AudioConverter;

/// Find best matching server format index given client's supported formats.
/// Iterates server formats in reverse (higher index = higher priority like Opus).
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
}

impl fmt::Debug for MacAudioHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacAudioHandler")
            .field("selected_format", &self.selected_format)
            .finish_non_exhaustive()
    }
}

impl MacAudioHandler {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        let mut formats = vec![
            AudioFormat {
                format: WaveFormat::PCM,
                n_channels: channels,
                n_samples_per_sec: sample_rate,
                n_avg_bytes_per_sec: sample_rate * (channels as u32) * 2,
                n_block_align: channels * 2,
                bits_per_sample: 16,
                data: None,
            },
        ];

        // TODO: Add Opus format when opus feature is enabled

        Self {
            formats,
            selected_format: None,
        }
    }
}

impl RdpsndServerHandler for MacAudioHandler {
    fn get_formats(&self) -> &[AudioFormat] {
        &self.formats
    }

    fn start(
        &mut self,
        client_format: &ironrdp_rdpsnd::pdu::ClientAudioFormatPdu,
    ) -> Option<u16> {
        let idx = find_matching_format(&self.formats, &client_format.formats)?;
        self.selected_format = Some(idx);
        tracing::info!(format_idx = idx, "Audio started with format {:?}", self.formats[idx].format);
        Some(idx as u16)
    }

    fn stop(&mut self) {
        tracing::info!("Audio stopped");
        self.selected_format = None;
    }
}

/// Factory that creates MacAudioHandler instances.
/// Holds audio_rx in Mutex for interior mutability (build_backend takes &self).
pub struct MacAudioFactory {
    audio_rx: Mutex<Option<mpsc::Receiver<AudioFrame>>>,
    event_sender: Option<tokio::sync::mpsc::UnboundedSender<ServerEvent>>,
    sample_rate: u32,
    channels: u16,
}

impl MacAudioFactory {
    pub fn new(
        audio_rx: mpsc::Receiver<AudioFrame>,
        sample_rate: u32,
        channels: u16,
    ) -> Self {
        Self {
            audio_rx: Mutex::new(Some(audio_rx)),
            event_sender: None,
            sample_rate,
            channels,
        }
    }
}

impl SoundServerFactory for MacAudioFactory {
    fn build_backend(&self) -> Box<dyn RdpsndServerHandler> {
        // audio_rx is taken once per connection.
        // The Mutex allows interior mutability since build_backend takes &self.
        let _rx = self.audio_rx.lock().unwrap().take()
            .expect("audio_rx already taken — build_backend called more than once");

        // TODO Task 4: spawn audio_loop with _rx and event_sender

        Box::new(MacAudioHandler::new(self.sample_rate, self.channels))
    }
}

impl ServerEventSender for MacAudioFactory {
    fn set_sender(&mut self, sender: tokio::sync::mpsc::UnboundedSender<ServerEvent>) {
        self.event_sender = Some(sender);
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p macrdp-audio`
Expected: All tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/macrdp-audio/
git commit -m "feat(audio): implement MacAudioHandler with RdpsndServerHandler trait"
```

---

## Task 4: Audio Processing Loop + Core Integration

**Goal:** Wire audio processing loop (Float32→S16LE→Wave) and integrate into macrdp-core.

**Files:**
- Modify: `crates/macrdp-audio/src/lib.rs` (add audio_loop)
- Modify: `crates/macrdp-core/src/config.rs` (add AudioConfig)
- Modify: `crates/macrdp-core/src/server.rs` (wire factory into builder)
- Modify: `crates/macrdp-core/Cargo.toml` (add macrdp-audio dep)

- [ ] **Step 1: Implement audio_loop function**

Add to `crates/macrdp-audio/src/lib.rs`:

```rust
/// Audio processing loop: receives AudioFrames from SCK, converts to S16LE,
/// and sends RdpsndServerMessage::Wave events.
async fn audio_loop(
    mut audio_rx: mpsc::Receiver<AudioFrame>,
    event_sender: tokio::sync::mpsc::UnboundedSender<ServerEvent>,
    frame_size_interleaved: usize,
    channels: u16,
    sample_rate: u32,
) {
    let mut ring_buffer: VecDeque<f32> = VecDeque::with_capacity(frame_size_interleaved * 4);
    let mut base_timestamp_ms: Option<u64> = None;
    let mut samples_sent: u64 = 0;

    tracing::info!(
        frame_size_interleaved,
        "Audio loop started ({}Hz, {}ch)",
        sample_rate,
        channels
    );

    while let Some(frame) = audio_rx.recv().await {
        if base_timestamp_ms.is_none() {
            base_timestamp_ms = Some(frame.timestamp_ms);
        }
        ring_buffer.extend(frame.data.iter());

        while ring_buffer.len() >= frame_size_interleaved {
            let chunk: Vec<f32> = ring_buffer.drain(..frame_size_interleaved).collect();
            let wave_data = AudioConverter::float32_to_s16le(&chunk);

            let samples_per_ch = frame_size_interleaved / channels as usize;
            let offset_ms = (samples_sent * 1000) / sample_rate as u64;
            let ts = base_timestamp_ms.unwrap_or(0) + offset_ms;

            let _ = event_sender.send(ServerEvent::Rdpsnd(
                RdpsndServerMessage::Wave(wave_data, ts as u32),
            ));

            samples_sent += samples_per_ch as u64;
        }
    }
    tracing::info!("Audio loop ended");
}
```

- [ ] **Step 2: Update MacAudioFactory::build_backend to spawn audio_loop**

Replace the TODO in `build_backend()`:

```rust
impl SoundServerFactory for MacAudioFactory {
    fn build_backend(&self) -> Box<dyn RdpsndServerHandler> {
        let rx = self.audio_rx.lock().unwrap().take()
            .expect("audio_rx already taken");
        let sender = self.event_sender.clone()
            .expect("event sender not set");

        let channels = self.channels;
        let sample_rate = self.sample_rate;
        // 20ms frame: 48000 * 0.020 = 960 samples/ch * 2 channels = 1920 interleaved
        let frame_size_interleaved = (sample_rate as usize / 50) * channels as usize;

        tokio::spawn(audio_loop(rx, sender, frame_size_interleaved, channels, sample_rate));

        Box::new(MacAudioHandler::new(sample_rate, channels))
    }
}
```

- [ ] **Step 3: Add AudioConfig to macrdp-core config**

In `crates/macrdp-core/src/config.rs`, add:

```rust
#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct AudioConfig {
    pub enabled: bool,
    pub sample_rate: u32,
    pub channels: u16,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sample_rate: 48000,
            channels: 2,
        }
    }
}
```

Add `pub audio: AudioConfig` field to the main `ServerConfig` struct (with `#[serde(default)]`).

- [ ] **Step 4: Add macrdp-audio dependency to macrdp-core**

In `crates/macrdp-core/Cargo.toml`:

```toml
macrdp-audio = { path = "../macrdp-audio" }
```

- [ ] **Step 5: Wire audio factory into server builder**

In `crates/macrdp-core/src/server.rs`, in the `run_server_thread()` function (around line 332), before `RdpServer::builder()`:

```rust
// Audio setup
let (audio_tx, audio_rx) = if config.audio.enabled {
    let (tx, rx) = tokio::sync::mpsc::channel::<macrdp_capture::AudioFrame>(32);
    (Some(tx), Some(rx))
} else {
    (None, None)
};

// Pass audio_tx to ScreenCapturer constructor
// (update the existing ScreenCapturer::new() call to include audio_tx)

// Create audio factory
let sound_factory: Option<Box<dyn ironrdp_server::SoundServerFactory>> =
    if let Some(rx) = audio_rx {
        let mut factory = macrdp_audio::MacAudioFactory::new(
            rx,
            config.audio.sample_rate,
            config.audio.channels,
        );
        Some(Box::new(factory))
    } else {
        None
    };
```

Then add to the builder chain:

```rust
.with_sound_factory(sound_factory)
```

- [ ] **Step 6: Build and verify**

Run: `cargo build -p macrdp-core`
Expected: Compiles without errors.

- [ ] **Step 7: Commit**

```bash
git add crates/macrdp-audio/ crates/macrdp-core/ crates/macrdp-capture/
git commit -m "feat(audio): wire audio processing loop and integrate into macrdp-core"
```

---

## Task 5: Clipboard Crate + PasteboardBridge

**Goal:** Create `macrdp-clipboard` crate with NSPasteboard wrapper using objc2.

**Files:**
- Create: `crates/macrdp-clipboard/Cargo.toml`
- Create: `crates/macrdp-clipboard/src/lib.rs`
- Create: `crates/macrdp-clipboard/src/pasteboard.rs`
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Create Cargo.toml**

```toml
# crates/macrdp-clipboard/Cargo.toml
[package]
name = "macrdp-clipboard"
version = "0.1.0"
edition = "2021"

[dependencies]
objc2 = "0.6"
objc2-foundation = { version = "0.3", features = [
    "NSString", "NSArray", "NSURL", "NSData", "NSObject",
] }
objc2-app-kit = { version = "0.3", features = ["NSPasteboard"] }
image = { version = "0.25", default-features = false, features = ["png", "tiff"] }
anyhow = { workspace = true }
tracing = { workspace = true }
tokio = { workspace = true, features = ["sync"] }
ironrdp-server = "0.10"
ironrdp-cliprdr = "0.5"
ironrdp-core = "0.1"
```

> **Note:** `ironrdp-core` is required for `AsAny` trait and `impl_as_any!` macro, which `CliprdrBackend` requires.

- [ ] **Step 2: Add to workspace members**

In root `Cargo.toml`, add `"crates/macrdp-clipboard"` to `workspace.members`.

- [ ] **Step 3: Create lib.rs with module declarations**

```rust
// crates/macrdp-clipboard/src/lib.rs
pub mod pasteboard;
pub mod formats;
pub mod file;
```

- [ ] **Step 4: Implement PasteboardBridge**

```rust
// crates/macrdp-clipboard/src/pasteboard.rs
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2_app_kit::NSPasteboard;
use objc2_foundation::{NSArray, NSString};

/// Safe wrapper around NSPasteboard.generalPasteboard.
pub struct PasteboardBridge {
    pasteboard: Retained<NSPasteboard>,
}

// NSPasteboard is not Send by default, but we access it from a dedicated thread
// with a RunLoop. This is safe as long as we only access from that thread.
unsafe impl Send for PasteboardBridge {}

impl PasteboardBridge {
    pub fn new() -> Self {
        let pasteboard = unsafe { NSPasteboard::generalPasteboard() };
        Self { pasteboard }
    }

    /// Returns the current change count. Each write increments this.
    pub fn change_count(&self) -> i64 {
        unsafe { self.pasteboard.changeCount() as i64 }
    }

    /// List available UTI type identifiers on the pasteboard.
    pub fn available_types(&self) -> Vec<String> {
        unsafe {
            let types = self.pasteboard.types();
            match types {
                Some(types) => {
                    let count = types.count();
                    (0..count)
                        .map(|i| types.objectAtIndex(i).to_string())
                        .collect()
                }
                None => vec![],
            }
        }
    }

    /// Read UTF-8 string from pasteboard.
    pub fn read_string(&self) -> Option<String> {
        unsafe {
            let ns_type = NSString::from_str("public.utf8-plain-text");
            self.pasteboard
                .stringForType(&ns_type)
                .map(|s| s.to_string())
        }
    }

    /// Read image data as PNG bytes from pasteboard.
    pub fn read_image(&self) -> Option<Vec<u8>> {
        unsafe {
            // Try PNG first, then TIFF
            for type_str in &["public.png", "public.tiff"] {
                let ns_type = NSString::from_str(type_str);
                if let Some(data) = self.pasteboard.dataForType(&ns_type) {
                    return Some(data.bytes().to_vec());
                }
            }
            None
        }
    }

    /// Read file URLs from pasteboard.
    pub fn read_file_urls(&self) -> Vec<PathBuf> {
        unsafe {
            let ns_type = NSString::from_str("public.file-url");
            // Read property list of file URLs
            let items = self.pasteboard.pasteboardItems();
            match items {
                Some(items) => {
                    let count = items.count();
                    let mut paths = Vec::new();
                    for i in 0..count {
                        let item = items.objectAtIndex(i);
                        if let Some(url_str) = item.stringForType(&ns_type) {
                            let url_string = url_str.to_string();
                            // Convert file:// URL to path
                            if let Some(path) = url_string.strip_prefix("file://") {
                                // URL-decode the path
                                let decoded = percent_decode(path);
                                paths.push(PathBuf::from(decoded));
                            }
                        }
                    }
                    paths
                }
                None => vec![],
            }
        }
    }

    /// Write UTF-8 string to pasteboard. Returns new change count.
    pub fn write_string(&self, text: &str) -> i64 {
        unsafe {
            self.pasteboard.clearContents();
            let ns_string = NSString::from_str(text);
            let ns_type = NSString::from_str("public.utf8-plain-text");
            self.pasteboard.setString_forType(&ns_string, &ns_type);
            self.pasteboard.changeCount() as i64
        }
    }

    /// Write PNG image data to pasteboard. Returns new change count.
    pub fn write_image(&self, png_data: &[u8]) -> i64 {
        unsafe {
            self.pasteboard.clearContents();
            let ns_data = objc2_foundation::NSData::from_vec(png_data.to_vec());
            let ns_type = NSString::from_str("public.png");
            self.pasteboard.setData_forType(&ns_data, &ns_type);
            self.pasteboard.changeCount() as i64
        }
    }

    /// Write file URL references to pasteboard. Returns new change count.
    /// Uses writeObjects with NSURL array to properly handle multiple files.
    pub fn write_file_urls(&self, paths: &[PathBuf]) -> i64 {
        unsafe {
            self.pasteboard.clearContents();
            // For multiple files, use NSPasteboardItem per file or
            // writeObjects with an array of NSURL objects.
            // Simple approach: write each as a separate pasteboard item.
            // NOTE: The exact objc2 API for writeObjects may differ.
            // During implementation, verify and use the correct method
            // (e.g., pasteboard.writeObjects with NSArray<NSURL>).
            for path in paths {
                let url_str = format!("file://{}", path.display());
                let ns_string = NSString::from_str(&url_str);
                let ns_type = NSString::from_str("public.file-url");
                // TODO: Use NSPasteboardItem per file for multi-file support
                self.pasteboard.setString_forType(&ns_string, &ns_type);
            }
            self.pasteboard.changeCount() as i64
        }
    }

    /// Clear pasteboard contents.
    pub fn clear(&self) -> i64 {
        unsafe {
            self.pasteboard.clearContents();
            self.pasteboard.changeCount() as i64
        }
    }
}

fn percent_decode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let h = chars.next().unwrap_or(b'0');
            let l = chars.next().unwrap_or(b'0');
            let decoded = u8::from_str_radix(
                &format!("{}{}", h as char, l as char), 16
            ).unwrap_or(b'?');
            result.push(decoded as char);
        } else {
            result.push(b as char);
        }
    }
    result
}
```

- [ ] **Step 5: Create empty module stubs**

```rust
// crates/macrdp-clipboard/src/formats.rs
// Format conversion: macOS UTI <-> RDP ClipboardFormat
// Implemented in Tasks 6 and 9.

// crates/macrdp-clipboard/src/file.rs
// File clipboard: FileGroupDescriptorW + FileContents
// Implemented in Task 11.
```

- [ ] **Step 6: Build and verify**

Run: `cargo build -p macrdp-clipboard`
Expected: Compiles. (objc2 requires macOS, which we're on.)

- [ ] **Step 7: Commit**

```bash
git add crates/macrdp-clipboard/ Cargo.toml Cargo.lock
git commit -m "feat(clipboard): add macrdp-clipboard crate with PasteboardBridge"
```

---

## Task 6: Text Format Converter

**Goal:** Implement UTF-8 ↔ UTF-16LE conversion and UTI↔RDP format mapping for text.

**Files:**
- Modify: `crates/macrdp-clipboard/src/formats.rs`

- [ ] **Step 1: Write text conversion tests**

```rust
// crates/macrdp-clipboard/src/formats.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_to_utf16le_ascii() {
        let result = utf8_to_utf16le("hello");
        // "hello" + null terminator = 6 UTF-16 code units = 12 bytes
        assert_eq!(result.len(), 12);
        // 'h' = 0x0068 LE → [0x68, 0x00]
        assert_eq!(result[0], 0x68);
        assert_eq!(result[1], 0x00);
        // null terminator
        assert_eq!(result[10], 0x00);
        assert_eq!(result[11], 0x00);
    }

    #[test]
    fn utf8_to_utf16le_chinese() {
        let result = utf8_to_utf16le("\u{4F60}\u{597D}");
        // 2 chars + null = 3 code units = 6 bytes
        assert_eq!(result.len(), 6);
        // U+4F60 uses little-endian bytes [0x60, 0x4F]
        assert_eq!(result[0], 0x60);
        assert_eq!(result[1], 0x4F);
    }

    #[test]
    fn utf16le_to_utf8_roundtrip() {
        let original = "Hello \u{4E16}\u{754C}! 🎉";
        let encoded = utf8_to_utf16le(original);
        let decoded = utf16le_to_utf8(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn utf16le_to_utf8_empty() {
        let result = utf16le_to_utf8(&[]);
        assert_eq!(result, Some(String::new()));
    }

    #[test]
    fn uti_to_rdp_text() {
        let rdp = uti_to_rdp_format("public.utf8-plain-text");
        assert!(rdp.is_some());
    }

    #[test]
    fn uti_to_rdp_html() {
        let rdp = uti_to_rdp_format("public.html");
        assert!(rdp.is_some());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p macrdp-clipboard`
Expected: FAIL.

- [ ] **Step 3: Implement format conversion functions**

```rust
// crates/macrdp-clipboard/src/formats.rs
use ironrdp_cliprdr::pdu::{ClipboardFormat, ClipboardFormatId, ClipboardFormatName};

/// Convert UTF-8 string to UTF-16LE bytes with null terminator.
pub fn utf8_to_utf16le(s: &str) -> Vec<u8> {
    let utf16: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    utf16.iter().flat_map(|&w| w.to_le_bytes()).collect()
}

/// Convert UTF-16LE bytes to UTF-8 string, stripping null terminator.
pub fn utf16le_to_utf8(data: &[u8]) -> Option<String> {
    if data.is_empty() {
        return Some(String::new());
    }
    let words: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&words)
        .ok()
        .map(|s| s.trim_end_matches('\0').to_string())
}

/// Map macOS UTI to RDP ClipboardFormat.
/// Uses ironrdp-cliprdr's predefined ClipboardFormatId constants.
pub fn uti_to_rdp_format(uti: &str) -> Option<ClipboardFormat> {
    match uti {
        "public.utf8-plain-text" | "public.plain-text" => {
            Some(ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT))
        }
        "public.png" | "public.tiff" | "public.jpeg" => {
            Some(ClipboardFormat::new(ClipboardFormatId::CF_DIB))
        }
        "public.html" => {
            // HTML Format is a registered format name in RDP
            Some(ClipboardFormat::new(ClipboardFormatId::new(0))
                .with_name(ClipboardFormatName::new("HTML Format")))
        }
        "public.file-url" => {
            // File clipboard uses registered format name "FileGroupDescriptorW"
            Some(ClipboardFormat::new(ClipboardFormatId::new(0))
                .with_name(ClipboardFormatName::new("FileGroupDescriptorW")))
        }
        _ => None,
    }
}

/// Map RDP format ID to macOS UTI.
pub fn rdp_format_to_uti(format_id: ClipboardFormatId) -> Option<&'static str> {
    if format_id == ClipboardFormatId::CF_UNICODETEXT {
        Some("public.utf8-plain-text")
    } else if format_id == ClipboardFormatId::CF_DIB {
        Some("public.png")
    } else {
        None
    }
}
```

> **Note:** Use `ClipboardFormatId::CF_UNICODETEXT`, `ClipboardFormatId::CF_DIB` etc. from ironrdp-cliprdr's predefined constants. For registered formats (HTML Format, FileGroupDescriptorW), use `ClipboardFormatId::new(0)` with `.with_name()`. Verify exact API methods during implementation — the `with_name()` method may differ.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p macrdp-clipboard`
Expected: All 5 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/macrdp-clipboard/src/formats.rs
git commit -m "feat(clipboard): implement text format converter (UTF-8 <-> UTF-16LE)"
```

---

## Task 7: MacClipboardBackend (Text) + Polling

**Goal:** Implement `CliprdrBackend` trait for text clipboard with polling thread and anti-echo.

**Files:**
- Modify: `crates/macrdp-clipboard/src/lib.rs`

- [ ] **Step 1: Implement MacClipboardBackend struct**

```rust
// crates/macrdp-clipboard/src/lib.rs
pub mod pasteboard;
pub mod formats;
pub mod file;

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ironrdp_cliprdr::backend::CliprdrBackend;
use ironrdp_cliprdr::pdu::*;
use ironrdp_core::impl_as_any;
use ironrdp_server::{CliprdrServerFactory, ServerEvent, ServerEventSender};
use ironrdp_server::CliprdrBackendFactory;
use tokio::sync::mpsc;

use crate::formats::*;
use crate::pasteboard::PasteboardBridge;

pub struct MacClipboardBackend {
    pasteboard: PasteboardBridge,
    event_sender: mpsc::UnboundedSender<ServerEvent>,
    last_change_count: Arc<AtomicI64>,
    remote_formats: Mutex<Vec<ClipboardFormat>>,
    file_handles: Mutex<HashMap<u32, File>>,
    temp_dir: PathBuf,
    poll_handle: Option<JoinHandle<()>>,
    stop_signal: Arc<AtomicBool>,
    locked: AtomicBool,
}

impl fmt::Debug for MacClipboardBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacClipboardBackend")
            .field("temp_dir", &self.temp_dir)
            .finish_non_exhaustive()
    }
}

// Required by CliprdrBackend: AsAny + Debug + Send
impl_as_any!(MacClipboardBackend);

impl MacClipboardBackend {
    pub fn new(
        event_sender: mpsc::UnboundedSender<ServerEvent>,
        temp_dir: PathBuf,
    ) -> Self {
        std::fs::create_dir_all(&temp_dir).ok();
        Self {
            pasteboard: PasteboardBridge::new(),
            event_sender,
            last_change_count: Arc::new(AtomicI64::new(0)),
            remote_formats: Mutex::new(Vec::new()),
            file_handles: Mutex::new(HashMap::new()),
            temp_dir,
            poll_handle: None,
            stop_signal: Arc::new(AtomicBool::new(false)),
            locked: AtomicBool::new(false),
        }
    }
}
```

- [ ] **Step 2: Implement CliprdrBackend trait**

The exact trait methods depend on ironrdp-cliprdr 0.5.0's `CliprdrBackend`. Implement all required methods. Key ones for text:

```rust
impl CliprdrBackend for MacClipboardBackend {
    fn temporary_directory(&self) -> &str {
        self.temp_dir.to_str().unwrap_or("/tmp/macrdp-clipboard")
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
    }

    fn on_ready(&mut self) {
        // Start polling thread
        let last = self.last_change_count.clone();
        let sender = self.event_sender.clone();
        let stop = self.stop_signal.clone();

        // Initialize change count
        let initial = self.pasteboard.change_count();
        last.store(initial, Ordering::SeqCst);

        self.poll_handle = Some(std::thread::spawn(move || {
            // NSPasteboard requires a thread with a RunLoop.
            // We create a thread-local PasteboardBridge and use
            // CFRunLoopRunInMode for proper Cocoa event dispatch.
            // Alternatively, wrap in an @autoreleasepool equivalent.
            let bridge = PasteboardBridge::new(); // Thread-local instance
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
                if stop.load(Ordering::Relaxed) { break; }

                let current = bridge.change_count();
                let prev = last.swap(current, Ordering::SeqCst);
                if current != prev {
                    let formats: Vec<ClipboardFormat> = bridge
                        .available_types()
                        .iter()
                        .filter_map(|uti| uti_to_rdp_format(uti))
                        .collect();
                    if !formats.is_empty() {
                        let _ = sender.send(ServerEvent::Clipboard(
                            ClipboardMessage::SendInitiateCopy(formats),
                        ));
                    }
                }
            }
        }));
        tracing::info!("Clipboard polling started");
    }

    fn on_process_negotiated_capabilities(
        &mut self,
        _capabilities: ClipboardGeneralCapabilityFlags,
    ) {
        // Store if needed for feature gating
    }

    fn on_request_format_list(&mut self) {
        let formats: Vec<ClipboardFormat> = self
            .pasteboard
            .available_types()
            .iter()
            .filter_map(|uti| uti_to_rdp_format(uti))
            .collect();
        let _ = self.event_sender.send(ServerEvent::Clipboard(
            ClipboardMessage::SendInitiateCopy(formats),
        ));
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        *self.remote_formats.lock().unwrap() = available_formats.to_vec();
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        // request.format is ClipboardFormatId, not u32
        let format = request.format;
        let data = if format == ClipboardFormatId::CF_UNICODETEXT {
            self.pasteboard
                .read_string()
                .map(|s| utf8_to_utf16le(&s))
                .unwrap_or_default()
        } else if format == ClipboardFormatId::CF_DIB {
            // Added in Task 10
            Vec::new()
        } else {
            tracing::warn!(?format, "Unsupported clipboard format requested");
            Vec::new()
        };

        // Construct response using FormatDataResponse API
        // OwnedFormatDataResponse is created via FormatDataResponse::new_data().into_owned()
        let response = OwnedFormatDataResponse::new_data(data);
        let _ = self.event_sender.send(ServerEvent::Clipboard(
            ClipboardMessage::SendFormatData(response),
        ));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        let data = response.data();

        // Security: enforce max_data_size_mb limit
        // (max_data_size from config, checked before processing)
        // if data.len() > self.max_data_size { return; }

        // Client sent clipboard data → write to NSPasteboard
        // Determine format from the most recently requested format.
        // Track the last requested format_id to know what we're receiving.

        // Try DIB (image) first — check for BITMAPINFOHEADER
        if data.len() >= 40 {
            let header_size = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            if header_size == 40 {
                if let Ok(png_data) = dib_to_png(data) {
                    let new_count = self.pasteboard.write_image(&png_data);
                    self.last_change_count.store(new_count, Ordering::SeqCst);
                    return;
                }
            }
        }

        // Fallback: try as text (UTF-16LE)
        if let Some(text) = utf16le_to_utf8(data) {
            let new_count = self.pasteboard.write_string(&text);
            // Anti-echo: update change count to skip next poll
            self.last_change_count.store(new_count, Ordering::SeqCst);
        }
    }

    // NOTE: For Client→macOS paste (SendInitiatePaste flow):
    // When on_remote_copy() is called with client's format list, we store the
    // formats. When a macOS app triggers paste, we need a mechanism to request
    // client data. This is handled by the server's event loop:
    // 1. on_remote_copy() stores formats
    // 2. When macOS app tries to paste, the polling thread detects no local
    //    clipboard change, but the CliprdrServer's internal state machine
    //    handles the paste request automatically via the SVC channel.
    // The lazy-fetch mechanism may need a custom NSPasteboard provider
    // (NSPasteboardWriting) that triggers SendInitiatePaste on demand.
    // This is complex and should be refined during implementation.

    fn on_file_contents_request(&mut self, _request: FileContentsRequest) {
        // Implemented in Task 11
    }

    fn on_file_contents_response(&mut self, _response: FileContentsResponse<'_>) {
        // Implemented in Task 11
    }

    fn on_lock(&mut self, _data_id: LockDataId) {
        self.locked.store(true, Ordering::SeqCst);
    }

    fn on_unlock(&mut self, _data_id: LockDataId) {
        self.locked.store(false, Ordering::SeqCst);
        self.file_handles.lock().unwrap().clear();
    }
}

impl Drop for MacClipboardBackend {
    fn drop(&mut self) {
        self.stop_signal.store(true, Ordering::SeqCst);
        if let Some(handle) = self.poll_handle.take() {
            let _ = handle.join();
        }
    }
}
```

- [ ] **Step 3: Implement MacClipboardFactory**

```rust
pub struct MacClipboardFactory {
    event_sender: Option<mpsc::UnboundedSender<ServerEvent>>,
    temp_dir: PathBuf,
}

impl MacClipboardFactory {
    pub fn new(temp_dir: PathBuf) -> Self {
        Self {
            event_sender: None,
            temp_dir,
        }
    }
}

impl CliprdrBackendFactory for MacClipboardFactory {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        Box::new(MacClipboardBackend::new(
            self.event_sender.clone().expect("event sender not set"),
            self.temp_dir.clone(),
        ))
    }
}

impl ServerEventSender for MacClipboardFactory {
    fn set_sender(&mut self, sender: mpsc::UnboundedSender<ServerEvent>) {
        self.event_sender = Some(sender);
    }
}

impl CliprdrServerFactory for MacClipboardFactory {}
```

- [ ] **Step 4: Build and verify**

Run: `cargo build -p macrdp-clipboard`
Expected: Compiles. Note: some trait method signatures may need adjustment based on exact ironrdp-cliprdr 0.5.0 API. Adapt as needed during implementation.

- [ ] **Step 5: Commit**

```bash
git add crates/macrdp-clipboard/
git commit -m "feat(clipboard): implement MacClipboardBackend with text support and polling"
```

---

## Task 8: Clipboard Integration in macrdp-core

**Goal:** Wire clipboard factory into macrdp-core server builder and config.

**Files:**
- Modify: `crates/macrdp-core/src/config.rs`
- Modify: `crates/macrdp-core/src/server.rs`
- Modify: `crates/macrdp-core/Cargo.toml`

- [ ] **Step 1: Add ClipboardConfig**

In `crates/macrdp-core/src/config.rs`:

```rust
#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct ClipboardConfig {
    pub enabled: bool,
    pub file_transfer: bool,
    pub max_file_size_mb: u32,
    pub max_data_size_mb: u32,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            file_transfer: true,
            max_file_size_mb: 100,
            max_data_size_mb: 50,
        }
    }
}
```

Add `pub clipboard: ClipboardConfig` to `ServerConfig`.

- [ ] **Step 2: Add macrdp-clipboard dependency**

In `crates/macrdp-core/Cargo.toml`:

```toml
macrdp-clipboard = { path = "../macrdp-clipboard" }
```

- [ ] **Step 3: Wire clipboard factory into server builder**

In `crates/macrdp-core/src/server.rs`, in `run_server_thread()`:

```rust
// Clipboard factory
let cliprdr_factory: Option<Box<dyn ironrdp_server::CliprdrServerFactory>> =
    if config.clipboard.enabled {
        let temp_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("macrdp")
            .join("clipboard");
        Some(Box::new(macrdp_clipboard::MacClipboardFactory::new(temp_dir)))
    } else {
        None
    };
```

Add to builder chain:

```rust
.with_cliprdr_factory(cliprdr_factory)
```

- [ ] **Step 4: Build and verify**

Run: `cargo build -p macrdp-core`
Expected: Compiles.

- [ ] **Step 5: Commit**

```bash
git add crates/macrdp-core/
git commit -m "feat(clipboard): integrate clipboard factory into macrdp-core"
```

---

## Task 9: Image Format Converter (DIB ↔ PNG)

**Goal:** Implement CF_DIB ↔ PNG/TIFF image format conversion.

**Files:**
- Modify: `crates/macrdp-clipboard/src/formats.rs`

- [ ] **Step 1: Write DIB conversion tests**

```rust
// Add to formats.rs tests module

#[test]
fn png_to_dib_and_back() {
    // Create a small 2x2 red PNG in memory
    let mut img = image::RgbaImage::new(2, 2);
    img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
    img.put_pixel(1, 0, image::Rgba([0, 255, 0, 255]));
    img.put_pixel(0, 1, image::Rgba([0, 0, 255, 255]));
    img.put_pixel(1, 1, image::Rgba([255, 255, 255, 255]));

    let mut png_bytes = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png_bytes), image::ImageFormat::Png).unwrap();

    // Convert PNG → DIB
    let dib = png_to_dib(&png_bytes).unwrap();
    assert!(dib.len() > 40); // At least BITMAPINFOHEADER

    // Verify BITMAPINFOHEADER
    let width = i32::from_le_bytes([dib[4], dib[5], dib[6], dib[7]]);
    let height = i32::from_le_bytes([dib[8], dib[9], dib[10], dib[11]]);
    assert_eq!(width, 2);
    assert!(height > 0); // Positive = bottom-up

    // Convert DIB → PNG
    let png_back = dib_to_png(&dib).unwrap();
    assert!(!png_back.is_empty());
}

#[test]
fn dib_header_size() {
    assert_eq!(BITMAPINFOHEADER_SIZE, 40);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p macrdp-clipboard`
Expected: FAIL.

- [ ] **Step 3: Implement DIB conversion**

Add to `crates/macrdp-clipboard/src/formats.rs`:

```rust
use image::{DynamicImage, RgbaImage, ImageFormat};
use std::io::Cursor;

pub const BITMAPINFOHEADER_SIZE: usize = 40;

/// Convert PNG/TIFF image bytes to Windows CF_DIB format.
/// DIB = BITMAPINFOHEADER (40 bytes) + BGRA pixel data (bottom-up row order).
pub fn png_to_dib(image_data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let img = image::load_from_memory(image_data)?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();

    let row_size = (width * 4) as usize; // BGRA, always 4-byte aligned at 32bpp
    let pixel_data_size = row_size * height as usize;

    let mut dib = Vec::with_capacity(BITMAPINFOHEADER_SIZE + pixel_data_size);

    // BITMAPINFOHEADER (40 bytes)
    dib.extend_from_slice(&40u32.to_le_bytes());           // biSize
    dib.extend_from_slice(&(width as i32).to_le_bytes());  // biWidth
    dib.extend_from_slice(&(height as i32).to_le_bytes()); // biHeight (positive = bottom-up)
    dib.extend_from_slice(&1u16.to_le_bytes());            // biPlanes
    dib.extend_from_slice(&32u16.to_le_bytes());           // biBitCount (BGRA)
    dib.extend_from_slice(&0u32.to_le_bytes());            // biCompression (BI_RGB)
    dib.extend_from_slice(&(pixel_data_size as u32).to_le_bytes()); // biSizeImage
    dib.extend_from_slice(&0i32.to_le_bytes());            // biXPelsPerMeter
    dib.extend_from_slice(&0i32.to_le_bytes());            // biYPelsPerMeter
    dib.extend_from_slice(&0u32.to_le_bytes());            // biClrUsed
    dib.extend_from_slice(&0u32.to_le_bytes());            // biClrImportant

    // Pixel data: RGBA (top-down) → BGRA (bottom-up)
    // Flip row order and swap R↔B channels
    for y in (0..height).rev() {
        for x in 0..width {
            let pixel = rgba.get_pixel(x, y);
            dib.push(pixel[2]); // B
            dib.push(pixel[1]); // G
            dib.push(pixel[0]); // R
            dib.push(pixel[3]); // A
        }
    }

    Ok(dib)
}

/// Convert Windows CF_DIB format to PNG bytes.
pub fn dib_to_png(dib: &[u8]) -> anyhow::Result<Vec<u8>> {
    if dib.len() < BITMAPINFOHEADER_SIZE {
        anyhow::bail!("DIB data too small for BITMAPINFOHEADER");
    }

    let width = i32::from_le_bytes([dib[4], dib[5], dib[6], dib[7]]) as u32;
    let height_raw = i32::from_le_bytes([dib[8], dib[9], dib[10], dib[11]]);
    let bottom_up = height_raw > 0;
    let height = height_raw.unsigned_abs();
    let bits_per_pixel = u16::from_le_bytes([dib[14], dib[15]]);

    let bytes_per_pixel = (bits_per_pixel / 8) as usize;
    let row_stride = ((width as usize * bytes_per_pixel + 3) / 4) * 4; // 4-byte aligned

    let pixel_offset = BITMAPINFOHEADER_SIZE;
    let pixel_data = &dib[pixel_offset..];

    let mut img = RgbaImage::new(width, height);

    for y in 0..height {
        let src_y = if bottom_up { height - 1 - y } else { y };
        let row_start = src_y as usize * row_stride;

        for x in 0..width {
            let px_start = row_start + x as usize * bytes_per_pixel;
            let (r, g, b, a) = match bits_per_pixel {
                32 => (pixel_data[px_start + 2], pixel_data[px_start + 1],
                       pixel_data[px_start], pixel_data[px_start + 3]),
                24 => (pixel_data[px_start + 2], pixel_data[px_start + 1],
                       pixel_data[px_start], 255),
                _ => (0, 0, 0, 255),
            };
            img.put_pixel(x, y, image::Rgba([r, g, b, a]));
        }
    }

    let mut png_bytes = Vec::new();
    img.write_to(&mut Cursor::new(&mut png_bytes), ImageFormat::Png)?;
    Ok(png_bytes)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p macrdp-clipboard`
Expected: All tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/macrdp-clipboard/src/formats.rs
git commit -m "feat(clipboard): implement DIB <-> PNG image format conversion"
```

---

## Task 10: Image Clipboard Support

**Goal:** Extend MacClipboardBackend to handle CF_DIB image format.

**Files:**
- Modify: `crates/macrdp-clipboard/src/lib.rs`

- [ ] **Step 1: Add CF_DIB handling to on_format_data_request**

In the `on_format_data_request` method, update the `CF_DIB` branch (currently returns `Vec::new()`):

```rust
} else if format == ClipboardFormatId::CF_DIB {
    self.pasteboard
        .read_image()
        .and_then(|png_data| png_to_dib(&png_data).ok())
        .unwrap_or_default()
}
```

- [ ] **Step 2: Verify on_format_data_response handles images**

The DIB→PNG conversion in `on_format_data_response` was already added in Task 7 (checks BITMAPINFOHEADER signature). Verify it works by testing with an actual DIB payload.

- [ ] **Step 3: Build and verify**

Run: `cargo build -p macrdp-clipboard`
Expected: Compiles.

- [ ] **Step 4: Commit**

```bash
git add crates/macrdp-clipboard/src/lib.rs
git commit -m "feat(clipboard): add CF_DIB image clipboard support"
```

---

## Task 11: File Clipboard (FileGroupDescriptorW + FileContents)

**Goal:** Implement file transfer via clipboard: FileGroupDescriptorW serialization, FileContents I/O, temp directory management.

**Files:**
- Modify: `crates/macrdp-clipboard/src/file.rs`
- Modify: `crates/macrdp-clipboard/src/lib.rs`
- Modify: `crates/macrdp-clipboard/src/formats.rs`

- [ ] **Step 1: Write FileGroupDescriptorW serialization tests**

```rust
// crates/macrdp-clipboard/src/file.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_file_descriptor_roundtrip() {
        let desc = FileDescriptor {
            name: "test.txt".to_string(),
            size: 1234,
        };
        let bytes = serialize_file_group_descriptor(&[desc.clone()]);
        // cItems (4 bytes) + 1 FILEDESCRIPTORW (592 bytes)
        assert_eq!(bytes.len(), 4 + FILEDESCRIPTORW_SIZE);

        let descs = parse_file_group_descriptor(&bytes).unwrap();
        assert_eq!(descs.len(), 1);
        assert_eq!(descs[0].name, "test.txt");
        assert_eq!(descs[0].size, 1234);
    }

    #[test]
    fn file_descriptor_unicode_name() {
        let desc = FileDescriptor {
            name: "\u{6587}\u{6863}.pdf".to_string(),
            size: 0,
        };
        let bytes = serialize_file_group_descriptor(&[desc]);
        let parsed = parse_file_group_descriptor(&bytes).unwrap();
        assert_eq!(parsed[0].name, "\u{6587}\u{6863}.pdf");
    }

    #[test]
    fn file_descriptor_multiple_files() {
        let descs = vec![
            FileDescriptor { name: "a.txt".to_string(), size: 100 },
            FileDescriptor { name: "b.png".to_string(), size: 200 },
        ];
        let bytes = serialize_file_group_descriptor(&descs);
        assert_eq!(bytes.len(), 4 + 2 * FILEDESCRIPTORW_SIZE);
        let parsed = parse_file_group_descriptor(&bytes).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "a.txt");
        assert_eq!(parsed[1].size, 200);
    }

    #[test]
    fn file_descriptor_rejects_path_traversal() {
        let desc = FileDescriptor {
            name: "../../../etc/passwd".to_string(),
            size: 0,
        };
        let bytes = serialize_file_group_descriptor(&[desc]);
        let parsed = parse_file_group_descriptor(&bytes).unwrap();
        // Path traversal entries are skipped
        assert_eq!(parsed.len(), 0);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p macrdp-clipboard`
Expected: FAIL.

- [ ] **Step 3: Implement FileGroupDescriptorW serialization**

```rust
// crates/macrdp-clipboard/src/file.rs
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Size of a single FILEDESCRIPTORW struct in bytes.
/// dwFlags(4) + clsid(16) + sizel(8) + pointl(8) + dwFileAttributes(4) +
/// ftCreationTime(8) + ftLastAccessTime(8) + ftLastWriteTime(8) +
/// nFileSizeHigh(4) + nFileSizeLow(4) + cFileName[260](520) = 592 bytes
pub const FILEDESCRIPTORW_SIZE: usize = 592;

/// Flag bits for dwFlags in FILEDESCRIPTORW
const FD_FILESIZE: u32 = 0x00000040;
const FD_WRITESTIME: u32 = 0x00000020;

#[derive(Clone, Debug)]
pub struct FileDescriptor {
    pub name: String,
    pub size: u64,
}

/// Build FileGroupDescriptorW bytes from file metadata.
pub fn serialize_file_group_descriptor(files: &[FileDescriptor]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + files.len() * FILEDESCRIPTORW_SIZE);

    // cItems
    buf.extend_from_slice(&(files.len() as u32).to_le_bytes());

    for file in files {
        let start = buf.len();

        // dwFlags
        buf.extend_from_slice(&(FD_FILESIZE).to_le_bytes());
        // Reserved (32 bytes of zeros)
        buf.extend_from_slice(&[0u8; 32]);
        // dwFileAttributes (0 = normal file)
        buf.extend_from_slice(&0u32.to_le_bytes());
        // ftCreationTime (8 bytes zero)
        buf.extend_from_slice(&[0u8; 8]);
        // ftLastAccessTime (8 bytes zero)
        buf.extend_from_slice(&[0u8; 8]);
        // ftLastWriteTime (8 bytes zero)
        buf.extend_from_slice(&[0u8; 8]);
        // nFileSizeHigh
        buf.extend_from_slice(&((file.size >> 32) as u32).to_le_bytes());
        // nFileSizeLow
        buf.extend_from_slice(&((file.size & 0xFFFFFFFF) as u32).to_le_bytes());

        // cFileName[260] — UTF-16LE, null-padded to 260 chars (520 bytes)
        let utf16: Vec<u16> = file.name.encode_utf16().collect();
        let mut name_buf = [0u8; 520];
        for (i, &code_unit) in utf16.iter().take(259).enumerate() {
            let bytes = code_unit.to_le_bytes();
            name_buf[i * 2] = bytes[0];
            name_buf[i * 2 + 1] = bytes[1];
        }
        buf.extend_from_slice(&name_buf);

        assert_eq!(buf.len() - start, FILEDESCRIPTORW_SIZE);
    }

    buf
}

/// Parse FileGroupDescriptorW bytes into file descriptors.
pub fn parse_file_group_descriptor(data: &[u8]) -> anyhow::Result<Vec<FileDescriptor>> {
    if data.len() < 4 {
        anyhow::bail!("FileGroupDescriptorW too short");
    }

    let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let mut result = Vec::with_capacity(count);
    let mut offset = 4;

    for _ in 0..count {
        if offset + FILEDESCRIPTORW_SIZE > data.len() {
            anyhow::bail!("Truncated FileGroupDescriptorW");
        }

        let entry = &data[offset..offset + FILEDESCRIPTORW_SIZE];

        // nFileSizeHigh at offset 68, nFileSizeLow at offset 72
        let size_high = u32::from_le_bytes([entry[68], entry[69], entry[70], entry[71]]) as u64;
        let size_low = u32::from_le_bytes([entry[72], entry[73], entry[74], entry[75]]) as u64;
        let size = (size_high << 32) | size_low;

        // cFileName at offset 76, 520 bytes (260 UTF-16LE chars)
        let name_data = &entry[76..76 + 520];
        let words: Vec<u16> = name_data
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&w| w != 0)
            .collect();
        let name = String::from_utf16_lossy(&words);

        // Security: reject path traversal
        if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
            tracing::warn!("Rejected file with suspicious path: {}", name);
            offset += FILEDESCRIPTORW_SIZE;
            continue;
        }

        result.push(FileDescriptor { name, size });
        offset += FILEDESCRIPTORW_SIZE;
    }

    Ok(result)
}

/// Collect file descriptors from local file paths.
pub fn file_descriptors_from_paths(paths: &[PathBuf]) -> Vec<FileDescriptor> {
    paths
        .iter()
        .filter_map(|path| {
            let meta = fs::metadata(path).ok()?;
            let name = path.file_name()?.to_string_lossy().to_string();
            Some(FileDescriptor {
                name,
                size: meta.len(),
            })
        })
        .collect()
}

/// Read file contents at a specific offset and length.
pub fn read_file_range(
    path: &Path,
    offset: u64,
    length: u32,
    max_file_size: u64,
) -> anyhow::Result<Vec<u8>> {
    let meta = fs::metadata(path)?;
    if meta.len() > max_file_size {
        anyhow::bail!("File exceeds maximum size limit");
    }

    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; length as usize];
    let bytes_read = file.read(&mut buf)?;
    buf.truncate(bytes_read);
    Ok(buf)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p macrdp-clipboard`
Expected: All tests PASS.

- [ ] **Step 5: Wire file format into MacClipboardBackend**

In `on_format_data_request()`, add file handling for FileGroupDescriptorW format. In `on_file_contents_request()`, implement file range reading. These modifications extend the existing match arms with the file clipboard logic.

- [ ] **Step 6: Commit**

```bash
git add crates/macrdp-clipboard/
git commit -m "feat(clipboard): implement file clipboard with FileGroupDescriptorW"
```

---

## Task 12: Opus Encoding (Feature-gated)

**Goal:** Add optional Opus audio encoding behind `opus` feature flag.

**Files:**
- Modify: `crates/macrdp-audio/src/opus.rs`
- Modify: `crates/macrdp-audio/src/lib.rs`

- [ ] **Step 1: Implement OpusEncoder wrapper**

```rust
// crates/macrdp-audio/src/opus.rs
use anyhow::Result;

pub struct OpusEncoder {
    encoder: opus::Encoder,
    frame_size: usize, // Samples per channel per frame (960 for 20ms@48kHz)
    channels: u16,
}

impl OpusEncoder {
    pub fn new(sample_rate: u32, channels: u16) -> Result<Self> {
        let ch = match channels {
            1 => opus::Channels::Mono,
            _ => opus::Channels::Stereo,
        };
        let encoder = opus::Encoder::new(sample_rate, ch, opus::Application::Audio)?;
        let frame_size = (sample_rate / 50) as usize; // 20ms

        Ok(Self {
            encoder,
            frame_size,
            channels,
        })
    }

    /// Encode interleaved Float32 PCM to Opus frames.
    /// Input length must be frame_size * channels.
    pub fn encode_frame(&mut self, input: &[f32]) -> Result<Vec<u8>> {
        let mut output = vec![0u8; 4000]; // Max Opus frame size
        let len = self.encoder.encode_float(input, &mut output)?;
        output.truncate(len);
        Ok(output)
    }

    pub fn frame_size(&self) -> usize {
        self.frame_size
    }
}
```

- [ ] **Step 2: Add Opus format to MacAudioHandler**

In `MacAudioHandler::new()`, when opus feature is enabled, add Opus AudioFormat:

```rust
#[cfg(feature = "opus")]
{
    formats.push(AudioFormat {
        format: WaveFormat::OPUS,
        n_channels: channels,
        n_samples_per_sec: sample_rate,
        n_avg_bytes_per_sec: 16000, // ~128kbps
        n_block_align: 1,
        bits_per_sample: 16,
        data: None,
    });
}
```

- [ ] **Step 3: Update audio_loop for Opus support**

Add Opus encoding path to the wave data generation in audio_loop (behind cfg feature gate).

- [ ] **Step 4: Build with Opus feature**

Run: `cargo build -p macrdp-audio --features opus`
Expected: Compiles (requires libopus system library).

- [ ] **Step 5: Commit**

```bash
git add crates/macrdp-audio/
git commit -m "feat(audio): add optional Opus encoding (feature-gated)"
```

---

## Task 13: End-to-End Verification

**Goal:** Manual end-to-end testing with RDP clients.

**Files:** None (testing only)

- [ ] **Step 1: Build full project**

Run: `cargo build --release`
Expected: Full project compiles.

- [ ] **Step 2: Run unit tests**

Run: `cargo test --workspace`
Expected: All unit tests pass.

- [ ] **Step 3: Test audio with FreeRDP**

1. Start macrdp server: `cargo run --release -p macrdp-server`
2. Connect with FreeRDP: `xfreerdp /v:localhost /u:user /p:pass /sound:sys:pulse`
3. Play audio on macOS → verify audio plays on client
4. Check log for "Audio started with format" message

- [ ] **Step 4: Test clipboard with mstsc**

1. Connect with Windows mstsc (enable clipboard redirection)
2. Copy text on macOS → paste in Windows → verify text matches
3. Copy text in Windows → paste on macOS → verify text matches
4. Copy image on macOS → paste in Windows → verify image
5. Copy file on macOS → paste in Windows → verify file contents

- [ ] **Step 5: Commit any fixes**

```bash
git add -A
git commit -m "fix: end-to-end testing fixes for clipboard and audio"
```

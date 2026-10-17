//! RDPGFX (Graphics Pipeline) DVC handler

use std::sync::{Arc, Mutex};

use ironrdp_core::{encode_vec, impl_as_any, Encode, EncodeResult, WriteCursor};
use ironrdp_dvc::{DvcEncode, DvcProcessor, DvcServerProcessor};
use ironrdp_pdu::gcc::{Monitor, MonitorFlags};
use ironrdp_pdu::geometry::InclusiveRectangle;
use ironrdp_pdu::rdp::vc::dvc::gfx::{
    Avc420BitmapStream, Avc444BitmapStream, CapabilitiesConfirmPdu, CapabilitiesV103Flags,
    CapabilitiesV104Flags, CapabilitiesV107Flags, CapabilitiesV10Flags, CapabilitiesV81Flags,
    CapabilitySet, ClientPdu, Codec1Type, CreateSurfacePdu, DeleteSurfacePdu, Encoding,
    EndFramePdu, MapSurfaceToOutputPdu, PixelFormat as GfxPixelFormat, QuantQuality, QueueDepth,
    ResetGraphicsPdu, ServerPdu, StartFramePdu, Timestamp, WireToSurface1Pdu,
};
use ironrdp_pdu::{decode, PduResult};

type DvcMessage = ironrdp_dvc::DvcMessage;
use tracing::{debug, info};

use crate::display::{GfxFrameUpdate, GfxUncompressedUpdate};

#[path = "timings.rs"]
mod timings;
pub use timings::{StageTimings, TimingSummary};

#[path = "connection_outcome.rs"]
mod connection_outcome;
pub use connection_outcome::ConnectionOutcome;

/// GFX channel name as defined by RDP spec
pub const GFX_CHANNEL_NAME: &str = "Microsoft::Windows::RDS::Graphics";
/// Opt-in transport hint used only by our experimental browser AVC420 client.
/// It fits the 15-character RDP client-name limit and retains Guacamole's
/// existing keyboard translation identity. This is not an authentication hint.
pub const BROWSER_AVC420_CLIENT_NAME: &str = "GuacamoleAVC420";
/// Guacamole/FreeRDP interop is more reliable when uncompressed surface
/// updates follow the normal StartFrame/EndFrame sequence instead of relying
/// on immediate application of standalone surface commands.
const USE_FRAME_BOUNDARIES_FOR_UNCOMPRESSED: bool = true;

/// Wrapper to make raw PDU bytes usable as DvcMessage
pub struct RawGfxPdu(pub Vec<u8>);

impl Encode for RawGfxPdu {
    fn encode(&self, dst: &mut WriteCursor<'_>) -> EncodeResult<()> {
        dst.write_slice(&self.0);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "GfxServerPdu"
    }

    fn size(&self) -> usize {
        self.0.len()
    }
}

impl DvcEncode for RawGfxPdu {}

// SAFETY: RawGfxPdu only contains Vec<u8> which is Send
unsafe impl Send for RawGfxPdu {}

/// Wrap raw GFX PDU bytes in RDP_SEGMENTED_DATA (ZGFX) format.
/// MS-RDPEGFX Section 2.2.5: ALL GFX PDUs must be ZGFX-wrapped before DVC transport.
/// Using uncompressed mode (descriptor 0xE0/0xE1, compression type 0x04).
fn wrap_zgfx(data: &[u8]) -> Vec<u8> {
    const SINGLE: u8 = 0xE0;
    const MULTIPART: u8 = 0xE1;
    const UNCOMPRESSED: u8 = 0x04;
    // Max data per segment = 65534 bytes. The segmentSize field includes the
    // 1-byte compression type, so segmentSize = data_len + 1 ≤ 65535 (0xFFFF).
    const MAX_SEG_DATA: usize = 65534;

    if data.len() <= MAX_SEG_DATA {
        // Single segment: descriptor(1) + compression_type(1) + data
        let mut out = Vec::with_capacity(2 + data.len());
        out.push(SINGLE);
        out.push(UNCOMPRESSED);
        out.extend_from_slice(data);
        out
    } else {
        // Multipart: descriptor(1) + seg_count(2) + uncompressed_size(4) + segments
        let seg_count = data.len().div_ceil(MAX_SEG_DATA);
        let mut out = Vec::with_capacity(7 + data.len() + seg_count * 5);
        out.push(MULTIPART);
        out.extend_from_slice(&(seg_count as u16).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        for chunk in data.chunks(MAX_SEG_DATA) {
            // segmentSize = compression_type(1) + chunk data
            let seg_size = (chunk.len() + 1) as u32;
            out.extend_from_slice(&seg_size.to_le_bytes());
            out.push(UNCOMPRESSED);
            out.extend_from_slice(chunk);
        }
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CapabilitySelection {
    rank: u8,
    avc420: bool,
    avc444: bool,
}

fn describe_capability(cap: &CapabilitySet) -> CapabilitySelection {
    match cap {
        CapabilitySet::V10_7 { flags } if !flags.contains(CapabilitiesV107Flags::AVC_DISABLED) => {
            CapabilitySelection {
                rank: 17,
                avc420: true,
                avc444: true,
            }
        }
        CapabilitySet::V10_6 { flags } | CapabilitySet::V10_6Err { flags }
            if !flags.contains(CapabilitiesV104Flags::AVC_DISABLED) =>
        {
            CapabilitySelection {
                rank: 16,
                avc420: true,
                avc444: true,
            }
        }
        CapabilitySet::V10_5 { flags } | CapabilitySet::V10_4 { flags }
            if !flags.contains(CapabilitiesV104Flags::AVC_DISABLED) =>
        {
            CapabilitySelection {
                rank: 15,
                avc420: true,
                avc444: true,
            }
        }
        CapabilitySet::V10_3 { flags } if !flags.contains(CapabilitiesV103Flags::AVC_DISABLED) => {
            CapabilitySelection {
                rank: 14,
                avc420: true,
                avc444: true,
            }
        }
        CapabilitySet::V10_2 { flags } | CapabilitySet::V10 { flags }
            if !flags.contains(CapabilitiesV10Flags::AVC_DISABLED) =>
        {
            CapabilitySelection {
                rank: 13,
                avc420: true,
                avc444: true,
            }
        }
        CapabilitySet::V8_1 { flags } if flags.contains(CapabilitiesV81Flags::AVC420_ENABLED) => {
            CapabilitySelection {
                rank: 11,
                avc420: true,
                avc444: false,
            }
        }
        CapabilitySet::V8_1 { .. } => CapabilitySelection {
            rank: 7,
            avc420: false,
            avc444: false,
        },
        // For non-AVC sessions prefer the most conservative capability set
        // that still keeps us on the graphics pipeline. Newer 10.x non-AVC
        // confirmations black-screened with Guacamole in testing.
        CapabilitySet::V10_1 => CapabilitySelection {
            rank: 6,
            avc420: false,
            avc444: false,
        },
        CapabilitySet::V8 { .. } => CapabilitySelection {
            rank: 5,
            avc420: false,
            avc444: false,
        },
        CapabilitySet::V10_2 { .. } | CapabilitySet::V10 { .. } => CapabilitySelection {
            rank: 4,
            avc420: false,
            avc444: false,
        },
        CapabilitySet::V10_3 { .. } => CapabilitySelection {
            rank: 3,
            avc420: false,
            avc444: false,
        },
        CapabilitySet::V10_5 { .. } | CapabilitySet::V10_4 { .. } => CapabilitySelection {
            rank: 2,
            avc420: false,
            avc444: false,
        },
        CapabilitySet::V10_7 { .. }
        | CapabilitySet::V10_6 { .. }
        | CapabilitySet::V10_6Err { .. } => CapabilitySelection {
            rank: 1,
            avc420: false,
            avc444: false,
        },
        CapabilitySet::Unknown(_) => CapabilitySelection {
            rank: 0,
            avc420: false,
            avc444: false,
        },
    }
}

fn choose_confirmed_capability(cap_sets: &[CapabilitySet]) -> (CapabilitySet, CapabilitySelection) {
    let mut best = None;

    for cap in cap_sets {
        let selection = describe_capability(cap);
        if best
            .as_ref()
            .is_none_or(|(_, best_selection)| selection > *best_selection)
        {
            best = Some((cap.clone(), selection));
        }
    }

    best.unwrap_or((
        CapabilitySet::V8 {
            flags: ironrdp_pdu::rdp::vc::dvc::gfx::CapabilitiesV8Flags::empty(),
        },
        CapabilitySelection {
            rank: 0,
            avc420: false,
            avc444: false,
        },
    ))
}

fn should_prefer_bitmap_fallback(selection: CapabilitySelection) -> bool {
    // Guacamole/FreeRDP sessions that negotiate GFX but do not explicitly
    // advertise AVC support have black-screened on the uncompressed-only path
    // in live testing. Those clients still render the classic bitmap path, so
    // pivot there immediately instead of waiting for an AVC failure that will
    // never arrive in non-AVC sessions.
    !selection.avc420
}

/// Unwrap ZGFX segmented data from client. Returns None if not ZGFX-wrapped.
fn unwrap_zgfx(data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() {
        return None;
    }
    match data[0] {
        0xE0 => {
            // Single segment: skip descriptor(1) + compression_type(1)
            if data.len() > 2 && data[1] == 0x04 {
                Some(data[2..].to_vec())
            } else {
                None
            }
        }
        0xE1 => {
            // Multipart: descriptor(1) + segment_count(2) + uncompressed_size(4)
            if data.len() < 7 {
                return None;
            }
            let seg_count = u16::from_le_bytes([data[1], data[2]]) as usize;
            let mut offset = 7;
            let mut result = Vec::new();
            for _ in 0..seg_count {
                if offset + 4 > data.len() {
                    break;
                }
                let seg_size = u32::from_le_bytes([
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                ]) as usize;
                offset += 4;
                if offset < data.len() && data[offset] == 0x04 {
                    // Uncompressed: skip compression_type byte
                    offset += 1;
                    let end = (offset + seg_size - 1).min(data.len());
                    result.extend_from_slice(&data[offset..end]);
                    offset = end;
                } else {
                    break;
                }
            }
            Some(result)
        }
        _ => None, // Not ZGFX-wrapped
    }
}

fn make_dvc_message(pdu: &ServerPdu) -> PduResult<DvcMessage> {
    let data = encode_vec(pdu).map_err(|e| ironrdp_pdu::custom_err!("GfxEncode", e))?;
    let wrapped = wrap_zgfx(&data);
    Ok(Box::new(RawGfxPdu(wrapped)))
}

/// Shared state between GfxHandler and the server
#[derive(Debug)]
pub struct GfxState {
    /// Stable terminal reason, published before disconnect callbacks run.
    pub connection_outcome: Option<ConnectionOutcome>,
    /// Clipboard was quarantined for this connection; a reconnect resets it.
    pub clipboard_disabled: bool,
    /// Whether this connection negotiated the GFX/DVC path at all.
    pub session_uses_gfx: bool,
    pub channel_id: Option<u32>,
    pub surface_created: bool,
    /// ResetGraphics resizes output but does not delete existing surfaces.
    surface_delete_pending: bool,
    pub caps_confirmed: bool,
    pub frame_id: u32,
    pub avc420_supported: bool,
    /// Whether the client supports AVC444 (V10+ with AVC not disabled)
    pub avc444_supported: bool,
    /// Whether AVC444 is enabled by server config (chroma_mode = "avc444")
    pub avc444_enabled: bool,
    /// The browser consumes one continuous full-frame AVC420 stream. Keep tiny
    /// updates in that stream instead of switching between video and raw RGB.
    pub browser_avc420: bool,
    pub confirmed_cap: Option<CapabilitySet>,
    pub width: u16,
    pub height: u16,
    /// Frames sent but not yet acknowledged
    pub pending_acks: u32,
    /// Last acknowledged frame ID
    pub last_ack_frame: u32,
    /// MS-RDPEGFX permits clients to suspend ACKs; silence is then expected.
    pub frame_acknowledgements_suspended: bool,
    /// Client-reported pending graphics bytes, when supplied by the client.
    pub client_queue_depth_bytes: Option<u32>,
    pub client_total_frames_decoded: u32,
    /// Network quality estimate (0.0 = congested, 1.0 = excellent)
    pub network_quality: f32,
    /// Send timestamps for RTT calculation (frame_id → Instant)
    pub frame_send_times: std::collections::HashMap<u32, std::time::Instant>,
    /// Exponentially weighted moving average RTT in ms
    pub rtt_ewma_ms: f64,
    /// Last frame encode time in ms
    pub last_encode_ms: f64,
    /// Most recent display update's socket/write-lock wait, excluding encoding.
    pub last_write_ms: f64,
    /// Last 1024 real samples for each stage, independently of UI polling.
    pub timings: StageTimings,
    /// Successfully delivered GFX frames or classic bitmap updates. Bitmap
    /// regions may split one captured frame, so this is an update-rate counter.
    pub total_frames_sent: u64,
    pub capture_queue_ms: f64,
    pub capture_frames_coalesced: u64,
    pub completion_queue_ms: f64,
    /// Last frame size in bytes
    pub last_frame_bytes: u32,
    /// Total bytes sent
    pub total_bytes_sent: u64,
    /// Total compressed fast-path bytes sent for classic bitmap updates.
    pub bitmap_wire_bytes: u64,
    /// Bitmap codec chosen from this connection's capability exchange.
    pub bitmap_codec: Option<&'static str>,
    /// Client's Confirm Active preferred depth; wire bitmaps currently use 32bpp.
    pub negotiated_client_depth: Option<u16>,
    /// Completed bitmap dispatches, including identical updates with no wire data.
    pub bitmap_dispatches: u64,
    pub bitmap_encode_ms_total: f64,
    pub bitmap_write_ms_total: f64,
    pub(crate) bitmap_budget: crate::server::bitmap_budget::BitmapBudget,
    /// Actual working partition budget, capped by the client's reassembly limit.
    pub bitmap_update_budget_bytes: Option<u32>,
    /// Completed uncompressed GFX service costs, independent of AVC/bitmap pacing.
    pub uncompressed_dispatches: u64,
    pub uncompressed_prepare_ms_total: f64,
    pub uncompressed_write_ms_total: f64,
    pub uncompressed_wire_bytes: u64,
    /// Time of first frame sent
    pub start_time: Option<std::time::Instant>,
    /// Client peer IP address (set after TCP connection is established)
    pub peer_addr: Option<std::net::IpAddr>,
    /// Instantaneous bitrate tracking (per-frame Mbps samples since last log)
    bitrate_samples: Vec<f64>,
    bitrate_max: f64,
    bitrate_min: f64,
    last_frame_time: Option<std::time::Instant>,
    /// Hot-updatable: target bitrate override (set via ConfigUpdate)
    pub target_bitrate: Option<u32>,
    /// Hot-updatable: frame rate override (set via ConfigUpdate).
    pub target_frame_rate: Option<u32>,
    /// Most recent native capture target after adaptive pacing.
    pub requested_capture_frame_rate: Option<u32>,
    /// Adaptive codec target and last confirmed backend application.
    pub requested_encoder_bitrate: Option<u32>,
    pub applied_encoder_bitrate: Option<u32>,
    pub encoder_bitrate_update_failures: u64,
    /// Currently applied capture frame rate after hot updates/adaptation.
    pub applied_frame_rate: Option<u32>,
    /// Failed native configuration attempts for the current capture owner.
    pub capture_frame_rate_update_failures: u64,
    /// The current target exhausted its bounded native retry budget.
    pub capture_frame_rate_retry_exhausted: bool,
    /// Hot-updatable: show cursor flag (set via ConfigUpdate)
    pub show_cursor: Option<bool>,
    /// Hot-updatable: resolution (set via ConfigUpdate)
    pub resolution: Option<String>,
    /// Hot-updatable: encoder preference (set via ConfigUpdate)
    pub encoder_pref: Option<String>,
    /// Requested image/video quality, retained across connection recovery.
    pub target_quality: Option<String>,
    /// Explicit user selection, separate from connection-local failure fallback.
    pub manual_bitmap_mode: bool,
    /// Coalescing wake-up for controls while capture is idle or awaiting encoding.
    controls_changed: tokio::sync::watch::Sender<u64>,
    /// Backend instantiated by capture, rather than the configured preference.
    pub active_encoder: Option<&'static str>,
    /// Hot-updatable: chroma mode (set via ConfigUpdate)
    pub chroma_mode: Option<String>,
    /// Whether to force negotiated GFX sessions into uncompressed validation mode.
    pub gfx_validation_mode: bool,
    /// Session-local escape hatch when a client negotiates AVC but never
    /// acknowledges any GFX frames. This keeps broken clients such as some
    /// Guacamole builds from getting stuck on a stale image.
    pub avc_fallback_to_uncompressed: bool,
    /// Session-local escape hatch to abandon GFX entirely and return to
    /// classic bitmap updates when negotiated GFX proves incompatible.
    pub bitmap_fallback_mode: bool,
}

impl GfxState {
    pub fn new(width: u16, height: u16, avc444_enabled: bool, gfx_validation_mode: bool) -> Self {
        Self {
            connection_outcome: None,
            clipboard_disabled: false,
            session_uses_gfx: false,
            channel_id: None,
            surface_created: false,
            surface_delete_pending: false,
            caps_confirmed: false,
            frame_id: 0,
            avc420_supported: false,
            avc444_supported: false,
            avc444_enabled,
            browser_avc420: false,
            confirmed_cap: None,
            width,
            height,
            pending_acks: 0,
            last_ack_frame: 0,
            frame_acknowledgements_suspended: false,
            client_queue_depth_bytes: None,
            client_total_frames_decoded: 0,
            network_quality: 1.0,
            frame_send_times: std::collections::HashMap::new(),
            rtt_ewma_ms: 0.0,
            last_encode_ms: 0.0,
            last_write_ms: 0.0,
            timings: StageTimings::default(),
            total_frames_sent: 0,
            capture_queue_ms: 0.0,
            capture_frames_coalesced: 0,
            completion_queue_ms: 0.0,
            last_frame_bytes: 0,
            total_bytes_sent: 0,
            bitmap_wire_bytes: 0,
            bitmap_codec: None,
            negotiated_client_depth: None,
            bitmap_dispatches: 0,
            bitmap_encode_ms_total: 0.0,
            bitmap_write_ms_total: 0.0,
            bitmap_budget: Default::default(),
            bitmap_update_budget_bytes: None,
            uncompressed_dispatches: 0,
            uncompressed_prepare_ms_total: 0.0,
            uncompressed_write_ms_total: 0.0,
            uncompressed_wire_bytes: 0,
            start_time: None,
            peer_addr: None,
            bitrate_samples: Vec::new(),
            bitrate_max: 0.0,
            bitrate_min: f64::MAX,
            last_frame_time: None,
            target_bitrate: None,
            target_frame_rate: None,
            requested_capture_frame_rate: None,
            requested_encoder_bitrate: None,
            applied_encoder_bitrate: None,
            encoder_bitrate_update_failures: 0,
            applied_frame_rate: None,
            capture_frame_rate_update_failures: 0,
            capture_frame_rate_retry_exhausted: false,
            show_cursor: None,
            resolution: None,
            encoder_pref: None,
            target_quality: None,
            manual_bitmap_mode: false,
            controls_changed: tokio::sync::watch::channel(0).0,
            active_encoder: None,
            chroma_mode: None,
            gfx_validation_mode,
            avc_fallback_to_uncompressed: false,
            bitmap_fallback_mode: false,
        }
    }

    pub fn controls_receiver(&self) -> tokio::sync::watch::Receiver<u64> {
        self.controls_changed.subscribe()
    }

    pub fn notify_controls_changed(&mut self) {
        self.controls_changed
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    /// User image modes do not erase negotiated capabilities or failure state.
    pub fn bitmap_mode(&self) -> bool {
        self.manual_bitmap_mode
            || self.bitmap_fallback_mode
            || self.target_quality.as_deref() == Some("lossless")
    }

    pub fn requested_bitmap_quality(&self) -> Option<crate::BitmapQuality> {
        self.target_quality
            .as_deref()
            .and_then(crate::BitmapQuality::from_config)
    }

    pub fn next_frame_id(&mut self) -> u32 {
        self.frame_id = self.frame_id.wrapping_add(1);
        if !self.frame_acknowledgements_suspended {
            self.frame_send_times
                .insert(self.frame_id, std::time::Instant::now());
            // Keep bounded diagnostic history across frame-id wraparound.
            if self.frame_send_times.len() > 120 {
                if let Some(oldest) = self
                    .frame_send_times
                    .iter()
                    .min_by_key(|(_, time)| **time)
                    .map(|(&id, _)| id)
                {
                    self.frame_send_times.remove(&oldest);
                }
            }
            self.pending_acks = self.frame_send_times.len() as u32;
        }
        self.frame_id
    }

    pub fn record_capture_queue(&mut self, ms: f64) {
        self.capture_queue_ms = ms;
        self.timings.capture.record(ms);
    }

    pub fn record_encode_timing(&mut self, ms: f64) {
        self.last_encode_ms = ms;
        self.timings.encode.record(ms);
    }

    pub fn record_completion_queue(&mut self, ms: f64) {
        self.completion_queue_ms = ms;
        self.timings.completion.record(ms);
    }

    /// Account only for display data whose entire transport write succeeded.
    /// `bytes` includes RDP framing; `write_ms` excludes capture and encoding.
    pub fn record_display_write(&mut self, bytes: usize, write_ms: f64) {
        self.total_bytes_sent = self.total_bytes_sent.saturating_add(bytes as u64);
        self.total_frames_sent = self.total_frames_sent.saturating_add(1);
        self.last_frame_bytes = u32::try_from(bytes).unwrap_or(u32::MAX);
        self.last_write_ms = write_ms;
        self.timings.write.record(write_ms);
        if self.start_time.is_none() {
            self.start_time = Some(std::time::Instant::now());
        }
        // Track instantaneous bitrate per frame
        if let Some(prev) = self.last_frame_time {
            let dt = prev.elapsed().as_secs_f64().max(0.0001);
            let mbps = self.last_frame_bytes as f64 * 8.0 / dt / 1_000_000.0;
            // Suspended or missing ACKs must not grow diagnostics indefinitely.
            if self.bitrate_samples.len() == 120 {
                self.bitrate_samples.remove(0);
            }
            self.bitrate_samples.push(mbps);
            if mbps > self.bitrate_max {
                self.bitrate_max = mbps;
            }
            if mbps < self.bitrate_min {
                self.bitrate_min = mbps;
            }
        }
        self.last_frame_time = Some(std::time::Instant::now());
    }

    /// Update network quality and RTT based on frame acknowledgment
    pub fn ack_frame(&mut self, ack_frame_id: u32) {
        // Calculate RTT from send timestamp
        if let Some(send_time) = self.frame_send_times.remove(&ack_frame_id) {
            self.last_ack_frame = ack_frame_id;
            self.pending_acks = self.frame_send_times.len() as u32;
            let rtt_ms = send_time.elapsed().as_secs_f64() * 1000.0;
            // EWMA with alpha=0.2 for smooth averaging
            if self.rtt_ewma_ms == 0.0 {
                self.rtt_ewma_ms = rtt_ms;
            } else {
                self.rtt_ewma_ms = self.rtt_ewma_ms * 0.8 + rtt_ms * 0.2;
            }
        }

        // Network quality based on RTT and ack backlog
        self.network_quality = if self.rtt_ewma_ms < 15.0 && self.pending_acks <= 2 {
            1.0 // Excellent: max bitrate
        } else if self.rtt_ewma_ms < 30.0 && self.pending_acks <= 5 {
            0.8 // Good
        } else if self.rtt_ewma_ms < 60.0 && self.pending_acks <= 10 {
            0.5 // Fair
        } else if self.rtt_ewma_ms < 100.0 {
            0.3 // Poor
        } else {
            0.15 // Congested
        };
    }

    /// Apply the complete ACK state transition, including optional suspension.
    pub fn acknowledge_frame(
        &mut self,
        frame_id: u32,
        queue_depth: QueueDepth,
        total_decoded: u32,
    ) {
        self.client_total_frames_decoded = total_decoded;
        self.client_queue_depth_bytes = match queue_depth {
            QueueDepth::AvailableBytes(bytes) => Some(bytes),
            _ => None,
        };
        self.frame_acknowledgements_suspended = matches!(queue_depth, QueueDepth::Suspend);
        if self.frame_acknowledgements_suspended {
            self.frame_send_times.clear();
            self.pending_acks = 0;
            self.network_quality = 1.0;
        } else {
            self.ack_frame(frame_id);
        }
    }

    /// Start ACK turnaround measurement after the frame has been written.
    /// An ACK that raced the write may already have removed this entry.
    pub fn frame_written(&mut self, frame_id: u32) {
        if let Some(sent_at) = self.frame_send_times.get_mut(&frame_id) {
            *sent_at = std::time::Instant::now();
        }
    }

    /// Get recommended bitrate based on network quality and base bitrate
    pub fn adaptive_bitrate(&self, base_bitrate: u32) -> u32 {
        (base_bitrate as f32 * self.network_quality) as u32
    }

    pub fn reset_connection_state(&mut self, session_uses_gfx: bool) {
        self.session_uses_gfx = session_uses_gfx;
        self.channel_id = None;
        self.surface_created = false;
        self.surface_delete_pending = false;
        self.caps_confirmed = false;
        self.confirmed_cap = None;
        self.avc420_supported = false;
        self.avc444_supported = false;
        self.browser_avc420 = false;
        self.avc_fallback_to_uncompressed = false;
        self.bitmap_fallback_mode = false;
        self.frame_id = 0;
        self.pending_acks = 0;
        self.last_ack_frame = 0;
        self.frame_acknowledgements_suspended = false;
        self.client_queue_depth_bytes = None;
        self.client_total_frames_decoded = 0;
        self.network_quality = 1.0;
        self.frame_send_times.clear();
        self.rtt_ewma_ms = 0.0;
        self.last_encode_ms = 0.0;
        self.last_write_ms = 0.0;
        self.timings = StageTimings::default();
        self.total_frames_sent = 0;
        self.capture_queue_ms = 0.0;
        self.capture_frames_coalesced = 0;
        self.requested_capture_frame_rate = None;
        self.requested_encoder_bitrate = None;
        self.applied_encoder_bitrate = None;
        self.encoder_bitrate_update_failures = 0;
        self.applied_frame_rate = None;
        self.capture_frame_rate_update_failures = 0;
        self.capture_frame_rate_retry_exhausted = false;
        self.completion_queue_ms = 0.0;
        self.last_frame_bytes = 0;
        self.total_bytes_sent = 0;
        self.bitmap_wire_bytes = 0;
        self.bitmap_codec = None;
        self.negotiated_client_depth = None;
        self.bitmap_dispatches = 0;
        self.bitmap_encode_ms_total = 0.0;
        self.bitmap_write_ms_total = 0.0;
        self.bitmap_budget = Default::default();
        self.bitmap_update_budget_bytes = None;
        self.uncompressed_dispatches = 0;
        self.uncompressed_prepare_ms_total = 0.0;
        self.uncompressed_write_ms_total = 0.0;
        self.uncompressed_wire_bytes = 0;
        self.active_encoder = None;
        self.start_time = None;
        self.bitrate_samples.clear();
        self.bitrate_max = 0.0;
        self.bitrate_min = f64::MAX;
        self.last_frame_time = None;
    }

    pub fn has_gfx_channel(&self) -> bool {
        self.channel_id.is_some() && self.caps_confirmed
    }

    pub fn set_client_graphics_preferences(&mut self, client_name: &str) {
        self.browser_avc420 = client_name == BROWSER_AVC420_CLIENT_NAME;
    }

    pub fn use_avc444(&self) -> bool {
        self.avc444_supported && self.avc444_enabled && !self.browser_avc420
    }

    /// Retire the current surface before reusing ID 0 on this graphics channel.
    pub fn invalidate_surface(&mut self) {
        self.surface_delete_pending |= self.surface_created;
        self.surface_created = false;
    }

    pub fn is_ready(&self) -> bool {
        self.has_gfx_channel() && self.avc420_supported
    }
}

/// Surface IDs remain allocated across ResetGraphics (MS-RDPEGFX 3.3.5.14).
/// Delete the prior allocation before creating a new surface with the same ID.
fn append_pending_surface_delete(state: &mut GfxState, raw_pdus: &mut Vec<u8>) {
    if state.surface_delete_pending {
        if let Ok(data) = encode_vec(&ServerPdu::DeleteSurface(DeleteSurfacePdu {
            surface_id: 0,
        })) {
            raw_pdus.extend_from_slice(&data);
            state.surface_delete_pending = false;
        }
    }
}

/// GFX DVC processor
pub struct GfxHandler {
    pub state: Arc<Mutex<GfxState>>,
}

impl_as_any!(GfxHandler);

impl GfxHandler {
    pub fn new(state: Arc<Mutex<GfxState>>) -> Self {
        Self { state }
    }

    fn bgra_to_xrgb_bytes(src: &[u8]) -> Vec<u8> {
        let mut out = src.to_vec();
        for px in out.chunks_exact_mut(4) {
            px[3] = 0;
        }
        out
    }

    /// Create a single ZGFX-wrapped buffer containing all GFX PDUs for an H.264 frame.
    /// Per MS-RDPEGFX Section 2.2.5: "The server SHOULD combine multiple RDPGFX commands
    /// into a single RDP_SEGMENTED_DATA structure."
    /// First call also includes surface setup PDUs (ResetGraphics + CreateSurface + MapSurfaceToOutput).
    pub fn create_frame_pdu(state: &mut GfxState, frame: &GfxFrameUpdate) -> Vec<u8> {
        // Concatenate all raw GFX PDUs, then ZGFX-wrap once
        let mut raw_pdus = Vec::new();

        let enc_w = frame.enc_width;
        let enc_h = frame.enc_height;

        // Detect resolution change (e.g. client resize or HiDPI mismatch) — rebuild surface
        if state.surface_created && (state.width != frame.width || state.height != frame.height) {
            info!(
                old_w = state.width,
                old_h = state.height,
                new_w = frame.width,
                new_h = frame.height,
                "GFX resolution changed, rebuilding surface"
            );
            state.width = frame.width;
            state.height = frame.height;
            state.invalidate_surface();
        }

        // First frame: surface setup (CapConfirm already sent by DVC handler)
        if !state.surface_created {
            append_pending_surface_delete(state, &mut raw_pdus);
            if let Ok(data) = encode_vec(&ServerPdu::ResetGraphics(ResetGraphicsPdu {
                width: state.width as u32,
                height: state.height as u32,
                monitors: vec![Monitor {
                    left: 0,
                    top: 0,
                    right: state.width as i32 - 1,
                    bottom: state.height as i32 - 1,
                    flags: MonitorFlags::PRIMARY,
                }],
            })) {
                raw_pdus.extend_from_slice(&data);
            }

            if let Ok(data) = encode_vec(&ServerPdu::CreateSurface(CreateSurfacePdu {
                surface_id: 0,
                width: state.width,
                height: state.height,
                pixel_format: GfxPixelFormat::XRgb,
            })) {
                raw_pdus.extend_from_slice(&data);
            }

            if let Ok(data) = encode_vec(&ServerPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                surface_id: 0,
                output_origin_x: 0,
                output_origin_y: 0,
            })) {
                raw_pdus.extend_from_slice(&data);
            }

            state.surface_created = true;
            info!(
                "GFX surface created: {}x{} (enc: {}x{})",
                state.width, state.height, enc_w, enc_h
            );
        }

        let frame_id = state.next_frame_id();

        // StartFrame
        if let Ok(data) = encode_vec(&ServerPdu::StartFrame(StartFramePdu {
            timestamp: Timestamp {
                milliseconds: 0,
                seconds: 0,
                minutes: 0,
                hours: 0,
            },
            frame_id,
        })) {
            raw_pdus.extend_from_slice(&data);
        }

        let make_rect = || InclusiveRectangle {
            left: 0,
            top: 0,
            right: frame.width,   // RDPGFX_RECT16 exclusive bound = visible crop
            bottom: frame.height, // RDPGFX_RECT16 exclusive bound = visible crop
        };

        let make_dest_rect = || InclusiveRectangle {
            left: 0,
            top: 0,
            right: frame.width, // RDPGFX_RECT16 exclusive bound = match surface/desktop
            bottom: frame.height, // RDPGFX_RECT16 exclusive bound = match surface/desktop
        };

        let make_qq = || QuantQuality {
            quantization_parameter: 22,
            progressive: false,
            quality: 100,
        };

        // Choose AVC444 or AVC420 path based on available data and negotiated caps
        let use_avc444 = frame.h264_aux.is_some() && state.use_avc444();

        if use_avc444 {
            // AVC444 path: WireToSurface1 + Avc444BitmapStream
            let aux_data = frame.h264_aux.as_ref().unwrap();
            let avc444_stream = Avc444BitmapStream {
                encoding: Encoding::LUMA_AND_CHROMA,
                stream1: Avc420BitmapStream {
                    rectangles: vec![make_rect()],
                    quant_qual_vals: vec![make_qq()],
                    data: &frame.h264_data,
                },
                stream2: Some(Avc420BitmapStream {
                    rectangles: vec![make_rect()],
                    quant_qual_vals: vec![make_qq()],
                    data: aux_data,
                }),
            };

            if let Ok(avc444_data) = encode_vec(&avc444_stream) {
                if let Ok(data) = encode_vec(&ServerPdu::WireToSurface1(WireToSurface1Pdu {
                    surface_id: 0,
                    codec_id: Codec1Type::Avc444,
                    pixel_format: GfxPixelFormat::XRgb,
                    destination_rectangle: make_dest_rect(),
                    bitmap_data: avc444_data,
                })) {
                    raw_pdus.extend_from_slice(&data);
                }
            }
        } else {
            // AVC420 path: WireToSurface1 + Avc420BitmapStream
            let avc_stream = Avc420BitmapStream {
                rectangles: vec![make_rect()],
                quant_qual_vals: vec![make_qq()],
                data: &frame.h264_data,
            };

            if let Ok(avc_data) = encode_vec(&avc_stream) {
                if let Ok(data) = encode_vec(&ServerPdu::WireToSurface1(WireToSurface1Pdu {
                    surface_id: 0,
                    codec_id: Codec1Type::Avc420,
                    pixel_format: GfxPixelFormat::XRgb,
                    destination_rectangle: make_dest_rect(),
                    bitmap_data: avc_data,
                })) {
                    raw_pdus.extend_from_slice(&data);
                }
            }
        }

        // EndFrame
        if let Ok(data) = encode_vec(&ServerPdu::EndFrame(EndFramePdu { frame_id })) {
            raw_pdus.extend_from_slice(&data);
        }

        debug!(
            frame_id,
            raw_bytes = raw_pdus.len(),
            h264_bytes = frame.h264_data.len(),
            "GFX frame PDU created",
        );

        // Single ZGFX wrap for all concatenated PDUs
        wrap_zgfx(&raw_pdus)
    }

    /// Create a ZGFX-wrapped buffer for uncompressed dirty rect updates.
    /// Uses WireToSurface1 with Codec1Type::Uncompressed for each rect.
    /// First call also includes surface setup PDUs.
    pub fn create_uncompressed_pdu(
        state: &mut GfxState,
        update: &GfxUncompressedUpdate,
    ) -> Vec<u8> {
        let mut raw_pdus = Vec::new();

        // Detect resolution change — rebuild surface
        if state.surface_created && (state.width != update.width || state.height != update.height) {
            info!(
                old_w = state.width,
                old_h = state.height,
                new_w = update.width,
                new_h = update.height,
                "GFX uncompressed resolution changed, rebuilding surface"
            );
            state.width = update.width;
            state.height = update.height;
            state.invalidate_surface();
        }

        // First frame: surface setup (same as H.264 path)
        if !state.surface_created {
            append_pending_surface_delete(state, &mut raw_pdus);
            if let Ok(data) = encode_vec(&ServerPdu::ResetGraphics(ResetGraphicsPdu {
                width: state.width as u32,
                height: state.height as u32,
                monitors: vec![Monitor {
                    left: 0,
                    top: 0,
                    right: state.width as i32 - 1,
                    bottom: state.height as i32 - 1,
                    flags: MonitorFlags::PRIMARY,
                }],
            })) {
                raw_pdus.extend_from_slice(&data);
            }

            if let Ok(data) = encode_vec(&ServerPdu::CreateSurface(CreateSurfacePdu {
                surface_id: 0,
                width: state.width,
                height: state.height,
                pixel_format: GfxPixelFormat::XRgb,
            })) {
                raw_pdus.extend_from_slice(&data);
            }

            if let Ok(data) = encode_vec(&ServerPdu::MapSurfaceToOutput(MapSurfaceToOutputPdu {
                surface_id: 0,
                output_origin_x: 0,
                output_origin_y: 0,
            })) {
                raw_pdus.extend_from_slice(&data);
            }

            state.surface_created = true;
            info!(
                "GFX surface created (uncompressed): {}x{}",
                state.width, state.height
            );
        }

        let frame_id = if USE_FRAME_BOUNDARIES_FOR_UNCOMPRESSED {
            Some(state.next_frame_id())
        } else {
            None
        };

        if let Some(frame_id) = frame_id {
            if let Ok(data) = encode_vec(&ServerPdu::StartFrame(StartFramePdu {
                timestamp: Timestamp {
                    milliseconds: 0,
                    seconds: 0,
                    minutes: 0,
                    hours: 0,
                },
                frame_id,
            })) {
                raw_pdus.extend_from_slice(&data);
            }
        }

        // WireToSurface1 per dirty rect with Codec1Type::Uncompressed
        let mut total_bytes = 0usize;
        for rect in &update.rects {
            if rect.width == 0 || rect.height == 0 {
                continue;
            }

            let rect_bytes = rect.pixel_data.len();
            info!(
                x = rect.x,
                y = rect.y,
                width = rect.width,
                height = rect.height,
                bytes = rect_bytes,
                expected_bytes = u32::from(rect.width) * u32::from(rect.height) * 4,
                "GFX uncompressed rect"
            );

            let bitmap_data = Self::bgra_to_xrgb_bytes(&rect.pixel_data);

            if let Ok(data) = encode_vec(&ServerPdu::WireToSurface1(WireToSurface1Pdu {
                surface_id: 0,
                codec_id: Codec1Type::Uncompressed,
                pixel_format: GfxPixelFormat::XRgb,
                destination_rectangle: InclusiveRectangle {
                    left: rect.x,
                    top: rect.y,
                    right: rect.x + rect.width,
                    bottom: rect.y + rect.height,
                },
                bitmap_data,
            })) {
                total_bytes += rect.pixel_data.len();
                raw_pdus.extend_from_slice(&data);
            }
        }

        if let Some(frame_id) = frame_id {
            if let Ok(data) = encode_vec(&ServerPdu::EndFrame(EndFramePdu { frame_id })) {
                raw_pdus.extend_from_slice(&data);
            }
        }

        debug!(
            frame_id = frame_id.unwrap_or(0),
            rects = update.rects.len(),
            raw_bytes = total_bytes,
            "GFX uncompressed frame PDU created",
        );

        wrap_zgfx(&raw_pdus)
    }
}

impl DvcProcessor for GfxHandler {
    fn channel_name(&self) -> &str {
        GFX_CHANNEL_NAME
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        info!(channel_id, "GFX channel opened");
        let mut state = self.state.lock().unwrap();
        state.channel_id = Some(channel_id);
        Ok(Vec::new())
    }

    fn process(&mut self, _channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        // Client GFX data is also ZGFX-wrapped. Unwrap the ZGFX layer first.
        let raw_data = unwrap_zgfx(payload);
        let data = raw_data.as_deref().unwrap_or(payload);

        let client_pdu: ClientPdu = match decode(data) {
            Ok(pdu) => pdu,
            Err(e) => {
                // Unknown PDU type (e.g., QoE FrameAcknowledge 0x16, CacheImportOffer 0x10)
                // Log and ignore rather than crashing the connection
                debug!(
                    payload_len = data.len(),
                    first_bytes = ?&data[..data.len().min(8)],
                    "GFX: ignoring unknown client PDU: {e}"
                );
                return Ok(Vec::new());
            }
        };

        match client_pdu {
            ClientPdu::CapabilitiesAdvertise(caps) => {
                let cap_sets = &caps.0;
                info!("GFX client capabilities: {} sets", cap_sets.len());

                let mut state = self.state.lock().unwrap();

                // If already confirmed, ignore duplicate CapabilitiesAdvertise
                if state.caps_confirmed {
                    info!("GFX caps already confirmed, ignoring duplicate CapabilitiesAdvertise");
                    return Ok(Vec::new());
                }

                let (confirmed, selection) = choose_confirmed_capability(cap_sets);
                state.avc420_supported = selection.avc420;
                state.avc444_supported = selection.avc444;
                state.bitmap_fallback_mode = should_prefer_bitmap_fallback(selection);
                state.avc_fallback_to_uncompressed = false;

                debug!(
                    offered_caps = ?cap_sets,
                    selected_cap = ?confirmed,
                    selected_rank = selection.rank,
                    selected_avc420 = selection.avc420,
                    selected_avc444 = selection.avc444,
                    bitmap_fallback_mode = state.bitmap_fallback_mode,
                    "GFX capability selection"
                );

                info!(
                    confirmed_cap = ?confirmed,
                    avc420 = state.avc420_supported,
                    avc444_client = state.avc444_supported,
                    avc444_enabled = state.avc444_enabled,
                    experimental_browser_avc420 = state.browser_avc420,
                    bitmap_fallback_mode = state.bitmap_fallback_mode,
                    effective_mode = if state.bitmap_mode() {
                        "bitmap-fallback-preferred"
                    } else if state.use_avc444() {
                        "gfx-avc444"
                    } else if state.avc420_supported {
                        "gfx-avc420"
                    } else {
                        "gfx-uncompressed-only"
                    },
                    "GFX capabilities negotiated"
                );

                // Send CapabilitiesConfirm from the DVC handler so it goes through
                // DrdynvcServer's proper encoding path. Some clients still need
                // session-local fallback to classic bitmap updates even after GFX
                // negotiation succeeds.
                state.confirmed_cap = Some(confirmed.clone());
                state.caps_confirmed = true;

                if state.bitmap_fallback_mode {
                    info!(
                        confirmed_cap = ?confirmed,
                        "GFX negotiated without explicit AVC support; preferring classic bitmap updates for client compatibility"
                    );
                }

                let confirm_pdu = ServerPdu::CapabilitiesConfirm(CapabilitiesConfirmPdu(confirmed));
                let msg = make_dvc_message(&confirm_pdu)?;
                info!("GFX CapabilitiesConfirm sent via DVC handler");
                Ok(vec![msg])
            }

            ClientPdu::FrameAcknowledge(ack) => {
                let mut state = self.state.lock().unwrap();
                let had_send_time = state.frame_send_times.contains_key(&ack.frame_id);
                state.acknowledge_frame(ack.frame_id, ack.queue_depth, ack.total_frames_decoded);

                debug!(
                    ack_frame_id = ack.frame_id,
                    tracked = had_send_time,
                    pending_acks = state.pending_acks,
                    last_ack_frame = state.last_ack_frame,
                    acknowledgements_suspended = state.frame_acknowledgements_suspended,
                    client_queue_depth_bytes = state.client_queue_depth_bytes,
                    total_frames_decoded = state.client_total_frames_decoded,
                    rtt_ewma_ms = state.rtt_ewma_ms,
                    "GFX frame acknowledged"
                );

                // Log stats every 60 acked frames (~1 second at 60fps)
                if ack.frame_id % 60 == 0 {
                    // Compute instant bitrate stats from samples since last log
                    let n = state.bitrate_samples.len() as f64;
                    let (inst_avg, inst_std) = if n > 0.0 {
                        let sum: f64 = state.bitrate_samples.iter().sum();
                        let avg = sum / n;
                        let var: f64 = state
                            .bitrate_samples
                            .iter()
                            .map(|x| (x - avg).powi(2))
                            .sum::<f64>()
                            / n;
                        (avg, var.sqrt())
                    } else {
                        (0.0, 0.0)
                    };
                    let inst_max = if state.bitrate_max > 0.0 {
                        state.bitrate_max
                    } else {
                        0.0
                    };
                    let inst_min = if state.bitrate_min < f64::MAX {
                        state.bitrate_min
                    } else {
                        0.0
                    };

                    info!(
                        "ACK turnaround {:.1}ms | encode {:.1}ms | instant {:.1}/{:.1}/{:.1} Mbps (avg/max/min) | std {:.1} | {}KB/f | {} pending",
                        state.rtt_ewma_ms,
                        state.last_encode_ms,
                        inst_avg,
                        inst_max,
                        inst_min,
                        inst_std,
                        state.last_frame_bytes / 1024,
                        state.pending_acks,
                    );

                    // Reset for next window
                    state.bitrate_samples.clear();
                    state.bitrate_max = 0.0;
                    state.bitrate_min = f64::MAX;
                }
                Ok(Vec::new())
            }
        }
    }

    fn close(&mut self, channel_id: u32) {
        info!(channel_id, "GFX channel closed");
        let mut state = self.state.lock().unwrap();
        let keep_session_uses_gfx = state.session_uses_gfx;
        state.reset_connection_state(keep_session_uses_gfx);
    }
}

impl DvcServerProcessor for GfxHandler {}

#[cfg(test)]
#[path = "gfx_browser_tests.rs"]
mod browser_tests;

#[cfg(test)]
mod tests {
    use super::{
        choose_confirmed_capability, should_prefer_bitmap_fallback, CapabilitySelection, GfxState,
    };
    use ironrdp_pdu::rdp::vc::dvc::gfx::{
        CapabilitiesV107Flags, CapabilitiesV10Flags, CapabilitiesV81Flags, CapabilitiesV8Flags,
        CapabilitySet,
    };

    fn assert_surface_recreated(bytes: &[u8], width: u16, height: u16) {
        use ironrdp_core::{Decode, ReadCursor};
        use ironrdp_pdu::rdp::vc::dvc::gfx::ServerPdu;

        let raw = super::unwrap_zgfx(bytes).unwrap();
        let mut cursor = ReadCursor::new(&raw);
        assert!(matches!(ServerPdu::decode(&mut cursor).unwrap(),
            ServerPdu::DeleteSurface(pdu) if pdu.surface_id == 0));
        assert!(matches!(ServerPdu::decode(&mut cursor).unwrap(),
            ServerPdu::ResetGraphics(pdu)
                if pdu.width == u32::from(width) && pdu.height == u32::from(height)));
        assert!(matches!(ServerPdu::decode(&mut cursor).unwrap(),
            ServerPdu::CreateSurface(pdu)
                if pdu.surface_id == 0 && pdu.width == width && pdu.height == height));
    }

    #[test]
    fn reactivation_deletes_old_surface_before_reusing_its_id() {
        let mut state = GfxState::new(1920, 1200, false, false);
        state.surface_created = true;
        state.channel_id = Some(7);
        state.caps_confirmed = true;
        state.avc420_supported = true;
        state.invalidate_surface();
        state.width = 1280;
        state.height = 720;
        let frame = crate::display::GfxUncompressedUpdate {
            rects: Vec::new(),
            width: 1280,
            height: 720,
        };
        let bytes = super::GfxHandler::create_uncompressed_pdu(&mut state, &frame);
        assert_surface_recreated(&bytes, 1280, 720);
        assert_eq!(state.channel_id, Some(7));
        assert!(state.is_ready());
        assert!(!state.surface_delete_pending);
    }

    #[test]
    fn avc_frame_resize_deletes_old_surface_before_reusing_its_id() {
        let mut state = GfxState::new(1920, 1200, false, false);
        state.surface_created = true;
        let frame = crate::display::GfxFrameUpdate {
            h264_data: bytes::Bytes::new(),
            width: 1280,
            height: 720,
            enc_width: 1280,
            enc_height: 720,
            is_keyframe: true,
            h264_aux: None,
        };
        let bytes = super::GfxHandler::create_frame_pdu(&mut state, &frame);
        assert_surface_recreated(&bytes, 1280, 720);
    }

    #[test]
    fn uncompressed_frame_resize_deletes_old_surface_before_reusing_its_id() {
        let mut state = GfxState::new(1920, 1200, false, false);
        state.surface_created = true;
        let frame = crate::display::GfxUncompressedUpdate {
            rects: Vec::new(),
            width: 1280,
            height: 720,
        };
        let bytes = super::GfxHandler::create_uncompressed_pdu(&mut state, &frame);
        assert_surface_recreated(&bytes, 1280, 720);
    }

    #[test]
    fn first_graphics_frame_does_not_delete_a_nonexistent_surface() {
        use ironrdp_core::{Decode, ReadCursor};
        use ironrdp_pdu::rdp::vc::dvc::gfx::ServerPdu;

        let mut state = GfxState::new(1280, 720, false, false);
        let frame = crate::display::GfxUncompressedUpdate {
            rects: Vec::new(),
            width: 1280,
            height: 720,
        };
        let bytes = super::GfxHandler::create_uncompressed_pdu(&mut state, &frame);
        let raw = super::unwrap_zgfx(&bytes).unwrap();
        assert!(matches!(
            ServerPdu::decode(&mut ReadCursor::new(&raw)).unwrap(),
            ServerPdu::ResetGraphics(_)
        ));
    }

    #[test]
    fn prefers_avc_capability_over_plain_v8_when_both_are_offered() {
        let caps = vec![
            CapabilitySet::V8 {
                flags: CapabilitiesV8Flags::THIN_CLIENT,
            },
            CapabilitySet::V8_1 {
                flags: CapabilitiesV81Flags::AVC420_ENABLED,
            },
        ];

        let (confirmed, selection) = choose_confirmed_capability(&caps);

        assert!(matches!(
            confirmed,
            CapabilitySet::V8_1 { flags } if flags.contains(CapabilitiesV81Flags::AVC420_ENABLED)
        ));
        assert_eq!(
            selection,
            CapabilitySelection {
                rank: 11,
                avc420: true,
                avc444: false,
            }
        );
    }

    #[test]
    fn does_not_treat_v10_1_as_explicit_avc_support() {
        let caps = vec![CapabilitySet::V10_1];

        let (confirmed, selection) = choose_confirmed_capability(&caps);

        assert!(matches!(confirmed, CapabilitySet::V10_1));
        assert_eq!(
            selection,
            CapabilitySelection {
                rank: 6,
                avc420: false,
                avc444: false,
            }
        );
        assert!(should_prefer_bitmap_fallback(selection));
    }

    #[test]
    fn prefers_non_avc_v81_over_ambiguous_v10_1() {
        let caps = vec![
            CapabilitySet::V8 {
                flags: CapabilitiesV8Flags::THIN_CLIENT,
            },
            CapabilitySet::V8_1 {
                flags: CapabilitiesV81Flags::THIN_CLIENT,
            },
            CapabilitySet::V10_1,
        ];

        let (confirmed, selection) = choose_confirmed_capability(&caps);

        assert!(matches!(confirmed, CapabilitySet::V8_1 { .. }));
        assert_eq!(
            selection,
            CapabilitySelection {
                rank: 7,
                avc420: false,
                avc444: false,
            }
        );
        assert!(should_prefer_bitmap_fallback(selection));
    }

    #[test]
    fn prefers_v81_over_newer_non_avc_caps_for_compatibility() {
        let caps = vec![
            CapabilitySet::V8_1 {
                flags: CapabilitiesV81Flags::THIN_CLIENT,
            },
            CapabilitySet::V10 {
                flags: CapabilitiesV10Flags::AVC_DISABLED,
            },
            CapabilitySet::V10_7 {
                flags: CapabilitiesV107Flags::AVC_DISABLED,
            },
        ];

        let (confirmed, selection) = choose_confirmed_capability(&caps);

        assert!(matches!(confirmed, CapabilitySet::V8_1 { .. }));
        assert_eq!(
            selection,
            CapabilitySelection {
                rank: 7,
                avc420: false,
                avc444: false,
            }
        );
        assert!(should_prefer_bitmap_fallback(selection));
    }

    #[test]
    fn keeps_avc_sessions_on_gfx_path() {
        let caps = vec![CapabilitySet::V8_1 {
            flags: CapabilitiesV81Flags::AVC420_ENABLED,
        }];

        let (_, selection) = choose_confirmed_capability(&caps);

        assert!(!should_prefer_bitmap_fallback(selection));
    }

    #[test]
    fn reset_connection_state_clears_bitmap_fallback_and_ack_tracking() {
        let mut state = GfxState::new(1920, 1080, true, false);
        state.session_uses_gfx = true;
        state.channel_id = Some(7);
        state.surface_created = true;
        state.caps_confirmed = true;
        state.frame_id = 41;
        state.avc420_supported = true;
        state.avc444_supported = true;
        state.confirmed_cap = Some(CapabilitySet::V10_1);
        state.pending_acks = 3;
        state.last_ack_frame = 39;
        state.network_quality = 0.15;
        state.frame_send_times.insert(41, std::time::Instant::now());
        state.rtt_ewma_ms = 87.0;
        state.last_encode_ms = 11.0;
        state.last_frame_bytes = 123_456;
        state.total_bytes_sent = 987_654;
        state.bitmap_wire_bytes = 456_789;
        state.bitmap_update_budget_bytes = Some(16_384);
        state
            .bitmap_budget
            .observe(32_768, 1000.0, std::time::Instant::now());
        state.uncompressed_dispatches = 4;
        state.uncompressed_prepare_ms_total = 7.0;
        state.uncompressed_write_ms_total = 8.0;
        state.uncompressed_wire_bytes = 500_000;
        state.bitmap_codec = Some("remotefx");
        state.negotiated_client_depth = Some(24);
        state.start_time = Some(std::time::Instant::now());
        state.peer_addr = Some(std::net::IpAddr::from([192, 168, 86, 250]));
        state.avc_fallback_to_uncompressed = true;
        state.bitmap_fallback_mode = true;

        state.reset_connection_state(false);

        assert!(!state.session_uses_gfx);
        assert_eq!(state.channel_id, None);
        assert!(!state.surface_created);
        assert!(!state.caps_confirmed);
        assert_eq!(state.frame_id, 0);
        assert!(!state.avc420_supported);
        assert!(!state.avc444_supported);
        assert_eq!(state.confirmed_cap, None);
        assert_eq!(state.pending_acks, 0);
        assert_eq!(state.last_ack_frame, 0);
        assert_eq!(state.network_quality, 1.0);
        assert!(state.frame_send_times.is_empty());
        assert_eq!(state.rtt_ewma_ms, 0.0);
        assert_eq!(state.last_encode_ms, 0.0);
        assert_eq!(state.last_frame_bytes, 0);
        assert_eq!(state.total_bytes_sent, 0);
        assert_eq!(state.bitmap_wire_bytes, 0);
        assert_eq!(state.bitmap_update_budget_bytes, None);
        assert_eq!(state.bitmap_budget.bytes(), 128 * 1024);
        assert_eq!(state.uncompressed_dispatches, 0);
        assert_eq!(state.uncompressed_prepare_ms_total, 0.0);
        assert_eq!(state.uncompressed_write_ms_total, 0.0);
        assert_eq!(state.uncompressed_wire_bytes, 0);
        assert_eq!(state.bitmap_codec, None);
        assert_eq!(state.negotiated_client_depth, None);
        assert_eq!(state.start_time, None);
        assert!(!state.avc_fallback_to_uncompressed);
        assert!(!state.bitmap_fallback_mode);
        assert_eq!(
            state.peer_addr,
            Some(std::net::IpAddr::from([192, 168, 86, 250]))
        );
    }

    #[test]
    fn suspended_acknowledgements_clear_tracking_and_resume_cleanly() {
        let mut state = GfxState::new(800, 600, false, false);
        let frame = state.next_frame_id();
        state.acknowledge_frame(frame, super::QueueDepth::Suspend, 1);
        assert!(state.frame_acknowledgements_suspended);
        for _ in 0..500 {
            state.next_frame_id();
            state.record_display_write(1024, 1.0);
        }
        assert_eq!(state.pending_acks, 0);
        assert!(state.frame_send_times.is_empty());
        assert!(state.bitrate_samples.len() <= 120);
        state.acknowledge_frame(state.frame_id, super::QueueDepth::AvailableBytes(4096), 500);
        assert!(!state.frame_acknowledgements_suspended);
        assert_eq!(state.client_queue_depth_bytes, Some(4096));
        assert_eq!(state.client_total_frames_decoded, 500);
        let frame = state.next_frame_id();
        assert_eq!(state.pending_acks, 1);
        state.acknowledge_frame(frame, super::QueueDepth::Unavailable, 501);
        assert_eq!(state.pending_acks, 0);
        assert_eq!(state.last_ack_frame, frame);
        state.reset_connection_state(true);
        assert!(!state.frame_acknowledgements_suspended);
        assert_eq!(state.client_total_frames_decoded, 0);
        assert_eq!(state.client_queue_depth_bytes, None);
    }

    #[test]
    fn duplicate_and_unknown_acks_do_not_remove_other_pending_frames() {
        let mut state = GfxState::new(800, 600, false, false);
        let first = state.next_frame_id();
        let second = state.next_frame_id();
        state.ack_frame(first);
        state.ack_frame(first);
        state.ack_frame(second + 1);
        assert_eq!(state.pending_acks, 1);
        assert!(state.frame_send_times.contains_key(&second));
        assert_eq!(state.last_ack_frame, first);
    }

    #[test]
    fn wire_accounting_starts_only_after_successful_delivery() {
        let mut state = GfxState::new(800, 600, false, false);
        state.last_frame_bytes = 999;
        state.next_frame_id();
        assert_eq!(state.total_bytes_sent, 0);
        assert_eq!(state.total_frames_sent, 0);
        assert!(state.start_time.is_none());
        state.record_display_write(1024, 2.5);
        assert_eq!(state.total_bytes_sent, 1024);
        assert_eq!(state.total_frames_sent, 1);
        assert_eq!(state.last_write_ms, 2.5);
        assert!(state.start_time.is_some());
    }
}

#[cfg(test)]
mod graphics_controls_tests {
    use super::*;

    #[tokio::test]
    async fn graphics_wakeup_retains_latest_policy_through_session_reset() {
        let mut state = GfxState::new(640, 480, true, false);
        let mut changed = state.controls_receiver();
        state.target_quality = Some("lossless".into());
        state.manual_bitmap_mode = true;
        state.encoder_pref = Some("bitmap".into());
        state.notify_controls_changed();
        changed.changed().await.unwrap();
        assert_eq!(*changed.borrow_and_update(), 1);
        state.reset_connection_state(true);
        assert_eq!(state.target_quality.as_deref(), Some("lossless"));
        assert_eq!(state.encoder_pref.as_deref(), Some("bitmap"));
        assert!(state.bitmap_mode());
        assert!(!changed.has_changed().unwrap());
        state.target_quality = Some("high_quality".into());
        state.manual_bitmap_mode = false;
        assert!(!state.bitmap_mode());
        state.bitmap_fallback_mode = true;
        assert!(
            state.bitmap_mode(),
            "user selection must not erase failure fallback"
        );
    }
}

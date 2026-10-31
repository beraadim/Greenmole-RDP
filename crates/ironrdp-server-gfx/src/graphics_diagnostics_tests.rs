use super::*;
use crate::display::GfxUncompressedUpdate;
use crate::gfx::{unwrap_zgfx, GfxHandler};
use ironrdp_core::{Decode, ReadCursor};
use ironrdp_pdu::rdp::vc::dvc::gfx::ServerPdu;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

fn frame() -> GfxFrameUpdate {
    GfxFrameUpdate {
        // Deliberately include a free-form canary in a synthetic picture. Only
        // NAL-header booleans and its length may reach the diagnostic event.
        h264_data: bytes::Bytes::from_static(
            b"\0\0\0\x01\x67private-video-canary\0\0\x01\x68\0\0\0\x01\x65\x42",
        ),
        width: 128,
        height: 64,
        enc_width: 128,
        enc_height: 64,
        is_keyframe: true,
        h264_aux: None,
    }
}

fn commands(bytes: &[u8]) -> Vec<ServerPdu> {
    let bytes = unwrap_zgfx(bytes).unwrap();
    let mut cursor = ReadCursor::new(&bytes);
    let mut pdus = Vec::new();
    while !cursor.is_empty() {
        pdus.push(ServerPdu::decode(&mut cursor).unwrap());
    }
    pdus
}

fn assert_setup(pdus: &[ServerPdu], frame_id: u32) {
    assert!(matches!(&pdus[0], ServerPdu::ResetGraphics(reset)
        if reset.width == 128 && reset.height == 64
        && reset.monitors.len() == 1 && reset.monitors[0].right == 127
        && reset.monitors[0].bottom == 63));
    assert!(matches!(&pdus[1], ServerPdu::CreateSurface(surface)
        if surface.surface_id == 0 && surface.width == 128 && surface.height == 64));
    assert!(matches!(&pdus[2], ServerPdu::MapSurfaceToOutput(map)
        if map.surface_id == 0 && map.output_origin_x == 0 && map.output_origin_y == 0));
    assert!(matches!(&pdus[3], ServerPdu::StartFrame(frame) if frame.frame_id == frame_id));
    assert!(
        matches!(pdus.last().unwrap(), ServerPdu::EndFrame(frame) if frame.frame_id == frame_id)
    );
}

#[test]
fn fresh_and_reconnected_avc_wire_has_complete_ordered_surface_setup() {
    let mut state = GfxState::new(128, 64, false, false);
    for _ in 0..2 {
        state.begin_connection(None);
        let first = GfxHandler::create_frame_pdu(&mut state, &frame());
        let pdus = commands(&first);
        assert_setup(&pdus, 1);
        assert_eq!(pdus.len(), 6);
        assert!(matches!(&pdus[4], ServerPdu::WireToSurface1(wire)
            if wire.surface_id == 0 && wire.destination_rectangle.right == 128
            && wire.destination_rectangle.bottom == 64));
        assert_eq!(state.startup_diagnostics.surface_generation, 1);
        assert!(!state.startup_diagnostics.first_avc_pending);

        let second = commands(&GfxHandler::create_frame_pdu(&mut state, &frame()));
        assert_eq!(second.len(), 3);
        assert!(matches!(&second[0], ServerPdu::StartFrame(frame) if frame.frame_id == 2));
    }
}

#[test]
fn raw_seed_then_avc_logs_first_video_without_recreating_surface() {
    let mut state = GfxState::new(128, 64, false, false);
    let raw = GfxHandler::create_uncompressed_pdu(
        &mut state,
        &GfxUncompressedUpdate {
            rects: Vec::new(),
            width: 128,
            height: 64,
        },
    );
    assert_setup(&commands(&raw), 1);
    assert!(state.startup_diagnostics.first_avc_pending);
    let avc = commands(&GfxHandler::create_frame_pdu(&mut state, &frame()));
    assert_eq!(avc.len(), 3);
    assert_eq!(state.startup_diagnostics.surface_generation, 1);
    assert!(!state.startup_diagnostics.first_avc_pending);

    state.invalidate_surface();
    let recreated = commands(&GfxHandler::create_frame_pdu(&mut state, &frame()));
    assert!(matches!(&recreated[0], ServerPdu::DeleteSurface(delete) if delete.surface_id == 0));
    assert_setup(&recreated[1..], 3);
    assert_eq!(state.startup_diagnostics.surface_generation, 2);
}

#[test]
fn nal_summary_accepts_both_annex_b_start_codes_and_ignores_payload() {
    assert_eq!(
        summarize_nals(&frame().h264_data),
        NalSummary {
            sps: true,
            pps: true,
            idr: true
        }
    );
    assert_eq!(
        summarize_nals(&[0, 0, 1, 0x41, 7, 8, 5]),
        NalSummary::default()
    );
    for bytes in [
        &[][..],
        &[0, 0, 1],
        &[0, 0, 0, 1],
        &[0xff, 0x67, 0x68, 0x65],
    ] {
        assert_eq!(summarize_nals(bytes), NalSummary::default());
    }
}

#[derive(Debug, PartialEq)]
enum Value {
    Number(u64),
    Boolean(bool),
    Text(String),
}

#[derive(Default)]
struct Fields(BTreeMap<String, Value>);
impl Visit for Fields {
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().into(), Value::Number(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().into(), Value::Boolean(value));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0
            .insert(field.name().into(), Value::Text(value.into()));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().into(), Value::Text(format!("{value:?}")));
    }
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<BTreeMap<String, Value>>>>);
impl Subscriber for Capture {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target() == "macrdp_transition"
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        self.0.lock().unwrap().push(fields.0);
    }
}

#[test]
fn startup_logging_is_bounded_numeric_and_payload_free() {
    let capture = Capture::default();
    tracing::subscriber::with_default(capture.clone(), || {
        let mut state = GfxState::new(128, 64, false, false);
        state.bitmap_dispatches = 1;
        state.total_frames_sent = 1;
        for _ in 0..10 {
            GfxHandler::create_frame_pdu(&mut state, &frame());
        }
        assert_eq!(capture.0.lock().unwrap().len(), 2);
        state.invalidate_surface();
        GfxHandler::create_frame_pdu(&mut state, &frame());
        assert_eq!(capture.0.lock().unwrap().len(), 4);
    });
    let events = capture.0.lock().unwrap();
    let video = &events[1];
    assert_eq!(video["bitmap_dispatches"], Value::Number(1));
    assert_eq!(video["display_updates_sent"], Value::Number(1));
    assert_eq!(video["main_has_sps"], Value::Boolean(true));
    assert_eq!(video["main_has_pps"], Value::Boolean(true));
    assert_eq!(video["main_has_idr"], Value::Boolean(true));
    assert_eq!(events[2]["surface_generation"], Value::Number(2));
    for event in events.iter() {
        for (field, value) in event {
            if let Value::Text(_) = value {
                assert!(matches!(
                    field.as_str(),
                    "component" | "phase" | "state" | "message"
                ));
            }
        }
    }
    assert!(!format!("{events:?}").contains("private-video-canary"));
}

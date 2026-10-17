//! Decode the actual wire stream to verify capture boundaries and compatibility.

use super::*;
use crate::{BitmapQuality, BitmapUpdate, PixelFormat, RdpServerDisplayUpdates};
use core::num::{NonZeroU16, NonZeroUsize};
use ironrdp_core::{decode_cursor, ReadCursor};
use ironrdp_pdu::fast_path::{FastPathHeader, FastPathUpdatePdu, Fragmentation, UpdateCode};
use ironrdp_pdu::surface_commands::{FrameAction, SurfaceCommand};

struct Writes(Rc<std::cell::RefCell<Vec<Vec<u8>>>>);
impl FramedWrite for Writes {
    type WriteAllFut<'write> = std::future::Ready<std::io::Result<()>>;
    fn write_all<'a>(&'a mut self, data: &'a [u8]) -> Self::WriteAllFut<'a> {
        self.0.borrow_mut().push(data.to_vec());
        std::future::ready(Ok(()))
    }
}

#[derive(Default)]
struct Regions {
    pending: bool,
    pointer_polls: usize,
}
#[async_trait::async_trait]
impl RdpServerDisplayUpdates for Regions {
    fn bitmap_frame_pending(&self) -> bool {
        self.pending
    }
    fn poll_pointer_update(&mut self) -> Option<DisplayUpdate> {
        self.pointer_polls += 1;
        (self.pointer_polls == 1).then_some(DisplayUpdate::HidePointer)
    }
    async fn next_update(&mut self) -> Result<Option<DisplayUpdate>> {
        panic!("dispatch must not consume another capture")
    }
}

fn bitmap(x: u16, width: u16, height: u16) -> BitmapUpdate {
    let pixels: Vec<u8> = (0..usize::from(width) * usize::from(height) * 4)
        .map(|i| (i.wrapping_mul(113) >> 3) as u8)
        .collect();
    BitmapUpdate {
        x,
        y: 0,
        width: NonZeroU16::new(width).unwrap(),
        height: NonZeroU16::new(height).unwrap(),
        format: PixelFormat::BgrA32,
        data: pixels.into(),
        stride: NonZeroUsize::new(usize::from(width) * 4).unwrap(),
        dirty_rects: Vec::new(),
    }
}

fn encoder(flags: CmdFlags, quality: BitmapQuality) -> UpdateEncoder {
    let mut codecs = UpdateEncoderCodecs::new();
    codecs.set_quality(quality);
    codecs.set_max_request_size(16_384);
    UpdateEncoder::new(
        DesktopSize {
            width: 256,
            height: 128,
        },
        flags,
        codecs,
    )
    .unwrap()
}

/// Reassemble each complete update before inspecting its command, exactly as a
/// receiver must. Pointer PDUs are valid only between complete updates.
fn commands(packets: &[Vec<u8>]) -> Vec<(UpdateCode, Vec<u8>)> {
    let mut complete = Vec::new();
    let mut partial: Option<(UpdateCode, Vec<u8>)> = None;
    for packet in packets {
        let mut cursor = ReadCursor::new(packet);
        let _: FastPathHeader = decode_cursor(&mut cursor).unwrap();
        let update: FastPathUpdatePdu<'_> = decode_cursor(&mut cursor).unwrap();
        assert!(cursor.is_empty());
        match update.fragmentation {
            Fragmentation::Single => {
                assert!(partial.is_none());
                complete.push((update.update_code, update.data.to_vec()));
            }
            Fragmentation::First => {
                assert!(partial.is_none());
                partial = Some((update.update_code, update.data.to_vec()));
            }
            Fragmentation::Next | Fragmentation::Last => {
                let (code, bytes) = partial.as_mut().unwrap();
                assert_eq!(*code, update.update_code);
                bytes.extend_from_slice(update.data);
                if update.fragmentation == Fragmentation::Last {
                    complete.push(partial.take().unwrap());
                }
            }
        }
    }
    assert!(partial.is_none());
    complete
}

fn markers(updates: &[(UpdateCode, Vec<u8>)]) -> Vec<(usize, FrameAction, u32)> {
    updates
        .iter()
        .enumerate()
        .filter_map(|(i, (code, data))| {
            if *code != UpdateCode::SurfaceCommands {
                return None;
            }
            match decode::<SurfaceCommand<'_>>(data).unwrap() {
                SurfaceCommand::FrameMarker(marker) => {
                    Some((i, marker.frame_action, marker.frame_id.unwrap()))
                }
                _ => None,
            }
        })
        .collect()
}

async fn dispatch(
    encoder: UpdateEncoder,
    update: DisplayUpdate,
    regions: &mut Regions,
    packets: Rc<std::cell::RefCell<Vec<Vec<u8>>>>,
) -> UpdateEncoder {
    let state = Arc::new(std::sync::Mutex::new(GfxState::new(256, 128, false, false)));
    dispatch_in_state(encoder, update, regions, packets, &state).await
}

async fn dispatch_in_state(
    encoder: UpdateEncoder,
    update: DisplayUpdate,
    regions: &mut Regions,
    packets: Rc<std::cell::RefCell<Vec<Vec<u8>>>>,
    state: &Arc<std::sync::Mutex<GfxState>>,
) -> UpdateEncoder {
    let mut native = Writes(packets);
    let mut writer = SharedWriter::new(&mut native, Duration::from_secs(2));
    RdpServer::dispatch_display_update(
        update,
        &mut writer,
        1002,
        1003,
        &mut vec![0; 4096],
        encoder,
        &state,
        None,
        regions,
    )
    .await
    .unwrap()
    .1
}

#[tokio::test]
async fn negotiated_markers_enclose_every_partition_and_pointer_of_a_capture() {
    let packets = Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut regions = Regions::default();
    let _ = dispatch(
        encoder(
            CmdFlags::SET_SURFACE_BITS | CmdFlags::FRAME_MARKER,
            BitmapQuality::HighQuality,
        ),
        DisplayUpdate::Bitmap(bitmap(0, 256, 128)),
        &mut regions,
        packets.clone(),
    )
    .await;
    let updates = commands(&packets.borrow());
    let markers = markers(&updates);
    assert_eq!(
        markers,
        [
            (0, FrameAction::Begin, 1),
            (updates.len() - 1, FrameAction::End, 1)
        ]
    );
    assert!(
        updates.len() > 4,
        "fixture must require multiple partitions"
    );
    assert!(updates
        .iter()
        .any(|(code, _)| *code == UpdateCode::HiddenPointer));
}

#[tokio::test]
async fn one_capture_marker_spans_multiple_lazy_display_regions() {
    let packets = Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut regions = Regions {
        pending: true,
        ..Default::default()
    };
    let encoder = dispatch(
        encoder(
            CmdFlags::SET_SURFACE_BITS | CmdFlags::FRAME_MARKER,
            BitmapQuality::HighQuality,
        ),
        DisplayUpdate::Bitmap(bitmap(0, 128, 128)),
        &mut regions,
        packets.clone(),
    )
    .await;
    assert_eq!(markers(&commands(&packets.borrow())).len(), 1);
    regions.pending = false;
    let _ = dispatch(
        encoder,
        DisplayUpdate::Bitmap(bitmap(128, 128, 128)),
        &mut regions,
        packets.clone(),
    )
    .await;
    let updates = commands(&packets.borrow());
    assert_eq!(
        markers(&updates),
        [
            (0, FrameAction::Begin, 1),
            (updates.len() - 1, FrameAction::End, 1)
        ]
    );
}

#[tokio::test]
async fn legacy_bitmap_and_clients_without_marker_capability_receive_no_markers() {
    for (flags, quality, expected) in [
        (CmdFlags::all(), BitmapQuality::Lossless, UpdateCode::Bitmap),
        (
            CmdFlags::SET_SURFACE_BITS,
            BitmapQuality::HighQuality,
            UpdateCode::SurfaceCommands,
        ),
        (
            CmdFlags::FRAME_MARKER,
            BitmapQuality::HighQuality,
            UpdateCode::Bitmap,
        ),
    ] {
        let packets = Rc::new(std::cell::RefCell::new(Vec::new()));
        let _ = dispatch(
            encoder(flags, quality),
            DisplayUpdate::Bitmap(bitmap(0, 256, 128)),
            &mut Regions::default(),
            packets.clone(),
        )
        .await;
        let updates = commands(&packets.borrow());
        assert!(markers(&updates).is_empty());
        assert!(updates.iter().any(|(code, _)| *code == expected));
    }
}

#[tokio::test]
async fn unchanged_capture_sends_neither_pixels_nor_empty_marker_pair() {
    let packets = Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut regions = Regions::default();
    let frame = bitmap(0, 256, 128);
    let encoder = dispatch(
        encoder(CmdFlags::all(), BitmapQuality::HighQuality),
        DisplayUpdate::Bitmap(frame.clone()),
        &mut regions,
        packets.clone(),
    )
    .await;
    packets.borrow_mut().clear();
    let _ = dispatch(
        encoder,
        DisplayUpdate::Bitmap(frame),
        &mut regions,
        packets.clone(),
    )
    .await;
    assert!(packets.borrow().is_empty());
}

#[tokio::test]
async fn abandoning_capture_closes_marker_before_next_pointer_update() {
    let packets = Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut regions = Regions {
        pending: true,
        ..Default::default()
    };
    let encoder = dispatch(
        encoder(CmdFlags::all(), BitmapQuality::HighQuality),
        DisplayUpdate::Bitmap(bitmap(0, 128, 128)),
        &mut regions,
        packets.clone(),
    )
    .await;
    regions.pending = false;
    let _ = dispatch(
        encoder,
        DisplayUpdate::DefaultPointer,
        &mut regions,
        packets.clone(),
    )
    .await;
    let updates = commands(&packets.borrow());
    let marks = markers(&updates);
    assert_eq!(marks[1], (updates.len() - 2, FrameAction::End, 1));
    assert_eq!(updates.last().unwrap().0, UpdateCode::DefaultPointer);
}

#[tokio::test]
async fn resize_closes_open_capture_before_deactivation() {
    let packets = Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut regions = Regions {
        pending: true,
        ..Default::default()
    };
    let encoder = dispatch(
        encoder(CmdFlags::all(), BitmapQuality::HighQuality),
        DisplayUpdate::Bitmap(bitmap(0, 128, 128)),
        &mut regions,
        packets.clone(),
    )
    .await;
    packets.borrow_mut().clear();
    // Resize wins even if the provider has not cleared its old capture queue.
    let mut encoder = dispatch(
        encoder,
        DisplayUpdate::Resize(DesktopSize {
            width: 128,
            height: 128,
        }),
        &mut regions,
        packets.clone(),
    )
    .await;
    let packets = packets.borrow();
    assert_eq!(packets.len(), 2);
    let first = commands(&packets[..1]);
    assert_eq!(markers(&first), [(0, FrameAction::End, 1)]);
    assert!(encoder.end_bitmap_frame().unwrap().is_none());
}

#[tokio::test]
async fn unchanged_final_region_closes_existing_frame_without_another_begin() {
    let packets = Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut regions = Regions {
        pending: true,
        ..Default::default()
    };
    let frame = bitmap(0, 256, 128);
    let encoder = dispatch(
        encoder(CmdFlags::all(), BitmapQuality::HighQuality),
        DisplayUpdate::Bitmap(frame.clone()),
        &mut regions,
        packets.clone(),
    )
    .await;
    packets.borrow_mut().clear();
    regions.pending = false;
    let _ = dispatch(
        encoder,
        DisplayUpdate::Bitmap(frame),
        &mut regions,
        packets.clone(),
    )
    .await;
    let updates = commands(&packets.borrow());
    assert_eq!(updates.len(), 1);
    assert_eq!(markers(&updates), [(0, FrameAction::End, 1)]);
}

#[tokio::test]
async fn live_lossless_change_closes_prior_surface_frame_before_bitmap_and_restores_monotonic_markers(
) {
    let packets = Rc::new(std::cell::RefCell::new(Vec::new()));
    let state = Arc::new(std::sync::Mutex::new(GfxState::new(256, 128, false, false)));
    let mut regions = Regions {
        pending: true,
        ..Default::default()
    };
    let mut codecs = UpdateEncoderCodecs::new();
    codecs.set_remotefx(Some((
        rdp::capability_sets::EntropyBits::Rlgr3,
        7,
        ironrdp_pdu::codecs::rfx::OperatingMode::IMAGE_MODE,
    )));
    let encoder = UpdateEncoder::new(
        DesktopSize {
            width: 256,
            height: 128,
        },
        CmdFlags::SET_SURFACE_BITS | CmdFlags::FRAME_MARKER,
        codecs,
    )
    .unwrap();
    let encoder = dispatch_in_state(
        encoder,
        DisplayUpdate::Bitmap(bitmap(0, 128, 128)),
        &mut regions,
        packets.clone(),
        &state,
    )
    .await;
    assert_eq!(markers(&commands(&packets.borrow())).len(), 1);
    state.lock().unwrap().target_quality = Some("lossless".into());
    regions.pending = false;
    let encoder = dispatch_in_state(
        encoder,
        DisplayUpdate::Bitmap(bitmap(0, 256, 128)),
        &mut regions,
        packets.clone(),
        &state,
    )
    .await;
    assert_eq!(encoder.bitmap_codec(), "rdp6-rle");
    let wire = commands(&packets.borrow());
    let boundary = markers(&wire);
    assert_eq!(boundary.len(), 2);
    assert_eq!(boundary[0].1, FrameAction::Begin);
    assert_eq!(boundary[1].1, FrameAction::End);
    assert_eq!(boundary[0].2, boundary[1].2);
    assert!(wire[boundary[1].0 + 1..]
        .iter()
        .all(|(code, _)| *code == UpdateCode::Bitmap));
    assert_eq!(state.lock().unwrap().bitmap_codec, Some("rdp6-rle"));
    state.lock().unwrap().target_quality = Some("high_quality".into());
    let encoder = dispatch_in_state(
        encoder,
        DisplayUpdate::Bitmap(bitmap(0, 256, 128)),
        &mut regions,
        packets.clone(),
        &state,
    )
    .await;
    assert_eq!(encoder.bitmap_codec(), "remotefx");
    let wire = commands(&packets.borrow());
    let boundary = markers(&wire);
    assert_eq!(boundary.len(), 4);
    assert_eq!(boundary[2].1, FrameAction::Begin);
    assert_eq!(boundary[3].1, FrameAction::End);
    assert_eq!(boundary[2].2, boundary[3].2);
    assert!(boundary[2].2 > boundary[0].2);
}

#[tokio::test]
async fn already_produced_gfx_updates_finish_in_order_after_an_image_policy_request() {
    use crate::{GfxFrameUpdate, GfxUncompressedUpdate, UncompressedRect};
    for update in [
        DisplayUpdate::GfxFrame(GfxFrameUpdate {
            h264_data: bytes::Bytes::from_static(&[0, 0, 0, 1, 0x65, 1, 2, 3]),
            width: 64,
            height: 64,
            enc_width: 64,
            enc_height: 64,
            is_keyframe: true,
            h264_aux: None,
        }),
        DisplayUpdate::GfxUncompressed(GfxUncompressedUpdate {
            width: 64,
            height: 64,
            rects: vec![UncompressedRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                pixel_data: bytes::Bytes::from_static(&[1, 2, 3, 255]),
            }],
        }),
    ] {
        let state = Arc::new(std::sync::Mutex::new(GfxState::new(64, 64, false, false)));
        {
            let mut state = state.lock().unwrap();
            state.channel_id = Some(5);
            state.caps_confirmed = true;
            state.avc420_supported = true;
            state.manual_bitmap_mode = true;
            state.target_quality = Some("lossless".into());
        }
        let packets = Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut native = Writes(packets.clone());
        let mut writer = SharedWriter::new(&mut native, Duration::from_secs(2));
        RdpServer::dispatch_display_update(
            update,
            &mut writer,
            1002,
            1003,
            &mut vec![0; 4096],
            encoder(CmdFlags::SET_SURFACE_BITS, BitmapQuality::Balanced),
            &state,
            Some(1004),
            &mut Regions::default(),
        )
        .await
        .unwrap();
        assert!(
            !packets.borrow().is_empty(),
            "discarding a produced AU can poison the next P-frame after a rapid policy reversal"
        );
        assert_eq!(state.lock().unwrap().frame_id, 1);
    }
}

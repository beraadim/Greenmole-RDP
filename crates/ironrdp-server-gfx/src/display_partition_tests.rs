//! Pointer delivery may interleave complete updates, never Fast-Path fragments.
use super::*;
use crate::{BitmapUpdate, PixelFormat, RdpServerDisplayUpdates};
use core::num::{NonZeroU16, NonZeroUsize};
use ironrdp_core::{decode_cursor, ReadCursor};
use ironrdp_pdu::fast_path::{FastPathHeader, FastPathUpdatePdu, Fragmentation, UpdateCode};

struct RecordedWrites(Rc<std::cell::RefCell<Vec<Vec<u8>>>>);
impl FramedWrite for RecordedWrites {
    type WriteAllFut<'write> = std::future::Ready<std::io::Result<()>>;
    fn write_all<'a>(&'a mut self, bytes: &'a [u8]) -> Self::WriteAllFut<'a> {
        self.0.borrow_mut().push(bytes.to_vec());
        std::future::ready(Ok(()))
    }
}

#[derive(Default)]
struct PointerChanges {
    polls: usize,
}
#[async_trait::async_trait]
impl RdpServerDisplayUpdates for PointerChanges {
    fn poll_pointer_update(&mut self) -> Option<DisplayUpdate> {
        self.polls += 1;
        match self.polls {
            1 => Some(DisplayUpdate::HidePointer),
            2 => Some(DisplayUpdate::DefaultPointer),
            _ => None,
        }
    }
    async fn next_update(&mut self) -> Result<Option<DisplayUpdate>> {
        panic!("A partition boundary must not consume another capture");
    }
}

#[tokio::test]
async fn full_frame_delivers_shape_and_hide_between_complete_partitions() {
    let width = 256;
    let height = 128;
    let mut seed = 0x1234_5678_u32;
    let mut pixels = Vec::new();
    for _ in 0..width * height {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        pixels.extend_from_slice(&seed.to_le_bytes());
    }
    let bitmap = BitmapUpdate {
        x: 0,
        y: 0,
        width: NonZeroU16::new(width).unwrap(),
        height: NonZeroU16::new(height).unwrap(),
        format: PixelFormat::BgrA32,
        data: pixels.into(),
        stride: NonZeroUsize::new(usize::from(width) * 4).unwrap(),
        dirty_rects: Vec::new(),
    };
    let mut codecs = UpdateEncoderCodecs::new();
    codecs.set_remotefx(Some((
        rdp::capability_sets::EntropyBits::Rlgr3,
        7,
        ironrdp_pdu::codecs::rfx::OperatingMode::IMAGE_MODE,
    )));
    codecs.set_quality(crate::BitmapQuality::HighQuality);
    codecs.set_max_request_size(32_768);
    let encoder = UpdateEncoder::new(
        DesktopSize { width, height },
        CmdFlags::SET_SURFACE_BITS,
        codecs,
    )
    .unwrap();
    let state = Arc::new(std::sync::Mutex::new(GfxState::new(
        width, height, false, false,
    )));
    let writes = Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut native = RecordedWrites(writes.clone());
    let mut writer = SharedWriter::new(&mut native, Duration::from_secs(10));
    let mut pointer = PointerChanges::default();
    let (_, mut encoder) = RdpServer::dispatch_display_update(
        DisplayUpdate::Bitmap(bitmap.clone()),
        &mut writer,
        1002,
        1003,
        &mut vec![0; 4096],
        encoder,
        &state,
        None,
        &mut pointer,
    )
    .await
    .unwrap();
    let mut assembling = false;
    let mut fragmented = false;
    let mut partitions = 0;
    let mut bitmap_bytes = 0;
    let mut pointer_positions = Vec::new();
    for packet in writes.borrow().iter() {
        let mut cursor = ReadCursor::new(packet);
        let _: FastPathHeader = decode_cursor(&mut cursor).unwrap();
        let update: FastPathUpdatePdu<'_> = decode_cursor(&mut cursor).unwrap();
        assert!(cursor.is_empty());
        if matches!(
            update.update_code,
            UpdateCode::HiddenPointer | UpdateCode::DefaultPointer
        ) {
            assert!(!assembling, "Pointer interrupted bitmap reassembly");
            assert!(matches!(update.fragmentation, Fragmentation::Single));
            pointer_positions.push(partitions);
            continue;
        }
        bitmap_bytes += packet.len();
        match update.fragmentation {
            Fragmentation::Single => {
                assert!(!assembling);
                partitions += 1;
            }
            Fragmentation::First => {
                assert!(!assembling);
                assembling = true;
                fragmented = true;
            }
            Fragmentation::Next => assert!(assembling),
            Fragmentation::Last => {
                assert!(assembling);
                assembling = false;
                partitions += 1;
            }
        }
    }
    assert!(!assembling && fragmented && partitions > 2);
    assert_eq!(pointer_positions, [1, 2]);
    assert_eq!(pointer.polls, partitions);
    let stats = state.lock().unwrap();
    assert_eq!(stats.bitmap_wire_bytes, bitmap_bytes as u64);
    assert_eq!(stats.bitmap_dispatches, 1);
    drop(stats);
    // Interleaving must preserve the committed full-desktop diff baseline.
    assert!(encoder
        .update(DisplayUpdate::Bitmap(bitmap))
        .next()
        .await
        .is_none());
}

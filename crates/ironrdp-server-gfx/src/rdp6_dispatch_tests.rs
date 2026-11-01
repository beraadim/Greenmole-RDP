//! Exercise RDP6 full-image coverage through the server writer and pointer polls.

use super::*;
use crate::encoder::rdp6_transport_tests::{decode_rectangle, fullscreen_pixels};
use crate::{BitmapUpdate, PixelFormat, RdpServerDisplayUpdates};
use core::num::{NonZeroU16, NonZeroUsize};
use ironrdp_core::{decode_cursor, ReadCursor};
use ironrdp_pdu::bitmap::BitmapUpdateData;
use ironrdp_pdu::fast_path::{FastPathHeader, FastPathUpdatePdu, Fragmentation, UpdateCode};

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
    pointer_polls: usize,
}
#[async_trait::async_trait]
impl RdpServerDisplayUpdates for Regions {
    fn bitmap_frame_pending(&self) -> bool {
        true
    }
    fn poll_pointer_update(&mut self) -> Option<DisplayUpdate> {
        self.pointer_polls += 1;
        (self.pointer_polls == 1).then_some(DisplayUpdate::HidePointer)
    }
    async fn next_update(&mut self) -> Result<Option<DisplayUpdate>> {
        panic!("dispatch must finish its image without consuming another capture")
    }
}

#[tokio::test]
async fn rdp6_server_dispatch_covers_fullscreen_while_pointer_updates_interleave() {
    let source_pixels = fullscreen_pixels();
    let source = BitmapUpdate {
        x: 0,
        y: 0,
        width: NonZeroU16::new(1920).unwrap(),
        height: NonZeroU16::new(1200).unwrap(),
        format: PixelFormat::BgrA32,
        data: bytes::Bytes::copy_from_slice(&source_pixels),
        stride: NonZeroUsize::new(1920 * 4).unwrap(),
        dirty_rects: Vec::new(),
    };
    let mut codecs = UpdateEncoderCodecs::new();
    codecs.set_max_request_size(16_777_215);
    let encoder = UpdateEncoder::new(
        DesktopSize {
            width: 1920,
            height: 1200,
        },
        CmdFlags::empty(),
        codecs,
    )
    .unwrap();
    let state = Arc::new(std::sync::Mutex::new(GfxState::new(
        1920, 1200, false, false,
    )));
    {
        let mut state = state.lock().unwrap();
        state.manual_bitmap_mode = true;
        state.channel_id = Some(5);
        state.caps_confirmed = true;
        state.avc420_supported = true;
    }
    let packets = Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut native = Writes(packets.clone());
    let mut writer = SharedWriter::new(&mut native, Duration::from_secs(2));
    let mut regions = Regions::default();
    RdpServer::dispatch_display_update(
        DisplayUpdate::Bitmap(source),
        &mut writer,
        1002,
        1003,
        &mut vec![0; 4096],
        encoder,
        &state,
        Some(1004),
        &mut regions,
    )
    .await
    .unwrap();
    assert!(regions.pointer_polls > 1);
    let mut partial = None;
    let mut covered = vec![0u8; 1920 * 1200];
    let mut pointers = 0;
    let mut updates = 0;
    let mut fragmented = false;
    for packet in packets.borrow().iter() {
        let mut cursor = ReadCursor::new(packet);
        let header: FastPathHeader = decode_cursor(&mut cursor).unwrap();
        let header_len = cursor.pos();
        let update: FastPathUpdatePdu<'_> = decode_cursor(&mut cursor).unwrap();
        assert!(cursor.is_empty());
        assert_eq!(header_len + header.data_length, packet.len());
        if update.update_code == UpdateCode::HiddenPointer {
            assert!(
                partial.is_none(),
                "pointer cannot split a logical image update"
            );
            assert_eq!(update.fragmentation, Fragmentation::Single);
            pointers += 1;
            continue;
        }
        assert_eq!(update.update_code, UpdateCode::Bitmap);
        let complete = match update.fragmentation {
            Fragmentation::Single => {
                assert!(partial.is_none());
                Some(update.data.to_vec())
            }
            Fragmentation::First => {
                assert!(partial.is_none());
                fragmented = true;
                partial = Some(update.data.to_vec());
                None
            }
            Fragmentation::Next | Fragmentation::Last => {
                partial.as_mut().unwrap().extend_from_slice(update.data);
                if update.fragmentation == Fragmentation::Last {
                    partial.take()
                } else {
                    None
                }
            }
        };
        if let Some(data) = complete {
            updates += 1;
            let update: BitmapUpdateData<'_> = decode(&data).unwrap();
            assert_eq!(
                update.rectangles.len(),
                1,
                "one RDP6 rectangle per logical update"
            );
            for rect in update.rectangles {
                let pixels = decode_rectangle(&rect);
                for y in rect.rectangle.top..=rect.rectangle.bottom {
                    for x in rect.rectangle.left..=rect.rectangle.right {
                        let dst = usize::from(y) * 1920 + usize::from(x);
                        let src = usize::from(rect.rectangle.bottom - y) * usize::from(rect.width)
                            + usize::from(x - rect.rectangle.left);
                        assert_eq!(
                            &pixels[src * 4..src * 4 + 3],
                            &source_pixels[dst * 4..dst * 4 + 3]
                        );
                        assert_eq!(pixels[src * 4 + 3], 255);
                        covered[dst] += 1;
                    }
                }
            }
        }
    }
    assert!(partial.is_none());
    assert!(updates > 1);
    assert!(fragmented, "noisy rectangles must span Fast-Path packets");
    assert_eq!(pointers, 1);
    assert!(covered.iter().all(|count| *count == 1));
    let state = state.lock().unwrap();
    assert_eq!(state.bitmap_update_budget_bytes, Some(131_072));
    assert_eq!(state.bitmap_dispatches, 1);
}

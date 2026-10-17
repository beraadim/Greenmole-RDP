//! Idle refresh uses the same callback/completion path as moving frames.
use super::{video_encode, CachedFrame};
use anyhow::Result;
use macrdp_encode::VideoEncoder;

/// AVC444 views share one codec chain and must remain a complete ordered pair,
/// including cached idle refreshes. Sending an AVC420 keepalive would terminate
/// browser reconstruction and lose the chroma baseline.
pub(super) fn encode_444(
    encoder: &mut dyn VideoEncoder,
    cache: &CachedFrame,
) -> Result<ironrdp_server::GfxFrameUpdate> {
    encoder.force_keyframe();
    match cache {
        CachedFrame::Bgra {
            data,
            width,
            height,
            stride,
        } => video_encode::encode_bgra(encoder, data, *width, *height, *stride, true),
        CachedFrame::BgraPixelBuffer(buffer, width, height, _) => buffer
            .with_locked_bgra(*width, *height, |data, stride| {
                video_encode::encode_bgra(encoder, data, *width, *height, stride, true)
            })
            .ok_or_else(|| anyhow::anyhow!("Idle AVC444 pixel buffer lock failed"))?,
        CachedFrame::PixelBuffer(_, _, _) => anyhow::bail!("Idle AVC444 requires BGRA capture"),
    }
}

pub(super) fn submit(encoder: &mut dyn VideoEncoder, cache: &CachedFrame) -> Result<(u16, u16)> {
    encoder.force_keyframe();
    let (width, height) = match cache {
        CachedFrame::Bgra {
            data,
            width,
            height,
            stride,
        } => {
            encoder.submit_bgra(data, *width, *height, *stride)?;
            (*width, *height)
        }
        CachedFrame::BgraPixelBuffer(buffer, width, height, _) => {
            buffer
                .with_locked_bgra(*width, *height, |data, stride| {
                    encoder.submit_bgra(data, *width, *height, stride)
                })
                .ok_or_else(|| anyhow::anyhow!("Idle BGRA pixel buffer lock failed"))??;
            (*width, *height)
        }
        CachedFrame::PixelBuffer(buffer, width, height) => {
            encoder.submit_pixel_buffer(buffer.as_ptr())?;
            (*width, *height)
        }
    };
    Ok((width as u16, height as u16))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use macrdp_encode::{Avc444EncodedFrame, EncodedFrame};

    #[derive(Default)]
    struct DelayedEncoder {
        forced: bool,
        submitted: bool,
        fail: bool,
        dual: bool,
    }
    impl VideoEncoder for DelayedEncoder {
        fn backend_name(&self) -> &'static str {
            "delayed"
        }
        fn encode_bgra(&mut self, _: &[u8], _: u32, _: u32, _: usize) -> Result<EncodedFrame> {
            panic!("idle refresh must never perform a synchronous encode")
        }
        fn encode_bgra_444(
            &mut self,
            _: &[u8],
            _: u32,
            _: u32,
            _: usize,
        ) -> Result<Avc444EncodedFrame> {
            assert!(self.dual && self.forced);
            let frame = || EncodedFrame {
                data: if self.fail {
                    Bytes::new()
                } else {
                    Bytes::from_static(&[0, 0, 1, 0x65])
                },
                is_keyframe: true,
                width: 16,
                height: 16,
            };
            Ok(Avc444EncodedFrame {
                main_view: frame(),
                aux_view: frame(),
            })
        }
        fn set_bitrate(&mut self, _: u32) -> Result<()> {
            Ok(())
        }
        fn force_keyframe(&mut self) {
            self.forced = true;
        }
        fn supports_444(&self) -> bool {
            self.dual
        }
        fn supports_pipelining(&self) -> bool {
            true
        }
        fn submit_bgra(&mut self, _: &[u8], width: u32, height: u32, stride: usize) -> Result<()> {
            assert!(self.forced);
            assert_eq!((width, height, stride), (16, 16, 64));
            anyhow::ensure!(!self.fail, "injected submission failure");
            self.submitted = true;
            Ok(())
        }
    }
    fn frame() -> CachedFrame {
        CachedFrame::Bgra {
            data: Bytes::from(vec![0; 1024]),
            width: 16,
            height: 16,
            stride: 64,
        }
    }
    #[test]
    fn idle_submission_returns_without_waiting_for_a_callback() {
        let mut encoder = DelayedEncoder::default();
        assert_eq!(submit(&mut encoder, &frame()).unwrap(), (16, 16));
        assert!(encoder.submitted);
    }
    #[test]
    fn failed_idle_submission_is_not_reported_as_pending() {
        let mut encoder = DelayedEncoder {
            fail: true,
            ..Default::default()
        };
        assert!(submit(&mut encoder, &frame()).is_err());
        assert!(!encoder.submitted);
    }
    #[test]
    fn idle_avc444_keeps_both_views_even_when_encoder_can_pipeline_420() {
        let mut encoder = DelayedEncoder {
            dual: true,
            ..Default::default()
        };
        let frame = encode_444(&mut encoder, &frame()).unwrap();
        assert!(encoder.forced && !encoder.submitted);
        assert!(frame.is_keyframe && frame.h264_aux.is_some());
        assert_eq!((frame.width, frame.height), (16, 16));
    }
    #[test]
    fn incomplete_idle_avc444_pair_is_never_delivered() {
        let mut encoder = DelayedEncoder {
            dual: true,
            fail: true,
            ..Default::default()
        };
        assert!(encode_444(&mut encoder, &frame()).is_err());
    }
}

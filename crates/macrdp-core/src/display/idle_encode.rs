//! Idle refresh uses the same callback/completion path as moving frames.
use super::CachedFrame;
use anyhow::Result;
use macrdp_encode::VideoEncoder;

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
            unreachable!()
        }
        fn set_bitrate(&mut self, _: u32) -> Result<()> {
            Ok(())
        }
        fn force_keyframe(&mut self) {
            self.forced = true;
        }
        fn supports_444(&self) -> bool {
            false
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
}

//! AVC444 is one retained input pair and one ordered H.264 reference chain.
//! Callback latency must never turn into a native flush or reuse its output slot.
use super::*;
use crate::Avc444EncodeError;

pub(super) enum PendingPair {
    Main { auxiliary: OwnedPixelBuffer },
    Auxiliary { main: NativeOutput },
}

impl VtEncoder {
    pub(super) fn encode_avc444(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: usize,
    ) -> Result<Avc444EncodedFrame> {
        self.validate_avc444_input(data, width, height, stride)?;
        if self.pending_444.is_none() {
            self.start_avc444(data, width, height, stride)?;
        }
        self.collect_avc444()
    }

    fn start_avc444(&mut self, data: &[u8], width: u32, height: u32, stride: usize) -> Result<()> {
        anyhow::ensure!(
            self.native.pending_input_buffer.is_none(),
            "Previous native input is still pending"
        );
        let bufs = self
            .yuv444_buf
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("AVC444 not enabled: no YUV444 buffers"))?;
        crate::yuv444_split::bgra_to_yuv444_padded(
            data,
            width,
            height,
            stride,
            self.width,
            self.height,
            &mut bufs.y444,
            &mut bufs.u444,
            &mut bufs.v444,
        )?;
        crate::yuv444_split::yuv444_split_to_yuv420(
            &bufs.y444,
            &bufs.u444,
            &bufs.v444,
            self.width,
            self.height,
            &mut bufs.main_view,
            &mut bufs.aux_view,
        );
        // Allocate both views before native submission. Failing to allocate the
        // auxiliary must not strand an already-encoded main reference.
        let main = Self::create_nv12_from_session_pool(
            self.native.session,
            self.width,
            self.height,
            &bufs.main_view.y,
            &bufs.main_view.u,
            &bufs.main_view.v,
        )?;
        let auxiliary = Self::create_nv12_from_session_pool(
            self.native.session,
            self.width,
            self.height,
            &bufs.aux_view.y,
            &bufs.aux_view.u,
            &bufs.aux_view.v,
        )?;
        let force_keyframe = self.keyframes.pending();
        self.submit_pair_view(main, force_keyframe)?;
        self.keyframes.submitted();
        self.pending_444 = Some(PendingPair::Main { auxiliary });
        Ok(())
    }

    fn submit_pair_view(&mut self, input: OwnedPixelBuffer, force_keyframe: bool) -> Result<()> {
        anyhow::ensure!(
            self.native.pending_input_buffer.is_none(),
            "Previous AVC444 view is still pending"
        );
        let props = Self::make_force_keyframe_props(force_keyframe)?;
        let (pts, duration) = self.make_pts_duration();
        self.native.callback_ctx.completion.begin();
        Self::submit_session_frame(
            self.native.session,
            &self.native.callback_ctx,
            input.as_ptr(),
            pts,
            duration,
            props.as_ref().map_or(std::ptr::null(), OwnedCf::as_ptr),
        )?;
        self.native.pending_input_buffer = Some(input);
        self.frame_count += 1;
        Ok(())
    }

    fn collect_avc444(&mut self) -> Result<Avc444EncodedFrame> {
        loop {
            let Some(output) =
                Self::collect_session_frame(&self.native.callback_ctx, std::time::Duration::ZERO)?
            else {
                return Err(Avc444EncodeError::Pending.into());
            };
            self.native.pending_input_buffer = None;
            match self.pending_444.take().expect("one submitted AVC444 view") {
                PendingPair::Main { auxiliary } => {
                    if !output.is_usable() {
                        // There is no useful pair without the main view. Do not
                        // create an auxiliary native reference that cannot be sent.
                        self.keyframes
                            .complete_pair(&output, &NativeOutput::Dropped);
                        return Err(if output.needs_repair() {
                            Avc444EncodeError::Failed
                        } else {
                            Avc444EncodeError::Dropped
                        }
                        .into());
                    }
                    if let Err(error) = self.submit_pair_view(auxiliary, false) {
                        self.keyframes.complete_pair(&output, &NativeOutput::Failed);
                        return Err(error);
                    }
                    self.pending_444 = Some(PendingPair::Auxiliary { main: output });
                }
                PendingPair::Auxiliary { main } => {
                    let usable = self.keyframes.complete_pair(&main, &output);
                    if !usable {
                        return Err(if main.needs_repair()
                            || output.needs_repair()
                            || (main.is_usable() && output.is_usable())
                        {
                            Avc444EncodeError::Failed
                        } else {
                            Avc444EncodeError::Dropped
                        }
                        .into());
                    }
                    let main_view = main.into_frame(self.width, self.height);
                    let aux_view = output.into_frame(self.width, self.height);
                    tracing::debug!(
                        frame = self.frame_count,
                        main_bytes = main_view.data.len(),
                        aux_bytes = aux_view.data.len(),
                        "AVC444 dual-view encode complete"
                    );
                    return Ok(Avc444EncodedFrame {
                        main_view,
                        aux_view,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> VtEncoder {
        VtEncoder::new_with_quality(16, 16, 120.0, 1_000_000, true, Quality::HighQuality).unwrap()
    }

    fn input(encoder: &VtEncoder) -> OwnedPixelBuffer {
        VtEncoder::create_nv12_from_session_pool(
            encoder.native.session,
            16,
            16,
            &[128; 256],
            &[128; 64],
            &[128; 64],
        )
        .unwrap()
    }

    #[test]
    fn native_timestamps_preserve_two_access_units_per_120_fps_desktop_frame() {
        let mut encoder = fixture();
        assert_eq!(encoder.fps, 240.0);
        let (main_pts, duration) = encoder.make_pts_duration();
        assert_eq!((main_pts.value, main_pts.timescale), (0, 60_000));
        assert_eq!((duration.value, duration.timescale), (250, 60_000));
        encoder.frame_count = 1;
        assert_eq!(encoder.make_pts_duration().0.value, 250);
        encoder.frame_count = 2;
        // Main-to-next-main interval is exactly 1/120 s, rather than the
        // old 600 Hz truncation's 1/150 s at this native access-unit rate.
        assert_eq!(encoder.make_pts_duration().0.value, 500);
    }

    #[test]
    fn pending_main_keeps_both_owned_inputs_and_never_submits_over_a_late_callback() {
        let mut encoder = fixture();
        encoder.pending_444 = Some(PendingPair::Main {
            auxiliary: input(&encoder),
        });
        encoder.native.pending_input_buffer = Some(input(&encoder));
        encoder.native.callback_ctx.completion.begin();
        let original = encoder
            .native
            .pending_input_buffer
            .as_ref()
            .unwrap()
            .as_ptr();
        for _ in 0..20 {
            let result = encoder.encode_avc444(&[0; 16 * 16 * 4], 16, 16, 64);
            assert_eq!(
                result.err().unwrap().downcast_ref::<Avc444EncodeError>(),
                Some(&Avc444EncodeError::Pending)
            );
            assert_eq!(encoder.frame_count, 0);
            assert_eq!(
                encoder
                    .native
                    .pending_input_buffer
                    .as_ref()
                    .unwrap()
                    .as_ptr(),
                original
            );
            assert!(matches!(
                encoder.pending_444,
                Some(PendingPair::Main { .. })
            ));
            assert!(encoder.native.callback_ctx.completion.timing().is_none());
        }
        // A late explicit drop completes the retained main. Its unnecessary
        // auxiliary is never submitted, so neither native references nor a
        // second outstanding callback can be manufactured by a retry.
        encode_callback(
            Arc::as_ptr(&encoder.native.callback_ctx) as *mut c_void,
            std::ptr::null_mut(),
            0,
            outcome::FRAME_DROPPED,
            std::ptr::null(),
        );
        let result = encoder.collect_avc444();
        assert_eq!(
            result.err().unwrap().downcast_ref::<Avc444EncodeError>(),
            Some(&Avc444EncodeError::Dropped)
        );
        assert!(encoder.pending_444.is_none());
        assert!(encoder.native.pending_input_buffer.is_none());
        assert_eq!(encoder.frame_count, 0);
        assert!(!encoder.keyframes.pending());
    }

    #[test]
    fn a_dropped_auxiliary_is_a_completed_drop_and_repairs_the_unsent_main_reference() {
        let mut encoder = fixture();
        encoder.pending_444 = Some(PendingPair::Auxiliary {
            main: NativeOutput::Encoded(vec![0, 0, 0, 1, 0x41], false),
        });
        encoder.native.pending_input_buffer = Some(input(&encoder));
        encoder.keyframes.submitted();
        encoder.native.callback_ctx.completion.begin();
        encode_callback(
            Arc::as_ptr(&encoder.native.callback_ctx) as *mut c_void,
            std::ptr::null_mut(),
            0,
            outcome::FRAME_DROPPED,
            std::ptr::null(),
        );
        let result = encoder.collect_avc444();
        assert_eq!(
            result.err().unwrap().downcast_ref::<Avc444EncodeError>(),
            Some(&Avc444EncodeError::Dropped)
        );
        assert!(encoder.keyframes.pending());
        assert!(encoder.pending_444.is_none());
        assert!(encoder.native.pending_input_buffer.is_none());
    }
}

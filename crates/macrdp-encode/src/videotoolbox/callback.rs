//! Native callback parsing validates complete, possibly segmented CMBlockBuffers.
use super::outcome::{CallbackEvent, FRAME_DROPPED};
use super::*;
use crate::avcc::{self, MAX_ENCODED_BYTES};
use std::borrow::Cow;

extern "C" {
    fn CMBlockBufferCopyDataBytes(
        buffer: *mut c_void,
        offset: usize,
        length: usize,
        destination: *mut c_void,
    ) -> OSStatus;
}

unsafe fn block_data<'a>(block: *mut c_void) -> Result<Cow<'a, [u8]>> {
    let total = CMBlockBufferGetDataLength(block);
    anyhow::ensure!(
        total > 0 && total <= MAX_ENCODED_BYTES,
        "Invalid encoded block length: {total}"
    );
    let mut contiguous = 0;
    let mut pointer = std::ptr::null_mut();
    let status = CMBlockBufferGetDataPointer(
        block,
        0,
        &mut contiguous,
        std::ptr::null_mut(),
        &mut pointer,
    );
    if status == 0 && !pointer.is_null() && contiguous >= total {
        return Ok(Cow::Borrowed(std::slice::from_raw_parts(pointer, total)));
    }
    // A CMBlockBuffer may hold multiple disjoint allocations. Never construct
    // a slice beyond the contiguous span returned by GetDataPointer.
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(total)?;
    bytes.resize(total, 0);
    check_status(
        CMBlockBufferCopyDataBytes(block, 0, total, bytes.as_mut_ptr().cast()),
        "CMBlockBufferCopyDataBytes",
    )?;
    Ok(Cow::Owned(bytes))
}

unsafe fn extract(sample: CMSampleBufferRef) -> Result<(Vec<u8>, bool)> {
    let format = CMSampleBufferGetFormatDescription(sample);
    let block = CMSampleBufferGetDataBuffer(sample);
    anyhow::ensure!(
        !format.is_null() && !block.is_null(),
        "Encoded sample has no format or data"
    );
    let data = block_data(block)?;
    let mut parameter_count = 0;
    let mut length_size = 0;
    check_status(
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            format,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut parameter_count,
            &mut length_size,
        ),
        "CMVideoFormatDescriptionGetH264ParameterSetAtIndex",
    )?;
    let (slices, keyframe) = avcc::convert(&data, length_size as usize)?;
    if !keyframe {
        return Ok((slices, false));
    }
    anyhow::ensure!(parameter_count <= 256, "Too many H.264 parameter sets");
    let mut parameter_sets = Vec::new();
    for index in 0..parameter_count {
        let mut pointer = std::ptr::null();
        let mut size = 0;
        check_status(
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                format,
                index,
                &mut pointer,
                &mut size,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ),
            "CMVideoFormatDescriptionGetH264ParameterSetAtIndex",
        )?;
        anyhow::ensure!(
            !pointer.is_null() && size > 0 && size <= MAX_ENCODED_BYTES,
            "Invalid H.264 parameter set"
        );
        parameter_sets.push(std::slice::from_raw_parts(pointer, size));
    }
    Ok((avcc::self_contained_idr(&slices, parameter_sets)?, true))
}

pub(super) extern "C" fn encode_callback(
    ref_con: *mut c_void,
    _source: *mut c_void,
    status: OSStatus,
    info_flags: VTEncodeInfoFlags,
    sample: CMSampleBufferRef,
) {
    if ref_con.is_null() {
        return;
    }
    let ctx = unsafe { &*(ref_con as *const CallbackCtx) };
    let _finished = CallbackFinished(ctx);
    let output = if status != 0 {
        ctx.diagnostics.record(
            CallbackEvent::Failure,
            status,
            info_flags,
            Some(&"native compression error"),
        );
        NativeOutput::Failed
    } else if sample.is_null() && info_flags & FRAME_DROPPED != 0 {
        ctx.diagnostics
            .record(CallbackEvent::Dropped, status, info_flags, None);
        NativeOutput::Dropped
    } else if sample.is_null() {
        // Apple permits a null sample for dropped frames and says the flag
        // MAY be set. With no explicit flag, retain conservative repair while
        // recording the ambiguity once per interval instead of flooding logs.
        ctx.diagnostics
            .record(CallbackEvent::UnexpectedEmpty, status, info_flags, None);
        NativeOutput::Failed
    } else {
        // No Rust unwind may cross Apple's C callback boundary. Malformed output
        // completes with no frame, allowing the normal recovery path to run.
        match std::panic::catch_unwind(|| unsafe { extract(sample) }) {
            Ok(Ok((data, keyframe))) if !data.is_empty() => {
                ctx.diagnostics
                    .record(CallbackEvent::Encoded, status, info_flags, None);
                NativeOutput::Encoded(data, keyframe)
            }
            Ok(Ok(_)) => {
                ctx.diagnostics.record(
                    CallbackEvent::Failure,
                    status,
                    info_flags,
                    Some(&"empty encoded access unit"),
                );
                NativeOutput::Failed
            }
            Ok(Err(error)) => {
                ctx.diagnostics
                    .record(CallbackEvent::Failure, status, info_flags, Some(&error));
                NativeOutput::Failed
            }
            Err(_) => {
                ctx.diagnostics
                    .record(CallbackEvent::ParserPanic, status, info_flags, None);
                NativeOutput::Failed
            }
        }
    };
    *ctx.output
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(output);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn null_native_completions_distinguish_explicit_drops_from_failures() {
        for (status, flags, expected, category) in [
            (0, FRAME_DROPPED, NativeOutput::Dropped, 1),
            (0, FRAME_DROPPED | 1, NativeOutput::Dropped, 1),
            (0, 0, NativeOutput::Failed, 2),
            (0, 1, NativeOutput::Failed, 2),
            (-1, FRAME_DROPPED, NativeOutput::Failed, 3),
        ] {
            let ctx = Arc::new(CallbackCtx::default());
            ctx.completion.begin();
            let mut completion = ctx.completion.subscribe();
            assert!(
                VtEncoder::collect_session_frame(&ctx, std::time::Duration::ZERO)
                    .unwrap()
                    .is_none()
            );
            encode_callback(
                Arc::as_ptr(&ctx) as *mut c_void,
                std::ptr::null_mut(),
                status,
                flags,
                std::ptr::null(),
            );
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                completion.wait_for(|timing| timing.is_some()),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(ctx.completion.timing().is_some());
            assert!(ctx.has_data.load(std::sync::atomic::Ordering::Acquire));
            assert_eq!(
                VtEncoder::collect_session_frame(&ctx, std::time::Duration::ZERO).unwrap(),
                Some(expected)
            );
            let mut counts = [0; 4];
            counts[category] = 1;
            assert_eq!(ctx.diagnostics.counts(), counts);
        }
    }

    extern "C" {
        static kCFAllocatorNull: CFAllocatorRef;
        fn CMBlockBufferCreateWithMemoryBlock(
            allocator: CFAllocatorRef,
            memory: *mut c_void,
            block_length: usize,
            block_allocator: CFAllocatorRef,
            custom_source: *const c_void,
            offset: usize,
            length: usize,
            flags: u32,
            output: *mut *mut c_void,
        ) -> OSStatus;
        fn CMBlockBufferAppendMemoryBlock(
            buffer: *mut c_void,
            memory: *mut c_void,
            block_length: usize,
            block_allocator: CFAllocatorRef,
            custom_source: *const c_void,
            offset: usize,
            length: usize,
            flags: u32,
        ) -> OSStatus;
    }

    #[test]
    fn fragmented_native_output_is_copied_before_parsing() {
        let mut first = vec![0u8; 4096];
        let mut second = vec![0u8; 4096];
        second[1] = 1;
        second[2] = 0x65;
        unsafe {
            let mut block = std::ptr::null_mut();
            check_status(
                CMBlockBufferCreateWithMemoryBlock(
                    std::ptr::null(),
                    first.as_mut_ptr().cast(),
                    2,
                    kCFAllocatorNull,
                    std::ptr::null(),
                    0,
                    2,
                    0,
                    &mut block,
                ),
                "test CMBlockBufferCreateWithMemoryBlock",
            )
            .unwrap();
            let _owned = OwnedCf::new(block).unwrap();
            check_status(
                CMBlockBufferAppendMemoryBlock(
                    block,
                    second.as_mut_ptr().cast(),
                    3,
                    kCFAllocatorNull,
                    std::ptr::null(),
                    0,
                    3,
                    0,
                ),
                "test CMBlockBufferAppendMemoryBlock",
            )
            .unwrap();
            let bytes = block_data(block).unwrap();
            assert!(matches!(&bytes, Cow::Owned(_)));
            assert_eq!(
                avcc::convert(&bytes, 4).unwrap(),
                (vec![0, 0, 0, 1, 0x65], true)
            );
        }
    }
}

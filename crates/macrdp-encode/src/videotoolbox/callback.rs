//! Native callback parsing validates complete, possibly segmented CMBlockBuffers.
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
    let mut output = Vec::new();
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
        avcc::append_nal(&mut output, std::slice::from_raw_parts(pointer, size))?;
    }
    anyhow::ensure!(
        output.len().saturating_add(slices.len()) <= MAX_ENCODED_BYTES,
        "Encoded frame plus parameter sets exceeds output limit"
    );
    output.try_reserve(slices.len())?;
    output.extend_from_slice(&slices);
    Ok((output, true))
}

pub(super) extern "C" fn encode_callback(
    ref_con: *mut c_void,
    _source: *mut c_void,
    status: OSStatus,
    _info_flags: VTEncodeInfoFlags,
    sample: CMSampleBufferRef,
) {
    if ref_con.is_null() {
        return;
    }
    let ctx = unsafe { &*(ref_con as *const CallbackCtx) };
    let _finished = CallbackFinished(ctx);
    if status != 0 || sample.is_null() {
        tracing::warn!(
            status,
            null_buf = sample.is_null(),
            "VT encode callback error"
        );
        return;
    }
    // No Rust unwind may cross Apple's C callback boundary. Malformed output
    // completes with no frame, allowing the normal recovery path to run.
    match std::panic::catch_unwind(|| unsafe { extract(sample) }) {
        Ok(Ok(output)) => {
            *ctx.output
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(output);
        }
        Ok(Err(error)) => tracing::warn!(%error, "Rejected malformed VideoToolbox output"),
        Err(_) => tracing::error!("VideoToolbox output parser panicked; dropping frame"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

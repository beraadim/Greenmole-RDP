//! VideoToolbox H.264 hardware encoder (macOS)
//!
//! Uses Apple's VideoToolbox framework for GPU-accelerated H.264 encoding.
//! Accepts BGRA pixel data and outputs Annex B H.264 NAL units.

use anyhow::Result;
use std::ffi::c_void;
use std::sync::Arc;

use crate::color_convert::VImageConverter;
use crate::{Avc444EncodedFrame, EncodedFrame, Quality, VideoEncoder};

mod callback;
mod outcome;
mod ownership;
mod pair;
mod startup;
#[cfg(test)]
mod startup_native_tests;
use callback::encode_callback;
use outcome::{CallbackDiagnostics, KeyframeRequests, NativeOutput};
use ownership::{NativeSession, OwnedCf, OwnedPixelBuffer};

static RETIREMENTS: std::sync::OnceLock<crate::retirement::RetirementPool> =
    std::sync::OnceLock::new();

pub(crate) fn native_cleanup_pending() -> bool {
    RETIREMENTS
        .get()
        .is_some_and(crate::retirement::RetirementPool::pending)
}

// --- FFI declarations ---

type CVPixelBufferRef = *mut c_void;
type VTCompressionSessionRef = *mut c_void;
type CMSampleBufferRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFStringRef = *const c_void;
type CFTypeRef = *const c_void;
type CFAllocatorRef = *const c_void;
type OSStatus = i32;
type VTEncodeInfoFlags = u32;

#[repr(C)]
#[derive(Copy, Clone)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

impl CMTime {
    fn make(value: i64, timescale: i32) -> Self {
        Self {
            value,
            timescale,
            flags: 1,
            epoch: 0,
        } // flags=1 = valid
    }
    #[allow(dead_code)]
    fn invalid() -> Self {
        Self {
            value: 0,
            timescale: 0,
            flags: 0,
            epoch: 0,
        }
    }
}

type VTCompressionOutputCallback = extern "C" fn(
    output_callback_ref_con: *mut c_void,
    source_frame_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: VTEncodeInfoFlags,
    sample_buffer: CMSampleBufferRef,
);

#[link(name = "VideoToolbox", kind = "framework")]
#[link(name = "CoreMedia", kind = "framework")]
#[link(name = "CoreVideo", kind = "framework")]
#[link(name = "CoreFoundation", kind = "framework")]
#[allow(dead_code)]
extern "C" {
    fn VTCompressionSessionCreate(
        allocator: CFAllocatorRef,
        width: i32,
        height: i32,
        codec_type: u32,
        encoder_specification: CFDictionaryRef,
        source_image_buffer_attributes: CFDictionaryRef,
        compressed_data_allocator: CFAllocatorRef,
        output_callback: Option<VTCompressionOutputCallback>,
        output_callback_ref_con: *mut c_void,
        compression_session_out: *mut VTCompressionSessionRef,
    ) -> OSStatus;
    fn VTSessionSetProperty(
        session: VTCompressionSessionRef,
        key: CFStringRef,
        value: CFTypeRef,
    ) -> OSStatus;
    fn VTCompressionSessionPrepareToEncodeFrames(session: VTCompressionSessionRef) -> OSStatus;
    fn VTCompressionSessionEncodeFrame(
        session: VTCompressionSessionRef,
        image_buffer: CVPixelBufferRef,
        pts: CMTime,
        duration: CMTime,
        frame_properties: CFDictionaryRef,
        source_frame_refcon: *mut c_void,
        info_flags_out: *mut VTEncodeInfoFlags,
    ) -> OSStatus;
    fn VTCompressionSessionInvalidate(session: VTCompressionSessionRef);

    fn CMSampleBufferGetDataBuffer(sbuf: CMSampleBufferRef) -> *mut c_void;
    fn CMBlockBufferGetDataPointer(
        buf: *mut c_void,
        offset: usize,
        length_at_offset_out: *mut usize,
        total_length_out: *mut usize,
        data_pointer_out: *mut *mut u8,
    ) -> OSStatus;
    fn CMBlockBufferGetDataLength(buf: *mut c_void) -> usize;
    fn CMSampleBufferGetFormatDescription(sbuf: CMSampleBufferRef) -> *const c_void;
    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        video_desc: *const c_void,
        index: usize,
        parameter_set_pointer_out: *mut *const u8,
        parameter_set_size_out: *mut usize,
        parameter_set_count_out: *mut usize,
        nal_unit_header_length_out: *mut i32,
    ) -> OSStatus;

    fn CVPixelBufferCreateWithBytes(
        allocator: CFAllocatorRef,
        width: usize,
        height: usize,
        pixel_format: u32,
        base_address: *mut c_void,
        bytes_per_row: usize,
        release_callback: *const c_void,
        release_ref_con: *mut c_void,
        pixel_buffer_attributes: CFDictionaryRef,
        pixel_buffer_out: *mut CVPixelBufferRef,
    ) -> OSStatus;
    fn CVPixelBufferCreateWithPlanarBytes(
        allocator: CFAllocatorRef,
        width: usize,
        height: usize,
        pixel_format_type: u32,
        data_ptr: *mut c_void, // top-level data pointer (NULL for biplanar)
        data_size: usize,      // total data size
        number_of_planes: usize,
        plane_base_address: *const *mut c_void,
        plane_width: *const usize,
        plane_height: *const usize,
        plane_bytes_per_row: *const usize,
        release_callback: *const c_void,
        release_ref_con: *mut c_void,
        pixel_buffer_attributes: CFDictionaryRef,
        pixel_buffer_out: *mut CVPixelBufferRef,
    ) -> OSStatus;
    fn CVPixelBufferCreate(
        allocator: CFAllocatorRef,
        width: usize,
        height: usize,
        pixel_format_type: u32,
        pixel_buffer_attributes: CFDictionaryRef,
        pixel_buffer_out: *mut CVPixelBufferRef,
    ) -> OSStatus;
    fn CVPixelBufferLockBaseAddress(pixel_buffer: CVPixelBufferRef, lock_flags: u64) -> OSStatus;
    fn CVPixelBufferUnlockBaseAddress(pixel_buffer: CVPixelBufferRef, lock_flags: u64) -> OSStatus;
    fn CVPixelBufferGetBaseAddress(pixel_buffer: CVPixelBufferRef) -> *mut c_void;
    fn CVPixelBufferGetBytesPerRow(pixel_buffer: CVPixelBufferRef) -> usize;
    fn CVPixelBufferRetain(pixel_buffer: CVPixelBufferRef) -> CVPixelBufferRef;
    fn CVPixelBufferRelease(pixel_buffer: CVPixelBufferRef);
    fn CVPixelBufferGetBaseAddressOfPlane(
        pixel_buffer: CVPixelBufferRef,
        plane_idx: usize,
    ) -> *mut c_void;
    fn CVPixelBufferGetBytesPerRowOfPlane(
        pixel_buffer: CVPixelBufferRef,
        plane_idx: usize,
    ) -> usize;
    fn VTCompressionSessionGetPixelBufferPool(session: VTCompressionSessionRef) -> *mut c_void; // CVPixelBufferPoolRef
    fn CVPixelBufferPoolCreatePixelBuffer(
        allocator: CFAllocatorRef,
        pool: *mut c_void, // CVPixelBufferPoolRef
        pixel_buffer_out: *mut CVPixelBufferRef,
    ) -> OSStatus;

    static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    static kCVPixelBufferWidthKey: CFStringRef;
    static kCVPixelBufferHeightKey: CFStringRef;

    static kCVPixelBufferIOSurfacePropertiesKey: CFStringRef;
    static kVTCompressionPropertyKey_RealTime: CFStringRef;
    static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;
    static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
    static kVTCompressionPropertyKey_MaxKeyFrameInterval: CFStringRef;
    static kVTCompressionPropertyKey_ExpectedFrameRate: CFStringRef;
    static kVTCompressionPropertyKey_MaxFrameDelayCount: CFStringRef;
    static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
    static kVTCompressionPropertyKey_H264EntropyMode: CFStringRef;
    static kVTCompressionPropertyKey_AllowOpenGOP: CFStringRef;
    static kVTCompressionPropertyKey_AllowTemporalCompression: CFStringRef;
    static kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: CFStringRef;
    static kVTVideoEncoderSpecification_EnableLowLatencyRateControl: CFStringRef;
    static kVTProfileLevel_H264_High_AutoLevel: CFStringRef;
    static kVTProfileLevel_H264_Baseline_AutoLevel: CFStringRef;
    static kVTProfileLevel_H264_ConstrainedBaseline_AutoLevel: CFStringRef;
    static kVTH264EntropyMode_CABAC: CFStringRef;
    static kVTH264EntropyMode_CAVLC: CFStringRef;

    static kCFBooleanTrue: CFTypeRef;
    static kCFBooleanFalse: CFTypeRef;
    // Callback tables are opaque here: only their exported addresses are used.
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;

    fn CFNumberCreate(allocator: CFAllocatorRef, the_type: i64, value: *const c_void) -> CFTypeRef;
    fn CFDictionaryCreate(
        allocator: CFAllocatorRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        num_values: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;
    fn CFArrayCreate(
        allocator: CFAllocatorRef,
        values: *const *const c_void,
        num_values: isize,
        callbacks: *const c_void,
    ) -> CFTypeRef;
    fn CFRelease(cf: *const c_void);
    fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        c_str: *const i8,
        encoding: u32,
    ) -> CFStringRef;

    static kCFTypeArrayCallBacks: c_void;
    static kVTEncodeFrameOptionKey_ForceKeyFrame: CFStringRef;
    static kVTCompressionPropertyKey_DataRateLimits: CFStringRef;
    static kVTCompressionPropertyKey_ColorPrimaries: CFStringRef;
    static kVTCompressionPropertyKey_TransferFunction: CFStringRef;
    static kVTCompressionPropertyKey_YCbCrMatrix: CFStringRef;
    static kCVImageBufferColorPrimaries_ITU_R_709_2: CFStringRef;
    static kCVImageBufferTransferFunction_ITU_R_709_2: CFStringRef;
    static kCVImageBufferYCbCrMatrix_ITU_R_709_2: CFStringRef;
    static kCMFormatDescriptionExtension_FullRangeVideo: CFStringRef;
}

// CFNumber types
const K_CF_NUMBER_SINT32_TYPE: i64 = 3;
const K_CF_NUMBER_FLOAT64_TYPE: i64 = 13;

// Pixel format: BGRA
const K_CV_PIXEL_FORMAT_32BGRA: u32 = 0x42475241; // 'BGRA'

// Pixel format: NV12 (420f — YUV 4:2:0 biplanar, full range)
// Full range (Y: 0-255) avoids the washed-out look of video range (Y: 16-235)
const K_CV_PIXEL_FORMAT_420F: u32 = 0x34323066; // '420f'

// H.264 codec type
const K_CM_VIDEO_CODEC_TYPE_H264: u32 = 0x61766331; // 'avc1'

fn cf_i32(v: i32) -> CFTypeRef {
    unsafe {
        CFNumberCreate(
            std::ptr::null(),
            K_CF_NUMBER_SINT32_TYPE,
            &v as *const _ as *const c_void,
        )
    }
}
fn cf_f64(v: f64) -> CFTypeRef {
    unsafe {
        CFNumberCreate(
            std::ptr::null(),
            K_CF_NUMBER_FLOAT64_TYPE,
            &v as *const _ as *const c_void,
        )
    }
}

fn check_status(status: OSStatus, operation: &str) -> Result<()> {
    anyhow::ensure!(status == 0, "{operation} failed: {status}");
    Ok(())
}

fn validate_bgra_layout(
    data: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    encoder_width: u32,
    encoder_height: u32,
) -> Result<()> {
    anyhow::ensure!(
        width > 0 && height > 0 && width <= encoder_width && height <= encoder_height,
        "BGRA dimensions do not fit the encoder"
    );
    let row_bytes = (width as usize)
        .checked_mul(4)
        .ok_or_else(|| anyhow::anyhow!("BGRA row size overflow"))?;
    anyhow::ensure!(stride >= row_bytes, "BGRA stride is shorter than a row");
    let required = (height as usize - 1)
        .checked_mul(stride)
        .and_then(|offset| offset.checked_add(row_bytes))
        .ok_or_else(|| anyhow::anyhow!("BGRA layout overflow"))?;
    anyhow::ensure!(
        data.len() >= required,
        "BGRA input is shorter than its layout"
    );
    Ok(())
}

unsafe fn set_required_property(
    session: VTCompressionSessionRef,
    key: CFStringRef,
    value: CFTypeRef,
    name: &str,
) -> Result<()> {
    check_status(VTSessionSetProperty(session, key, value), name)
}

unsafe fn set_optional_property(
    session: VTCompressionSessionRef,
    key: CFStringRef,
    value: CFTypeRef,
    name: &str,
) {
    let status = VTSessionSetProperty(session, key, value);
    if status != 0 {
        tracing::warn!(
            property = name,
            status,
            "Optional VideoToolbox property was not applied"
        );
    }
}

// --- Callback context (shared between encoder thread and VT callback thread) ---

#[derive(Default)]
struct CallbackCtx {
    output: std::sync::Mutex<Option<NativeOutput>>,
    ready: std::sync::Condvar,
    has_data: std::sync::atomic::AtomicBool,
    completion: crate::completion::EncodeCompletion,
    diagnostics: CallbackDiagnostics,
}

/// Every callback exit, including malformed/error output, wakes its consumer.
struct CallbackFinished<'a>(&'a CallbackCtx);

impl Drop for CallbackFinished<'_> {
    fn drop(&mut self) {
        let _output = self
            .0
            .output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.0
            .has_data
            .store(true, std::sync::atomic::Ordering::Release);
        self.0.completion.finish();
        self.0.ready.notify_one();
    }
}

fn low_latency_wait_budget(fps: f32) -> std::time::Duration {
    let frame_ms = if fps > 0.0 { 1000.0 / fps as f64 } else { 16.0 };
    let wait_ms = (frame_ms * 0.75).clamp(8.0, 20.0);
    std::time::Duration::from_millis(wait_ms.round() as u64)
}

// --- YUV444 reusable buffers for AVC444 mode ---

struct Yuv444Buffers {
    y444: Vec<u8>,
    u444: Vec<u8>,
    v444: Vec<u8>,
    main_view: crate::yuv444_split::Yuv420Frame,
    aux_view: crate::yuv444_split::Yuv420Frame,
}

impl Yuv444Buffers {
    fn new(width: u32, height: u32) -> Self {
        let full = (width * height) as usize;
        Self {
            y444: vec![0u8; full],
            u444: vec![0u8; full],
            v444: vec![0u8; full],
            main_view: crate::yuv444_split::Yuv420Frame::new(width, height),
            aux_view: crate::yuv444_split::Yuv420Frame::new(width, height),
        }
    }
}

/// Send-safe pointer wrappers for multi-threaded NV12 conversion.
/// Safety: callers ensure the pointed-to memory is valid and disjoint per thread.
#[derive(Clone, Copy)]
struct SendPtr(usize);
#[derive(Clone, Copy)]
struct SendPtrMut(usize);
unsafe impl Send for SendPtr {}
unsafe impl Send for SendPtrMut {}
unsafe impl Sync for SendPtr {}
unsafe impl Sync for SendPtrMut {}
impl SendPtr {
    fn from(p: *const u8) -> Self {
        Self(p as usize)
    }
    unsafe fn add(self, off: usize) -> *const u8 {
        (self.0 + off) as *const u8
    }
}
impl SendPtrMut {
    fn from(p: *mut u8) -> Self {
        Self(p as usize)
    }
    unsafe fn add(self, off: usize) -> *mut u8 {
        (self.0 + off) as *mut u8
    }
}

// --- Encoder ---

pub struct VtEncoder {
    native: NativeSession,
    applied_bitrate: u32,
    width: u32,
    height: u32,
    frame_count: u64,
    fps: f32,
    mode_444: bool,
    yuv444_buf: Option<Yuv444Buffers>,
    keyframes: KeyframeRequests,
    pending_444: Option<pair::PendingPair>,
    /// vImage SIMD converter for BGRA→NV12 acceleration.
    vimage: Option<VImageConverter>,
}

// VTCompressionSession is thread-safe per Apple docs
unsafe impl Send for VtEncoder {}

impl VtEncoder {
    fn validate_avc444_input(
        &self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: usize,
    ) -> Result<()> {
        validate_bgra_layout(data, width, height, stride, self.width, self.height)?;
        anyhow::ensure!(
            width % 2 == 0 && height % 2 == 0,
            "AVC444 requires even frame dimensions, got {width}x{height}"
        );
        Ok(())
    }

    /// Create a balanced encoder; callers selecting a preset use new_with_quality.
    pub fn new(width: u32, height: u32, fps: f32, bitrate: u32, mode_444: bool) -> Result<Self> {
        Self::new_with_quality(width, height, fps, bitrate, mode_444, Quality::Balanced)
    }

    pub fn new_with_quality(
        width: u32,
        height: u32,
        fps: f32,
        bitrate: u32,
        mode_444: bool,
        quality: Quality,
    ) -> Result<Self> {
        quality.quantizer_range()?;
        let callback_ctx = Arc::new(CallbackCtx::default());
        // Each desktop frame has two ordered native access units in AVC444.
        // Keep their combined target bitrate while describing their true cadence.
        let coded_fps = if mode_444 { fps * 2.0 } else { fps };

        // NV12 full-range input for both AVC420 and AVC444.
        // BGRA input produces video range (16-235) causing washed-out colors.
        let native = Self::create_session(
            width,
            height,
            coded_fps,
            bitrate,
            &callback_ctx,
            K_CV_PIXEL_FORMAT_420F,
            quality,
            mode_444,
        )?;

        let yuv444_buf = if mode_444 {
            Some(Yuv444Buffers::new(width, height))
        } else {
            None
        };

        let vimage = VImageConverter::new()
            .map_err(|e| tracing::warn!("vImage init failed: {e}"))
            .ok();

        tracing::info!(
            width,
            height,
            fps,
            coded_fps,
            mode_444,
            bitrate_mbps = bitrate as f64 / 1_000_000.0,
            ?quality,
            "VideoToolbox hardware encoder created"
        );

        Ok(Self {
            native,
            applied_bitrate: bitrate,
            width,
            height,
            frame_count: 0,
            fps: coded_fps,
            mode_444,
            yuv444_buf,
            keyframes: KeyframeRequests::default(),
            pending_444: None,
            vimage,
        })
    }

    /// Create a VT compression session with the given parameters.
    /// `pixel_format` controls the expected input pixel format (BGRA or NV12).
    fn create_session(
        width: u32,
        height: u32,
        fps: f32,
        bitrate: u32,
        callback_ctx: &Arc<CallbackCtx>,
        pixel_format: u32,
        quality: Quality,
        mode_444: bool,
    ) -> Result<NativeSession> {
        let retirement = RETIREMENTS
            .get_or_init(|| crate::retirement::RetirementPool::new(2))
            .reserve()?;
        let retirement_id = retirement.diagnostic_id();
        let mut session = std::ptr::null_mut();
        unsafe {
            let spec_keys = [
                kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
                kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
            ];
            // Low-latency rate control may drop individual native frames. A
            // dropped AVC444 view discards its counterpart and forces a new IDR,
            // which can sustain bitrate debt indefinitely. Keep the normal
            // bitrate controller for ordered pairs; RealTime, no reordering and
            // zero frame delay still apply to both transport modes.
            let spec_values = [
                kCFBooleanTrue,
                if mode_444 {
                    kCFBooleanFalse
                } else {
                    kCFBooleanTrue
                },
            ];
            let encoder_spec = OwnedCf::dictionary(&spec_keys, &spec_values)?;
            let src_keys = [
                kCVPixelBufferPixelFormatTypeKey,
                kCVPixelBufferWidthKey,
                kCVPixelBufferHeightKey,
            ];
            let fmt_num = OwnedCf::new(cf_i32(pixel_format as i32))?;
            let w_num = OwnedCf::new(cf_i32(width as i32))?;
            let h_num = OwnedCf::new(cf_i32(height as i32))?;
            let src_values = [fmt_num.as_ptr(), w_num.as_ptr(), h_num.as_ptr()];
            let src_attrs = OwnedCf::dictionary(&src_keys, &src_values)?;
            let create_started = std::time::Instant::now();
            tracing::info!(target: "macrdp_transition",
                component = "video_encoder", worker_pid = std::process::id(),
                retirement_id, phase = "vt_create", state = "begin",
                width, height, fps, bitrate, pixel_format, mode_444, ?quality,
                require_hardware = true, low_latency_rate_control = !mode_444,
                "Encoder lifecycle transition");
            let status = VTCompressionSessionCreate(
                std::ptr::null(),
                width as i32,
                height as i32,
                K_CM_VIDEO_CODEC_TYPE_H264,
                encoder_spec.as_ptr(),
                src_attrs.as_ptr(),
                std::ptr::null(),
                Some(encode_callback),
                Arc::as_ptr(callback_ctx) as *mut c_void,
                &mut session,
            );
            tracing::info!(target: "macrdp_transition",
                component = "video_encoder", worker_pid = std::process::id(),
                retirement_id, phase = "vt_create", state = "end",
                outcome = if status == 0 && !session.is_null() { "ok" } else { "error" },
                status, elapsed_ms = create_started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                "Encoder lifecycle transition");
            anyhow::ensure!(
                !session.is_null(),
                "VTCompressionSessionCreate failed: {status}"
            );
            // Own the session before checking status/configuration. A failed
            // initialization follows the same nonblocking retirement as teardown.
            let native = NativeSession {
                session,
                callback_ctx: callback_ctx.clone(),
                pending_input_buffer: None,
                retirement: Some(retirement),
            };
            check_status(status, "VTCompressionSessionCreate")?;
            // Native service activation can dominate Create on the first use.
            // Keep configuration and prepare timings separate so a future slow
            // startup can be attributed without changing the worker's lifetime.
            let properties = startup::Phase::begin(retirement_id, "vt_properties");
            set_required_property(
                session,
                kVTCompressionPropertyKey_ProfileLevel,
                kVTProfileLevel_H264_ConstrainedBaseline_AutoLevel,
                "ProfileLevel",
            )?;
            set_required_property(
                session,
                kVTCompressionPropertyKey_RealTime,
                kCFBooleanTrue,
                "RealTime",
            )?;
            // A generic H.264 decoder uses SPS VUI, while RDP decoders use
            // the protocol's full-range BT.709 matrix. Make both agree with
            // our 420f input rather than leaving color interpretation inferred.
            for (key, value, name) in [
                (
                    kVTCompressionPropertyKey_ColorPrimaries,
                    kCVImageBufferColorPrimaries_ITU_R_709_2,
                    "ColorPrimaries",
                ),
                (
                    kVTCompressionPropertyKey_TransferFunction,
                    kCVImageBufferTransferFunction_ITU_R_709_2,
                    "TransferFunction",
                ),
                (
                    kVTCompressionPropertyKey_YCbCrMatrix,
                    kCVImageBufferYCbCrMatrix_ITU_R_709_2,
                    "YCbCrMatrix",
                ),
            ] {
                set_required_property(session, key, value, name)?;
            }
            set_required_property(
                session,
                kVTCompressionPropertyKey_AllowFrameReordering,
                kCFBooleanFalse,
                "AllowFrameReordering",
            )?;
            set_optional_property(
                session,
                kVTCompressionPropertyKey_H264EntropyMode,
                kVTH264EntropyMode_CAVLC,
                "H264EntropyMode",
            );
            set_optional_property(
                session,
                kVTCompressionPropertyKey_AllowOpenGOP,
                kCFBooleanFalse,
                "AllowOpenGOP",
            );
            let delay = OwnedCf::new(cf_i32(0))?;
            set_optional_property(
                session,
                kVTCompressionPropertyKey_MaxFrameDelayCount,
                delay.as_ptr(),
                "MaxFrameDelayCount",
            );
            set_optional_property(
                session,
                kVTCompressionPropertyKey_AllowTemporalCompression,
                kCFBooleanTrue,
                "AllowTemporalCompression",
            );
            set_optional_property(
                session,
                kCMFormatDescriptionExtension_FullRangeVideo,
                kCFBooleanTrue,
                "FullRangeVideo",
            );
            let expected_fps = OwnedCf::new(cf_f64(fps as f64))?;
            set_optional_property(
                session,
                kVTCompressionPropertyKey_ExpectedFrameRate,
                expected_fps.as_ptr(),
                "ExpectedFrameRate",
            );
            let average_bitrate = OwnedCf::new(cf_i32(bitrate as i32))?;
            set_required_property(
                session,
                kVTCompressionPropertyKey_AverageBitRate,
                average_bitrate.as_ptr(),
                "AverageBitRate",
            )?;
            let interval = OwnedCf::new(cf_i32(fps as i32 * 5))?;
            set_optional_property(
                session,
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                interval.as_ptr(),
                "MaxKeyFrameInterval",
            );
            let (min_qp, max_qp) = quality.quantizer_range()?;
            // Require the requested quantizer bounds. Unsupported hardware
            // falls back to software with the same quality policy.
            for (name, label, qp) in [
                (
                    b"MinAllowedFrameQP\0".as_slice(),
                    "MinAllowedFrameQP",
                    min_qp,
                ),
                (
                    b"MaxAllowedFrameQP\0".as_slice(),
                    "MaxAllowedFrameQP",
                    max_qp,
                ),
            ] {
                let key = OwnedCf::new(CFStringCreateWithCString(
                    std::ptr::null(),
                    name.as_ptr().cast(),
                    0x08000100,
                ))?;
                let value = OwnedCf::new(cf_i32(i32::from(qp)))?;
                set_required_property(session, key.as_ptr(), value.as_ptr(), label)?;
            }
            tracing::info!(
                ?quality,
                min_qp,
                max_qp,
                "VideoToolbox accepted quality quantizer bounds"
            );
            let speed_key = OwnedCf::new(CFStringCreateWithCString(
                std::ptr::null(),
                b"PrioritizeEncodingSpeedOverQuality\0".as_ptr().cast(),
                0x08000100,
            ))?;
            set_optional_property(
                session,
                speed_key.as_ptr(),
                if quality == Quality::LowLatency {
                    kCFBooleanTrue
                } else {
                    kCFBooleanFalse
                },
                "PrioritizeEncodingSpeedOverQuality",
            );
            properties.finish(0);
            let prepare = startup::Phase::begin(retirement_id, "vt_prepare");
            let status = VTCompressionSessionPrepareToEncodeFrames(session);
            prepare.finish(status);
            check_status(status, "VTCompressionSessionPrepareToEncodeFrames")?;
            Ok(native)
        }
    }

    /// Fast BGRA→NV12 full-range conversion via session pool buffer.
    /// Optimized: unsafe pointer math, no bounds checks, auto-vectorizable loops.
    fn create_nv12_from_bgra_fast(
        session: VTCompressionSessionRef,
        enc_w: u32,
        enc_h: u32,
        data: &[u8],
        src_w: u32,
        src_h: u32,
        stride: usize,
    ) -> Result<OwnedPixelBuffer> {
        let mut pb: CVPixelBufferRef = std::ptr::null_mut();
        let sw = src_w.min(enc_w) as usize;
        let sh = src_h.min(enc_h) as usize;

        unsafe {
            let pool = VTCompressionSessionGetPixelBufferPool(session);
            if pool.is_null() {
                anyhow::bail!("VT pixel buffer pool is null");
            }
            let status = CVPixelBufferPoolCreatePixelBuffer(std::ptr::null(), pool, &mut pb);
            if status != 0 || pb.is_null() {
                anyhow::bail!("Pool alloc failed: {status}");
            }

            let mut pixel_buffer = OwnedPixelBuffer::new(pb)?;
            pixel_buffer.lock()?;
            let y_base = CVPixelBufferGetBaseAddressOfPlane(pb, 0) as *mut u8;
            let y_bpr = CVPixelBufferGetBytesPerRowOfPlane(pb, 0);
            let uv_base = CVPixelBufferGetBaseAddressOfPlane(pb, 1) as *mut u8;
            let uv_bpr = CVPixelBufferGetBytesPerRowOfPlane(pb, 1);

            if y_base.is_null() || uv_base.is_null() {
                anyhow::bail!("NV12 plane is null");
            }

            // Single-pass BGRA→NV12: process row pairs (Y for 2 rows + UV for 1 row).
            // Single-threaded: thread spawn/join overhead per frame exceeds savings.
            let src = data.as_ptr();
            let uv_w = sw / 2;
            for pr in 0..(sh / 2) {
                let r0 = pr * 2;
                let r1 = r0 + 1;
                let src_r0 = src.add(r0 * stride);
                let src_r1 = src.add(r1 * stride);
                let y_dst0 = y_base.add(r0 * y_bpr);
                let y_dst1 = y_base.add(r1 * y_bpr);
                let uv_dst = uv_base.add(pr * uv_bpr);

                for col in 0..uv_w {
                    let c0 = col * 2;
                    let c1 = c0 + 1;
                    let p00 = src_r0.add(c0 * 4);
                    let p01 = src_r0.add(c1 * 4);
                    let p10 = src_r1.add(c0 * 4);
                    let p11 = src_r1.add(c1 * 4);

                    let (b00, g00, r00) = (*p00 as i32, *p00.add(1) as i32, *p00.add(2) as i32);
                    let (b01, g01, r01) = (*p01 as i32, *p01.add(1) as i32, *p01.add(2) as i32);
                    let (b10, g10, r10) = (*p10 as i32, *p10.add(1) as i32, *p10.add(2) as i32);
                    let (b11, g11, r11) = (*p11 as i32, *p11.add(1) as i32, *p11.add(2) as i32);

                    *y_dst0.add(c0) = crate::color::luma(r00, g00, b00);
                    *y_dst0.add(c1) = crate::color::luma(r01, g01, b01);
                    *y_dst1.add(c0) = crate::color::luma(r10, g10, b10);
                    *y_dst1.add(c1) = crate::color::luma(r11, g11, b11);

                    let rb = (r00 + r01 + r10 + r11) >> 2;
                    let gb = (g00 + g01 + g10 + g11) >> 2;
                    let bb = (b00 + b01 + b10 + b11) >> 2;
                    let (u, v) = crate::color::chroma(rb, gb, bb);
                    *uv_dst.add(col * 2) = u;
                    *uv_dst.add(col * 2 + 1) = v;
                }
            }

            pixel_buffer.unlock();
            Ok(pixel_buffer)
        }
    }

    /// Create NV12 CVPixelBuffer using vImage SIMD acceleration directly into
    /// the VT-owned pixel buffer planes. Falls back to scalar conversion if
    /// vImage is unavailable.
    fn create_nv12_vimage(
        &mut self,
        session: VTCompressionSessionRef,
        enc_w: u32,
        enc_h: u32,
        data: &[u8],
        src_w: u32,
        src_h: u32,
        stride: usize,
    ) -> Result<OwnedPixelBuffer> {
        let vimage = match &self.vimage {
            Some(v) => v,
            None => {
                return Self::create_nv12_from_bgra_fast(
                    session, enc_w, enc_h, data, src_w, src_h, stride,
                )
            }
        };

        let h = src_h.min(enc_h) as usize;

        let mut pb: CVPixelBufferRef = std::ptr::null_mut();
        unsafe {
            let pool = VTCompressionSessionGetPixelBufferPool(session);
            if pool.is_null() {
                anyhow::bail!("VT pixel buffer pool is null");
            }
            let status = CVPixelBufferPoolCreatePixelBuffer(std::ptr::null(), pool, &mut pb);
            if status != 0 || pb.is_null() {
                anyhow::bail!("Pool alloc failed: {status}");
            }

            let mut pixel_buffer = OwnedPixelBuffer::new(pb)?;
            pixel_buffer.lock()?;
            let y_base = CVPixelBufferGetBaseAddressOfPlane(pb, 0) as *mut u8;
            let y_bpr = CVPixelBufferGetBytesPerRowOfPlane(pb, 0);
            let uv_base = CVPixelBufferGetBaseAddressOfPlane(pb, 1) as *mut u8;
            let uv_bpr = CVPixelBufferGetBytesPerRowOfPlane(pb, 1);

            if y_base.is_null() || uv_base.is_null() {
                anyhow::bail!("NV12 plane is null");
            }

            let y_plane_len = y_bpr * h;
            let uv_plane_len = uv_bpr * (h / 2);
            let y_plane = std::slice::from_raw_parts_mut(y_base, y_plane_len);
            let uv_plane = std::slice::from_raw_parts_mut(uv_base, uv_plane_len);

            if let Err(e) = vimage.bgra_to_nv12_with_strides(
                data, src_w, src_h, stride, y_bpr, uv_bpr, y_plane, uv_plane,
            ) {
                return Err(anyhow::anyhow!("vImage bgra_to_nv12 failed: {e}"));
            }

            pixel_buffer.unlock();
            Ok(pixel_buffer)
        }
    }

    /// Create a BGRA CVPixelBuffer with VT-managed memory and copy frame data into it.
    fn create_bgra_pixelbuffer(
        enc_w: u32,
        enc_h: u32,
        data: &[u8],
        src_w: u32,
        src_h: u32,
        stride: usize,
    ) -> Result<OwnedPixelBuffer> {
        let mut pb: CVPixelBufferRef = std::ptr::null_mut();
        unsafe {
            let status = CVPixelBufferCreate(
                std::ptr::null(),
                enc_w as usize,
                enc_h as usize,
                K_CV_PIXEL_FORMAT_32BGRA,
                std::ptr::null(),
                &mut pb,
            );
            if status != 0 || pb.is_null() {
                anyhow::bail!("CVPixelBufferCreate BGRA failed: {status}");
            }

            let mut pixel_buffer = OwnedPixelBuffer::new(pb)?;
            pixel_buffer.lock()?;
            let base = CVPixelBufferGetBaseAddress(pb) as *mut u8;
            anyhow::ensure!(!base.is_null(), "BGRA pixel buffer has no base address");
            let bpr = CVPixelBufferGetBytesPerRow(pb);
            let copy_rows = (src_h as usize).min(enc_h as usize);
            let copy_cols_bytes = (src_w as usize * 4).min(bpr);
            for row in 0..copy_rows {
                let src_start = row * stride;
                let dst_start = row * bpr;
                if src_start + copy_cols_bytes <= data.len() {
                    std::ptr::copy_nonoverlapping(
                        data.as_ptr().add(src_start),
                        base.add(dst_start),
                        copy_cols_bytes,
                    );
                }
            }

            pixel_buffer.unlock();
            Ok(pixel_buffer)
        }
    }

    /// Convert I420 (YUV420) frame to BGRA using RDP's full-range BT.709 inverse.
    fn yuv420_to_bgra(frame: &crate::yuv444_split::Yuv420Frame) -> Vec<u8> {
        let w = frame.width as usize;
        let h = frame.height as usize;
        let uv_w = w / 2;
        let mut bgra = vec![0u8; w * h * 4];

        for row in 0..h {
            for col in 0..w {
                let y_idx = row * w + col;
                let uv_idx = (row / 2) * uv_w + (col / 2);

                let y = frame.y.get(y_idx).copied().unwrap_or(0) as i32;
                let u = frame.u.get(uv_idx).copied().unwrap_or(128) as i32;
                let v = frame.v.get(uv_idx).copied().unwrap_or(128) as i32;

                let d = u - 128;
                let e = v - 128;
                let r = (y + ((103_206 * e + 32_768) >> 16)).clamp(0, 255) as u8;
                let g = (y - ((12_276 * d + 30_679 * e + 32_768) >> 16)).clamp(0, 255) as u8;
                let b = (y + ((121_609 * d + 32_768) >> 16)).clamp(0, 255) as u8;

                let px = (row * w + col) * 4;
                bgra[px] = b;
                bgra[px + 1] = g;
                bgra[px + 2] = r;
                bgra[px + 3] = 255;
            }
        }
        bgra
    }

    /// Convert BGRA frame to NV12 full-range via session pool buffer (single pass).
    /// BT.709 full range: Y=0-255, UV=0-255.
    #[allow(dead_code)]
    fn create_nv12_from_bgra(
        session: VTCompressionSessionRef,
        enc_w: u32,
        enc_h: u32,
        data: &[u8],
        src_w: u32,
        src_h: u32,
        stride: usize,
    ) -> Result<OwnedPixelBuffer> {
        let mut pb: CVPixelBufferRef = std::ptr::null_mut();
        let w = enc_w as usize;
        let h = enc_h as usize;
        let sw = src_w as usize;
        let sh = src_h as usize;

        unsafe {
            let pool = VTCompressionSessionGetPixelBufferPool(session);
            if pool.is_null() {
                anyhow::bail!("VT pixel buffer pool is null");
            }
            let status = CVPixelBufferPoolCreatePixelBuffer(std::ptr::null(), pool, &mut pb);
            if status != 0 || pb.is_null() {
                anyhow::bail!("CVPixelBufferPoolCreatePixelBuffer failed: {status}");
            }

            let mut pixel_buffer = OwnedPixelBuffer::new(pb)?;
            pixel_buffer.lock()?;

            let y_base = CVPixelBufferGetBaseAddressOfPlane(pb, 0) as *mut u8;
            let y_bpr = CVPixelBufferGetBytesPerRowOfPlane(pb, 0);
            let uv_base = CVPixelBufferGetBaseAddressOfPlane(pb, 1) as *mut u8;
            let uv_bpr = CVPixelBufferGetBytesPerRowOfPlane(pb, 1);

            if y_base.is_null() || uv_base.is_null() {
                anyhow::bail!("NV12 plane address is null");
            }

            let rows = sh.min(h);
            let cols = sw.min(w);

            // Y plane: full resolution, BT.709 full range
            for row in 0..rows {
                let bgra_row = row * stride;
                let y_row = row * y_bpr;
                for col in 0..cols {
                    let px = bgra_row + col * 4;
                    let b = data[px] as i32;
                    let g = data[px + 1] as i32;
                    let r = data[px + 2] as i32;
                    *y_base.add(y_row + col) = crate::color::luma(r, g, b);
                }
            }

            // UV plane: half resolution, 2x2 averaged, BT.709 full range
            let uv_rows = rows / 2;
            let uv_cols = cols / 2;
            for row in 0..uv_rows {
                let r0 = row * 2;
                let r1 = r0 + 1;
                let uv_row_off = row * uv_bpr;
                for col in 0..uv_cols {
                    let c0 = col * 2;
                    let c1 = c0 + 1;
                    // 2x2 block averaging
                    let mut rb = 0i32;
                    let mut gb = 0i32;
                    let mut bb = 0i32;
                    for &sr in &[r0, r1] {
                        for &sc in &[c0, c1] {
                            let px = sr * stride + sc * 4;
                            bb += data[px] as i32;
                            gb += data[px + 1] as i32;
                            rb += data[px + 2] as i32;
                        }
                    }
                    rb >>= 2;
                    gb >>= 2;
                    bb >>= 2; // /4
                    let (u, v) = crate::color::chroma(rb, gb, bb);
                    let off = uv_row_off + col * 2;
                    *uv_base.add(off) = u;
                    *uv_base.add(off + 1) = v;
                }
            }

            pixel_buffer.unlock();
            Ok(pixel_buffer)
        }
    }

    /// Allocate NV12 pixel buffer from VT session's pool (IOSurface-backed, hardware compatible)
    /// and fill with I420 plane data.
    fn create_nv12_from_session_pool(
        session: VTCompressionSessionRef,
        width: u32,
        height: u32,
        y_plane: &[u8],
        u_plane: &[u8],
        v_plane: &[u8],
    ) -> Result<OwnedPixelBuffer> {
        let mut pb: CVPixelBufferRef = std::ptr::null_mut();
        let w = width as usize;
        let h = height as usize;

        unsafe {
            let pool = VTCompressionSessionGetPixelBufferPool(session);
            if pool.is_null() {
                anyhow::bail!("VTCompressionSessionGetPixelBufferPool returned null");
            }

            let status = CVPixelBufferPoolCreatePixelBuffer(std::ptr::null(), pool, &mut pb);
            if status != 0 || pb.is_null() {
                anyhow::bail!("CVPixelBufferPoolCreatePixelBuffer failed: {status}");
            }

            let mut pixel_buffer = OwnedPixelBuffer::new(pb)?;
            pixel_buffer.lock()?;

            // Plane 0: Y
            let y_base = CVPixelBufferGetBaseAddressOfPlane(pb, 0) as *mut u8;
            let y_bpr = CVPixelBufferGetBytesPerRowOfPlane(pb, 0);
            if y_base.is_null() {
                anyhow::bail!("Pool NV12 Y plane is null");
            }
            for row in 0..h {
                let src_off = row * w;
                let dst_off = row * y_bpr;
                if src_off + w <= y_plane.len() {
                    std::ptr::copy_nonoverlapping(
                        y_plane.as_ptr().add(src_off),
                        y_base.add(dst_off),
                        w.min(y_bpr),
                    );
                }
            }

            // Plane 1: interleaved UV (NV12)
            let uv_base = CVPixelBufferGetBaseAddressOfPlane(pb, 1) as *mut u8;
            let uv_bpr = CVPixelBufferGetBytesPerRowOfPlane(pb, 1);
            if uv_base.is_null() {
                anyhow::bail!("Pool NV12 UV plane is null");
            }
            let uv_w = w / 2;
            let uv_h = h / 2;
            for row in 0..uv_h {
                for col in 0..uv_w {
                    let src_idx = row * uv_w + col;
                    let dst_off = row * uv_bpr + col * 2;
                    if src_idx < u_plane.len() && src_idx < v_plane.len() {
                        *uv_base.add(dst_off) = u_plane[src_idx];
                        *uv_base.add(dst_off + 1) = v_plane[src_idx];
                    }
                }
            }

            pixel_buffer.unlock();
            Ok(pixel_buffer)
        }
    }

    /// Create an IOSurface-backed NV12 CVPixelBuffer from I420 planes.
    /// Uses CVPixelBufferCreate + memcpy into planes (IOSurface-backed = VT hardware compatible).
    #[allow(dead_code)]
    fn create_nv12_pixelbuffer(
        width: u32,
        height: u32,
        y_plane: &[u8],
        u_plane: &[u8],
        v_plane: &[u8],
    ) -> Result<OwnedPixelBuffer> {
        let mut pb: CVPixelBufferRef = std::ptr::null_mut();
        let w = width as usize;
        let h = height as usize;

        unsafe {
            // Create NV12 pixel buffer without IOSurface properties.
            // VT will handle the memory backing internally.
            let status = CVPixelBufferCreate(
                std::ptr::null(),
                w,
                h,
                K_CV_PIXEL_FORMAT_420F,
                std::ptr::null(), // no attributes — avoids CVPixelBufferCreate+IOSurface+NV12 crash
                &mut pb,
            );

            if status != 0 || pb.is_null() {
                anyhow::bail!("CVPixelBufferCreate NV12 failed: status={status}");
            }

            let mut pixel_buffer = OwnedPixelBuffer::new(pb)?;
            pixel_buffer.lock()?;

            // Plane 0: Y (full resolution)
            let y_base = CVPixelBufferGetBaseAddressOfPlane(pb, 0) as *mut u8;
            let y_bpr = CVPixelBufferGetBytesPerRowOfPlane(pb, 0);
            if y_base.is_null() {
                anyhow::bail!("NV12 Y plane is null");
            }
            for row in 0..h {
                let src_start = row * w;
                let dst_start = row * y_bpr;
                if src_start + w <= y_plane.len() {
                    std::ptr::copy_nonoverlapping(
                        y_plane.as_ptr().add(src_start),
                        y_base.add(dst_start),
                        w.min(y_bpr),
                    );
                }
            }

            // Plane 1: interleaved UV (NV12 CbCr)
            let uv_base = CVPixelBufferGetBaseAddressOfPlane(pb, 1) as *mut u8;
            let uv_bpr = CVPixelBufferGetBytesPerRowOfPlane(pb, 1);
            if uv_base.is_null() {
                anyhow::bail!("NV12 UV plane is null");
            }
            let uv_w = w / 2;
            let uv_h = h / 2;
            for row in 0..uv_h {
                for col in 0..uv_w {
                    let src_idx = row * uv_w + col;
                    let dst_offset = row * uv_bpr + col * 2;
                    if src_idx < u_plane.len() && src_idx < v_plane.len() {
                        *uv_base.add(dst_offset) = u_plane[src_idx];
                        *uv_base.add(dst_offset + 1) = v_plane[src_idx];
                    }
                }
            }

            pixel_buffer.unlock();
            Ok(pixel_buffer)
        }
    }

    /// Encode a single frame through a VT session and wait for the callback.
    /// `frame_properties` is passed to VTCompressionSessionEncodeFrame (e.g. to force keyframe).
    /// Legacy synchronous users share the same one-input ownership invariant as
    /// the async path. A deadline never flushes the native session or makes a
    /// late callback eligible to overwrite the next input's output slot.
    fn encode_session_frame(&mut self, pixel_buffer: OwnedPixelBuffer) -> Result<NativeOutput> {
        anyhow::ensure!(
            self.native.pending_input_buffer.is_none(),
            "Previous native input is still pending"
        );
        let (pts, duration) = self.make_pts_duration();
        let props = Self::make_force_keyframe_props(self.keyframes.pending())?;
        self.native.callback_ctx.completion.begin();
        Self::submit_session_frame(
            self.native.session,
            &self.native.callback_ctx,
            pixel_buffer.as_ptr(),
            pts,
            duration,
            props.as_ref().map_or(std::ptr::null(), OwnedCf::as_ptr),
        )?;
        self.native.pending_input_buffer = Some(pixel_buffer);
        // Submission consumes a timestamp and carries the current IDR request,
        // even if its callback arrives after this synchronous caller's deadline.
        self.keyframes.submitted();
        self.frame_count += 1;
        match Self::collect_session_frame(
            &self.native.callback_ctx,
            low_latency_wait_budget(self.fps),
        )? {
            Some(output) => {
                self.native.pending_input_buffer = None;
                Ok(output)
            }
            None => anyhow::bail!(
                "Native callback deadline elapsed; retaining input until completion or retirement"
            ),
        }
    }

    /// Submit a frame for async encoding (reset callback + submit). Returns immediately.
    fn submit_session_frame(
        session: VTCompressionSessionRef,
        ctx: &Arc<CallbackCtx>,
        pixel_buffer: CVPixelBufferRef,
        pts: CMTime,
        duration: CMTime,
        frame_properties: CFDictionaryRef,
    ) -> Result<()> {
        {
            let mut guard = ctx.output.lock().unwrap();
            *guard = None;
            ctx.has_data
                .store(false, std::sync::atomic::Ordering::Release);
        }
        unsafe {
            let status = VTCompressionSessionEncodeFrame(
                session,
                pixel_buffer,
                pts,
                duration,
                frame_properties,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            if status != 0 {
                anyhow::bail!("VTCompressionSessionEncodeFrame failed: {status}");
            }
        }
        Ok(())
    }

    /// Wait only for the callback. A timeout preserves the submitted input;
    /// native flushing could block indefinitely and belongs outside this API.
    fn collect_session_frame(
        ctx: &Arc<CallbackCtx>,
        timeout: std::time::Duration,
    ) -> Result<Option<NativeOutput>> {
        let guard = ctx.output.lock().unwrap();
        let (mut guard, _) = ctx
            .ready
            .wait_timeout_while(guard, timeout, |_| {
                !ctx.has_data.load(std::sync::atomic::Ordering::Acquire)
            })
            .unwrap();
        if ctx.has_data.load(std::sync::atomic::Ordering::Acquire) {
            Ok(guard.take())
        } else {
            Ok(None)
        }
    }

    /// Compute PTS and duration for the current frame.
    fn make_pts_duration(&self) -> (CMTime, CMTime) {
        // 600 Hz truncates 240 fps to 300 fps. Preserve sub-frame cadence for
        // high-refresh dual-view encoding and standard fractional frame rates.
        let scale = 60_000;
        let frame_duration = (f64::from(scale) / f64::from(self.fps)).round().max(1.0) as i64;
        let pts = CMTime::make(self.frame_count as i64 * frame_duration, scale);
        (pts, CMTime::make(frame_duration, scale))
    }

    fn replace_pending_input_buffer(&mut self, pixel_buffer: OwnedPixelBuffer) {
        self.native.pending_input_buffer = Some(pixel_buffer);
    }

    fn clear_pending_input_buffer(&mut self) {
        self.native.pending_input_buffer = None;
    }

    /// Keep the keyframe request pending until native submission succeeds.
    fn make_force_keyframe_props(pending: bool) -> Result<Option<OwnedCf>> {
        if !pending {
            return Ok(None);
        }
        unsafe {
            let keys = [kVTEncodeFrameOptionKey_ForceKeyFrame];
            let values = [kCFBooleanTrue];
            Ok(Some(OwnedCf::dictionary(&keys, &values)?))
        }
    }
}

impl VideoEncoder for VtEncoder {
    fn backend_name(&self) -> &'static str {
        "videotoolbox"
    }

    fn encode_bgra(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: usize,
    ) -> Result<EncodedFrame> {
        validate_bgra_layout(data, width, height, stride, self.width, self.height)?;

        // Convert BGRA → NV12 full-range via vImage SIMD (falls back to scalar).
        let pixel_buffer = self.create_nv12_vimage(
            self.native.session,
            self.width,
            self.height,
            data,
            width,
            height,
            stride,
        )?;

        let output = self.encode_session_frame(pixel_buffer).map_err(|error| {
            self.keyframes.request();
            error
        })?;

        self.keyframes.complete(&output);
        let encoded = output.into_frame(self.width, self.height);
        let nal_data = &encoded.data;
        let is_keyframe = encoded.is_keyframe;

        // NAL type diagnostic for first 10 frames
        if self.frame_count <= 10 {
            let mut nal_types = Vec::new();
            let mut profile_info = String::new();
            let mut scan = 0usize;
            while scan + 4 < nal_data.len() {
                if nal_data[scan] == 0
                    && nal_data[scan + 1] == 0
                    && nal_data[scan + 2] == 0
                    && nal_data[scan + 3] == 1
                {
                    scan += 4;
                    if scan < nal_data.len() {
                        let nal_type = nal_data[scan] & 0x1F;
                        let nal_name = match nal_type {
                            1 => "P-slice",
                            5 => "IDR",
                            6 => "SEI",
                            7 => "SPS",
                            8 => "PPS",
                            9 => "AUD",
                            _ => "other",
                        };
                        nal_types.push(format!("{}({})", nal_name, nal_type));
                        // Extract SPS profile info
                        if nal_type == 7 && scan + 3 < nal_data.len() {
                            let profile_idc = nal_data[scan + 1];
                            let constraint = nal_data[scan + 2];
                            let level_idc = nal_data[scan + 3];
                            let name = match profile_idc {
                                66 => "Baseline",
                                77 => "Main",
                                100 => "High",
                                _ => "Unknown",
                            };
                            profile_info = format!(
                                "{}(idc={},constraint=0x{:02X},level={})",
                                name, profile_idc, constraint, level_idc
                            );
                        }
                    }
                } else {
                    scan += 1;
                }
            }
            tracing::debug!(
                frame = self.frame_count,
                output_bytes = nal_data.len(),
                is_keyframe,
                nal_units = nal_types.join(", "),
                profile = profile_info,
                "VideoToolbox NAL diagnostic"
            );
        }

        if self.frame_count % 300 == 0 {
            tracing::debug!(
                frame = self.frame_count,
                output_bytes = nal_data.len(),
                is_keyframe,
                "VideoToolbox encode result"
            );
        }

        Ok(encoded)
    }

    fn encode_pixel_buffer(
        &mut self,
        pixel_buffer_ptr: *mut c_void,
        force_keyframe: bool,
    ) -> Result<EncodedFrame> {
        anyhow::ensure!(
            !pixel_buffer_ptr.is_null(),
            "Cannot encode a null pixel buffer"
        );
        if force_keyframe {
            self.keyframes.request();
        }

        // Own the borrowed input through any late callback or native retirement.
        let pixel_buffer = unsafe { OwnedPixelBuffer::new(CVPixelBufferRetain(pixel_buffer_ptr))? };
        let output = self.encode_session_frame(pixel_buffer).map_err(|error| {
            self.keyframes.request();
            error
        })?;

        self.keyframes.complete(&output);

        Ok(output.into_frame(self.width, self.height))
    }

    fn encode_bgra_444(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: usize,
    ) -> Result<Avc444EncodedFrame> {
        self.encode_avc444(data, width, height, stride)
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        anyhow::ensure!(
            (1..=crate::MAX_BITRATE_BPS).contains(&bitrate_bps),
            "Invalid bitrate"
        );
        if self.applied_bitrate == bitrate_bps {
            return Ok(());
        }
        unsafe {
            let bitrate = OwnedCf::new(cf_i32(bitrate_bps as i32))?;
            set_required_property(
                self.native.session,
                kVTCompressionPropertyKey_AverageBitRate,
                bitrate.as_ptr(),
                "AverageBitRate",
            )?;
        }
        self.applied_bitrate = bitrate_bps;
        tracing::debug!(bitrate_bps, "VideoToolbox accepted bitrate update");
        Ok(())
    }

    fn applied_bitrate(&self) -> Option<u32> {
        Some(self.applied_bitrate)
    }

    fn force_keyframe(&mut self) {
        self.keyframes.request();
    }

    fn supports_444(&self) -> bool {
        self.mode_444
    }

    fn supports_pixel_buffer(&self) -> bool {
        true
    }

    fn supports_pipelining(&self) -> bool {
        true
    }

    fn encode_completion(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<Option<crate::EncodeTiming>>> {
        Some(self.native.callback_ctx.completion.subscribe())
    }

    fn last_encode_timing(&self) -> Option<crate::EncodeTiming> {
        self.native.callback_ctx.completion.timing()
    }

    fn submit_bgra(&mut self, data: &[u8], width: u32, height: u32, stride: usize) -> Result<()> {
        validate_bgra_layout(data, width, height, stride, self.width, self.height)?;
        anyhow::ensure!(
            self.native.pending_input_buffer.is_none(),
            "Previous async encode is still pending"
        );
        self.native.callback_ctx.completion.begin();
        let pixel_buffer = self.create_nv12_vimage(
            self.native.session,
            self.width,
            self.height,
            data,
            width,
            height,
            stride,
        )?;
        let (pts, duration) = self.make_pts_duration();
        let frame_props = Self::make_force_keyframe_props(self.keyframes.pending())?;

        let result = Self::submit_session_frame(
            self.native.session,
            &self.native.callback_ctx,
            pixel_buffer.as_ptr(),
            pts,
            duration,
            frame_props
                .as_ref()
                .map_or(std::ptr::null(), OwnedCf::as_ptr),
        );

        match result {
            Ok(()) => {
                self.replace_pending_input_buffer(pixel_buffer);
                self.keyframes.submitted();
                self.frame_count += 1;
                Ok(())
            }
            Err(e) => {
                self.keyframes.request();
                Err(e)
            }
        }
    }

    fn submit_pixel_buffer(&mut self, ptr: *mut std::ffi::c_void) -> Result<()> {
        anyhow::ensure!(
            self.native.pending_input_buffer.is_none(),
            "Previous async encode is still pending"
        );
        self.native.callback_ctx.completion.begin();
        let (pts, duration) = self.make_pts_duration();
        let frame_props = Self::make_force_keyframe_props(self.keyframes.pending())?;
        anyhow::ensure!(!ptr.is_null(), "Cannot encode a null pixel buffer");
        let pixel_buffer = unsafe { OwnedPixelBuffer::new(CVPixelBufferRetain(ptr))? };

        let result = Self::submit_session_frame(
            self.native.session,
            &self.native.callback_ctx,
            pixel_buffer.as_ptr(),
            pts,
            duration,
            frame_props
                .as_ref()
                .map_or(std::ptr::null(), OwnedCf::as_ptr),
        );

        match result {
            Ok(()) => {
                self.replace_pending_input_buffer(pixel_buffer);
                self.keyframes.submitted();
                self.frame_count += 1;
                Ok(())
            }
            Err(e) => {
                self.keyframes.request();
                Err(e)
            }
        }
    }

    fn collect_encoded(&mut self, timeout: std::time::Duration) -> Result<Option<EncodedFrame>> {
        // Zero duration is a nonblocking poll, never a request to flush the
        // hardware codec or release an input buffer still being encoded.
        if timeout.is_zero()
            && !self
                .native
                .callback_ctx
                .has_data
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(None);
        }
        let collected = Self::collect_session_frame(&self.native.callback_ctx, timeout)?;
        let completed = self
            .native
            .callback_ctx
            .has_data
            .load(std::sync::atomic::Ordering::Acquire);
        if !completed {
            return Ok(None);
        }
        self.clear_pending_input_buffer();
        let output = collected.unwrap_or(NativeOutput::Failed);
        self.keyframes.complete(&output);
        Ok(Some(output.into_frame(self.width, self.height)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_avc444_single_stream_preserves_one_pixel_chroma_with_width_padding() {
        for (width, height) in [(128, 64), (126, 60)] {
            let mut encoder =
                VtEncoder::new_with_quality(128, 64, 30.0, 12_000_000, true, Quality::HighQuality)
                    .unwrap();
            crate::color::tests::assert_avc444_fidelity(&mut encoder, width, height);
        }
    }

    #[test]
    fn actual_videotoolbox_output_preserves_colors_with_simd_and_scalar_nv12() {
        for simd in [true, false] {
            let mut encoder =
                VtEncoder::new_with_quality(128, 64, 30.0, 3_000_000, false, Quality::HighQuality)
                    .unwrap();
            if !simd {
                encoder.vimage = None;
            }
            crate::color::tests::assert_encoded_color_fidelity(&mut encoder, 128, 64);
        }
    }

    #[test]
    fn malformed_bgra_layouts_are_rejected_before_native_conversion() {
        let bytes = vec![0; 16 * 16 * 4];
        for (width, height, stride) in [
            (0, 16, 64),
            (16, 0, 64),
            (17, 16, 68),
            (16, 16, 63),
            (16, 16, 65),
            (16, 16, usize::MAX),
        ] {
            assert!(validate_bgra_layout(&bytes, width, height, stride, 16, 16).is_err());
        }
        assert!(validate_bgra_layout(&bytes, 16, 16, 64, 16, 16).is_ok());
        // The final row need not carry trailing stride padding.
        assert!(validate_bgra_layout(&vec![0; 15 * 80 + 64], 16, 16, 80, 16, 16).is_ok());
    }

    #[test]
    fn timed_collection_does_not_complete_or_discard_an_outstanding_callback() {
        let ctx = Arc::new(CallbackCtx::default());
        ctx.completion.begin();
        assert!(
            VtEncoder::collect_session_frame(&ctx, std::time::Duration::from_millis(1))
                .unwrap()
                .is_none()
        );
        assert!(ctx.completion.timing().is_none());
        *ctx.output.lock().unwrap() = Some(NativeOutput::Encoded(vec![1, 2, 3], true));
        drop(CallbackFinished(&ctx));
        assert_eq!(
            VtEncoder::collect_session_frame(&ctx, std::time::Duration::ZERO).unwrap(),
            Some(NativeOutput::Encoded(vec![1, 2, 3], true))
        );
    }

    #[tokio::test]
    async fn failed_native_callback_wakes_async_consumer() {
        let ctx = CallbackCtx::default();
        ctx.completion.begin();
        let mut completion = ctx.completion.subscribe();
        encode_callback(
            &ctx as *const CallbackCtx as *mut c_void,
            std::ptr::null_mut(),
            -1,
            0,
            std::ptr::null_mut(),
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            completion.wait_for(|timing| timing.is_some()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(ctx.has_data.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(*ctx.output.lock().unwrap(), Some(NativeOutput::Failed));
    }
}

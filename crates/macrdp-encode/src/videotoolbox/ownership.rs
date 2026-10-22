//! Core Foundation's create/retain references are owned immediately, including
//! on failed property updates and early returns from encoding.
use super::*;

pub(super) struct OwnedCf(CFTypeRef);
impl OwnedCf {
    pub unsafe fn new(pointer: CFTypeRef) -> Result<Self> {
        anyhow::ensure!(!pointer.is_null(), "Core Foundation allocation failed");
        Ok(Self(pointer))
    }
    pub fn as_ptr(&self) -> CFTypeRef {
        self.0
    }

    /// Retain every key/value independently of the scoped owners supplying it.
    /// Native consumers may retain this dictionary after its creator returns.
    pub unsafe fn dictionary(keys: &[CFTypeRef], values: &[CFTypeRef]) -> Result<Self> {
        anyhow::ensure!(
            keys.len() == values.len(),
            "CF dictionary key/value count mismatch"
        );
        anyhow::ensure!(
            keys.iter().chain(values).all(|value| !value.is_null()),
            "CF dictionary contains a null key or value"
        );
        let count = isize::try_from(keys.len())?;
        Self::new(CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            count,
            std::ptr::addr_of!(kCFTypeDictionaryKeyCallBacks),
            std::ptr::addr_of!(kCFTypeDictionaryValueCallBacks),
        ))
    }
}
impl Drop for OwnedCf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

pub(super) struct OwnedPixelBuffer {
    pointer: CVPixelBufferRef,
    locked: bool,
}
// Only ownership crosses threads; pixel access remains local and locked.
unsafe impl Send for OwnedPixelBuffer {}
impl OwnedPixelBuffer {
    pub unsafe fn new(pointer: CVPixelBufferRef) -> Result<Self> {
        anyhow::ensure!(
            !pointer.is_null(),
            "Core Video returned a null pixel buffer"
        );
        Ok(Self {
            pointer,
            locked: false,
        })
    }
    pub fn as_ptr(&self) -> CVPixelBufferRef {
        self.pointer
    }
    pub fn lock(&mut self) -> Result<()> {
        let status = unsafe { CVPixelBufferLockBaseAddress(self.pointer, 0) };
        anyhow::ensure!(status == 0, "CVPixelBufferLockBaseAddress failed: {status}");
        self.locked = true;
        Ok(())
    }
    pub fn unlock(&mut self) {
        if self.locked {
            unsafe {
                CVPixelBufferUnlockBaseAddress(self.pointer, 0);
            }
            self.locked = false;
        }
    }
}
impl Drop for OwnedPixelBuffer {
    fn drop(&mut self) {
        self.unlock();
        unsafe { CVPixelBufferRelease(self.pointer) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    unsafe extern "C" fn release_bytes(context: *mut c_void, _bytes: *const c_void) {
        (*(context as *const AtomicUsize)).fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn dictionary_keeps_native_values_alive_after_their_scoped_owner_drops() {
        // A release callback directly verifies ownership; CFNumber retain
        // counts can be misleading because small numbers may be cached/tagged.
        let releases = AtomicUsize::new(0);
        let mut bytes = vec![0u8; 16 * 16 * 4];
        let dictionary = unsafe {
            let mut raw = std::ptr::null_mut();
            check_status(
                CVPixelBufferCreateWithBytes(
                    std::ptr::null(),
                    16,
                    16,
                    K_CV_PIXEL_FORMAT_32BGRA,
                    bytes.as_mut_ptr().cast(),
                    64,
                    release_bytes as *const c_void,
                    &releases as *const _ as *mut c_void,
                    std::ptr::null(),
                    &mut raw,
                ),
                "test CVPixelBufferCreateWithBytes",
            )
            .unwrap();
            let value = OwnedPixelBuffer::new(raw).unwrap();
            let key = OwnedCf::new(CFStringCreateWithCString(
                std::ptr::null(),
                b"retained encoder property\0".as_ptr().cast(),
                0x08000100,
            ))
            .unwrap();
            OwnedCf::dictionary(&[key.as_ptr()], &[value.as_ptr().cast()]).unwrap()
        };
        assert_eq!(releases.load(Ordering::SeqCst), 0);
        drop(dictionary);
        assert_eq!(releases.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn early_error_unlocks_and_releases_the_owned_input_buffer() {
        let releases = AtomicUsize::new(0);
        let mut bytes = vec![0u8; 16 * 16 * 4];
        let mut fail = || -> Result<()> {
            let mut raw = std::ptr::null_mut();
            let status = unsafe {
                CVPixelBufferCreateWithBytes(
                    std::ptr::null(),
                    16,
                    16,
                    K_CV_PIXEL_FORMAT_32BGRA,
                    bytes.as_mut_ptr().cast(),
                    64,
                    release_bytes as *const c_void,
                    &releases as *const _ as *mut c_void,
                    std::ptr::null(),
                    &mut raw,
                )
            };
            check_status(status, "test CVPixelBufferCreateWithBytes")?;
            let mut owned = unsafe { OwnedPixelBuffer::new(raw)? };
            owned.lock()?;
            check_status(-1, "injected encode failure")?;
            Ok(())
        };
        assert!(fail().is_err());
        assert_eq!(releases.load(Ordering::SeqCst), 1);
    }
}

pub(super) struct NativeSession {
    pub session: VTCompressionSessionRef,
    pub callback_ctx: Arc<CallbackCtx>,
    pub pending_input_buffer: Option<OwnedPixelBuffer>,
    pub retirement: Option<crate::retirement::RetirementWorker>,
}
unsafe impl Send for NativeSession {}

impl Drop for NativeSession {
    fn drop(&mut self) {
        // Move the complete native owner, including every callback reference,
        // to the already-reserved worker before any invalidation can block.
        struct Retired {
            session: VTCompressionSessionRef,
            _callback_ctx: Arc<CallbackCtx>,
            _input: Option<OwnedPixelBuffer>,
            retirement_id: u64,
        }
        unsafe impl Send for Retired {}
        impl Retired {
            fn cleanup(self) {
                let started = std::time::Instant::now();
                tracing::info!(target: "macrdp_transition",
                    component = "video_encoder", worker_pid = std::process::id(),
                    retirement_id = self.retirement_id, phase = "vt_invalidate", state = "begin",
                    "Encoder lifecycle transition");
                unsafe {
                    VTCompressionSessionInvalidate(self.session);
                }
                tracing::info!(target: "macrdp_transition",
                    component = "video_encoder", worker_pid = std::process::id(),
                    retirement_id = self.retirement_id, phase = "vt_invalidate", state = "end", outcome = "returned",
                    elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                    "Encoder lifecycle transition");
                let started = std::time::Instant::now();
                tracing::info!(target: "macrdp_transition",
                    component = "video_encoder", worker_pid = std::process::id(),
                    retirement_id = self.retirement_id, phase = "vt_release", state = "begin",
                    "Encoder lifecycle transition");
                unsafe {
                    CFRelease(self.session);
                }
                tracing::info!(target: "macrdp_transition",
                    component = "video_encoder", worker_pid = std::process::id(),
                    retirement_id = self.retirement_id, phase = "vt_release", state = "end", outcome = "returned",
                    elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                    "Encoder lifecycle transition");
                // Retained buffers/context drop only after invalidation.
            }
        }
        let retirement = self
            .retirement
            .take()
            .expect("native session owns a retirement reservation");
        let retired = Retired {
            session: self.session,
            _callback_ctx: self.callback_ctx.clone(),
            _input: self.pending_input_buffer.take(),
            retirement_id: retirement.diagnostic_id(),
        };
        retirement.retire(move || retired.cleanup());
    }
}

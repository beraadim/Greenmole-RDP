//! Core Audio marshals CFData messages into the sandboxed HAL plug-in and
//! supplies the authenticated caller PID. No privileged helper is used at runtime.

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{c_char, c_void};
    use std::mem::size_of;
    use std::ptr;

    type CFRef = *const c_void;

    #[repr(C)]
    struct PropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    impl PropertyAddress {
        const fn global(selector: [u8; 4]) -> Self {
            Self {
                selector: u32::from_be_bytes(selector),
                scope: u32::from_be_bytes(*b"glob"),
                element: 0,
            }
        }
    }

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectGetPropertyData(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
            data: *mut c_void,
        ) -> i32;
        fn AudioObjectSetPropertyData(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: u32,
            data: *const c_void,
        ) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(
            allocator: CFRef,
            bytes: *const c_char,
            encoding: u32,
        ) -> CFRef;
        fn CFDataCreate(allocator: CFRef, bytes: *const u8, length: isize) -> CFRef;
        fn CFGetTypeID(value: CFRef) -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFNumberGetValue(number: CFRef, number_type: isize, value: *mut c_void) -> u8;
        fn CFRelease(value: CFRef);
    }

    struct OwnedCf(CFRef);

    impl Drop for OwnedCf {
        fn drop(&mut self) {
            // All constructors below either reject null or wrap a successful
            // Core Foundation create/copy result whose ownership is ours.
            unsafe {
                CFRelease(self.0);
            }
        }
    }

    pub(crate) struct Driver {
        device: u32,
    }

    impl Driver {
        pub(crate) fn connect() -> anyhow::Result<Self> {
            // kAudioHardwarePropertyTranslateUIDToDevice takes CFStringRef as
            // its qualifier and returns an AudioObjectID (zero means absent).
            let uid = unsafe {
                CFStringCreateWithCString(
                    ptr::null(),
                    b"com.macrdp.microphone\0".as_ptr().cast::<c_char>(),
                    0x0800_0100,
                )
            };
            anyhow::ensure!(
                !uid.is_null(),
                "Could not allocate microphone device identifier"
            );
            let uid = OwnedCf(uid);
            let mut device = 0_u32;
            let mut size = size_of::<u32>() as u32;
            let status = unsafe {
                AudioObjectGetPropertyData(
                    1,
                    &PropertyAddress::global(*b"uidd"),
                    size_of::<CFRef>() as u32,
                    (&uid.0 as *const CFRef).cast(),
                    &mut size,
                    (&mut device as *mut u32).cast(),
                )
            };
            anyhow::ensure!(
                status == 0 && size == size_of::<u32>() as u32 && device != 0,
                "Greenmole microphone driver is unavailable; install it in Microphone settings"
            );
            let driver = Self { device };
            driver.check_version()?;
            Ok(driver)
        }

        fn check_version(&self) -> anyhow::Result<()> {
            let mut number: CFRef = ptr::null();
            let mut size = size_of::<CFRef>() as u32;
            let status = unsafe {
                AudioObjectGetPropertyData(
                    self.device,
                    &PropertyAddress::global(*b"gmmv"),
                    0,
                    ptr::null(),
                    &mut size,
                    (&mut number as *mut CFRef).cast(),
                )
            };
            // A successful CFPropertyList getter returns a retained CF object.
            anyhow::ensure!(
                status == 0 && size == size_of::<CFRef>() as u32 && !number.is_null(),
                "Greenmole microphone driver protocol is unavailable; reinstall it"
            );
            let number = OwnedCf(number);
            let mut version = 0_i32;
            let valid = unsafe {
                CFGetTypeID(number.0) == CFNumberGetTypeID()
                    && CFNumberGetValue(number.0, 3, (&mut version as *mut i32).cast()) != 0
            };
            anyhow::ensure!(
                valid && version == crate::wire::VERSION,
                "Greenmole microphone driver needs to be updated"
            );
            Ok(())
        }

        pub(crate) fn send(&self, bytes: &[u8]) -> anyhow::Result<()> {
            anyhow::ensure!(
                bytes.len() <= crate::wire::HEADER_BYTES + crate::wire::MAX_FRAMES * 8,
                "Microphone driver message exceeded limit"
            );
            let data = unsafe { CFDataCreate(ptr::null(), bytes.as_ptr(), bytes.len() as isize) };
            anyhow::ensure!(
                !data.is_null(),
                "Could not allocate microphone driver message"
            );
            let data = OwnedCf(data);
            let status = unsafe {
                AudioObjectSetPropertyData(
                    self.device,
                    &PropertyAddress::global(*b"gmmf"),
                    0,
                    ptr::null(),
                    size_of::<CFRef>() as u32,
                    (&data.0 as *const CFRef).cast(),
                )
            };
            anyhow::ensure!(
                status == 0,
                "Microphone driver rejected a session message (OSStatus {status})"
            );
            Ok(())
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub(crate) struct Driver;
    impl Driver {
        pub(crate) fn connect() -> anyhow::Result<Self> {
            anyhow::bail!("Microphone redirection requires macOS")
        }
        pub(crate) fn send(&self, _bytes: &[u8]) -> anyhow::Result<()> {
            anyhow::bail!("Microphone redirection requires macOS")
        }
    }
}

pub(crate) use platform::Driver;

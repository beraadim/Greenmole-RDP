//! Read-only lookup of logical shortcut letters in the host's active layout.
//!
//! Carbon's TextInputSources APIs require the main thread in GUI applications.
//! A process that never initializes AppKit may instead serialize its calls.
//! Keep that choice explicit: merely being called from a worker is not evidence
//! that this library is running in a command-line process.
use std::sync::Mutex;

use core_foundation_sys::base::{CFRelease, CFTypeRef};
use core_foundation_sys::data::{CFDataGetBytePtr, CFDataGetLength, CFDataRef};
use core_foundation_sys::string::CFStringRef;

type LetterMap = [Option<u16>; 26];

static LAYOUT: Mutex<LayoutState> = Mutex::new(LayoutState {
    headless: false,
    letters: [None; 26],
});

struct LayoutState {
    headless: bool,
    letters: LetterMap,
}

impl LayoutState {
    fn resolve(
        &mut self,
        index: usize,
        main_thread: bool,
        read: impl FnOnce() -> LetterMap,
    ) -> Option<u16> {
        if main_thread || self.headless {
            self.letters = read();
        }
        self.letters[index]
    }
}

unsafe extern "C" {
    fn pthread_main_np() -> std::ffi::c_int;
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    // Inspect only on the main thread, without creating NSApplication.
    static NSApp: *const std::ffi::c_void;
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> CFTypeRef;
    fn TISGetInputSourceProperty(source: CFTypeRef, key: CFStringRef) -> CFTypeRef;
    static kTISPropertyUnicodeKeyLayoutData: CFStringRef;
    fn LMGetKbdType() -> u8;
    fn UCKeyTranslate(
        layout: *const u8,
        key: u16,
        action: u16,
        modifiers: u32,
        keyboard_type: u32,
        options: u32,
        dead_state: *mut u32,
        capacity: usize,
        count: *mut usize,
        output: *mut u16,
    ) -> i32;
}

fn is_main_thread() -> bool {
    unsafe { pthread_main_np() != 0 }
}

/// Opt a dedicated command-line process into serialized background layout reads.
///
/// Call on the process main thread before starting workers. This process must
/// never initialize AppKit/NSApplication or a UI event loop afterwards, and all
/// of its TextInputSources access must go through this module. GUI applications
/// must leave the default mode in place and use `refresh_on_main_thread` instead.
/// The existing application singleton is checked without creating one.
pub fn configure_for_headless_process() -> anyhow::Result<()> {
    anyhow::ensure!(
        is_main_thread(),
        "Headless keyboard setup requires the process main thread"
    );
    anyhow::ensure!(
        unsafe { NSApp.is_null() },
        "Headless keyboard setup cannot run after AppKit initialization"
    );
    let mut state = LAYOUT
        .lock()
        .map_err(|_| anyhow::anyhow!("Host keyboard layout lock failed"))?;
    state.headless = true;
    // Warm Carbon before a remote shortcut needs it. A missing layout leaves
    // physical fallback available; it must not prevent server startup.
    state.letters = read_native_layout();
    Ok(())
}

/// Refresh the background-readable snapshot from an embedding GUI's main thread.
///
/// Call during setup and when the selected input source changes. Background
/// callers use only this snapshot; they never wait for a UI event loop, create
/// NSApplication, or call thread-unsafe Carbon functions themselves.
pub fn refresh_on_main_thread() -> anyhow::Result<()> {
    anyhow::ensure!(
        is_main_thread(),
        "Host keyboard layout refresh requires the process main thread"
    );
    let mut state = LAYOUT
        .lock()
        .map_err(|_| anyhow::anyhow!("Host keyboard layout lock failed"))?;
    state.letters = read_native_layout();
    Ok(())
}

/// Resolve a Latin shortcut letter without switching input sources or posting
/// input. Non-Latin input methods use the last ASCII-capable keyboard layout.
/// An unconfigured background embedding returns its main-thread snapshot, or
/// `None` if none is available, so callers can retain their physical fallback.
pub fn keycode_for_character(character: char) -> Option<u16> {
    if !character.is_ascii_alphabetic() {
        return None;
    }
    let index = usize::from(character.to_ascii_lowercase() as u8 - b'a');
    let mut state = LAYOUT.lock().ok()?;
    state.resolve(index, is_main_thread(), read_native_layout)
}

// Called only while LAYOUT is locked and the process/thread policy permits
// Carbon access. Copy-rule source ownership encloses the borrowed layout data.
fn read_native_layout() -> LetterMap {
    let mut letters = [None; 26];
    unsafe {
        let source = TISCopyCurrentASCIICapableKeyboardLayoutInputSource();
        if source.is_null() {
            return letters;
        }
        struct Source(CFTypeRef);
        impl Drop for Source {
            fn drop(&mut self) {
                unsafe { CFRelease(self.0) }
            }
        }
        let source = Source(source);
        let data =
            TISGetInputSourceProperty(source.0, kTISPropertyUnicodeKeyLayoutData) as CFDataRef;
        if data.is_null() || CFDataGetLength(data) <= 0 {
            return letters;
        }
        let bytes = CFDataGetBytePtr(data);
        if bytes.is_null() {
            return letters;
        }
        let keyboard_type = u32::from(LMGetKbdType());
        for key in 0..128u16 {
            let mut dead = 0;
            let mut count = 0;
            let mut output = [0u16; 8];
            // Display action and no-dead-keys inspect the base character, with
            // no composition carried from one candidate key to the next.
            if UCKeyTranslate(
                bytes,
                key,
                3,
                0,
                keyboard_type,
                1,
                &mut dead,
                output.len(),
                &mut count,
                output.as_mut_ptr(),
            ) == 0
                && count == 1
                && (b'a' as u16..=b'z' as u16).contains(&output[0])
            {
                letters[usize::from(output[0] - b'a' as u16)].get_or_insert(key);
            }
        }
    }
    letters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_embedding_uses_snapshot_without_entering_carbon() {
        let mut state = LayoutState {
            headless: false,
            letters: [None; 26],
        };
        assert_eq!(
            state.resolve(0, false, || panic!("background Carbon access")),
            None
        );
        state.letters[0] = Some(12);
        assert_eq!(
            state.resolve(0, false, || panic!("background Carbon access")),
            Some(12)
        );
    }

    #[test]
    fn headless_and_main_thread_reads_refresh_instead_of_retaining_an_old_layout() {
        for (headless, main_thread) in [(true, false), (false, true)] {
            let mut state = LayoutState {
                headless,
                letters: [None; 26],
            };
            for keycode in [Some(0), Some(12), None] {
                assert_eq!(
                    state.resolve(0, main_thread, || {
                        let mut layout = [None; 26];
                        layout[0] = keycode;
                        layout
                    }),
                    keycode
                );
            }
        }
    }

    #[test]
    fn thread_policy_serializes_parallel_reads_without_changing_global_mode() {
        let state = Mutex::new(LayoutState {
            headless: true,
            letters: [None; 26],
        });
        let active = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..20 {
                        state.lock().unwrap().resolve(0, false, || {
                            use std::sync::atomic::Ordering;
                            assert_eq!(active.fetch_add(1, Ordering::SeqCst), 0);
                            std::thread::yield_now();
                            assert_eq!(active.fetch_sub(1, Ordering::SeqCst), 1);
                            [None; 26]
                        });
                    }
                });
            }
        });
    }
}

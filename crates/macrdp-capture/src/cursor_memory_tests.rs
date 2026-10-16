use super::CursorCapture;
use std::ffi::c_void;

#[repr(C)]
#[derive(Default)]
struct MallocStatistics {
    blocks_in_use: u32,
    size_in_use: usize,
    max_size_in_use: usize,
    size_allocated: usize,
}

unsafe extern "C" {
    fn malloc_zone_statistics(zone: *mut c_void, statistics: *mut MallocStatistics);
}

fn allocated_bytes() -> usize {
    let mut statistics = MallocStatistics::default();
    // A null zone aggregates all malloc zones. Inspect only allocation totals,
    // never the current screen contents or the contents of process memory.
    unsafe { malloc_zone_statistics(std::ptr::null_mut(), &mut statistics) };
    statistics.size_in_use
}

#[test]
#[ignore = "opt-in live macOS cursor memory regression; run alone with --test-threads=1"]
fn repeated_cursor_polling_keeps_native_allocations_bounded() {
    let mut capture = CursorCapture::default();
    // Warm AppKit's lazy caches, then measure allocations while this worker is
    // still alive: thread-exit cleanup would conceal a missing local pool.
    for _ in 0..64 {
        std::hint::black_box(capture.capture_if_changed().unwrap());
    }
    let baseline = allocated_bytes();
    for _ in 0..1024 {
        std::hint::black_box(capture.capture_if_changed().unwrap());
    }
    let after = allocated_bytes();
    let growth = after.saturating_sub(baseline);
    eprintln!("cursor_memory polls=1024 before_bytes={baseline} after_bytes={after} growth_bytes={growth}");
    assert!(
        growth < 16 * 1024 * 1024,
        "cursor polling retained {growth} bytes after warmup"
    );
}

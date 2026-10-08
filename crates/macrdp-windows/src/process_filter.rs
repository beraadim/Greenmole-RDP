//! Processes that the background Accessibility worker may inspect.

/// An AX request targeting this process can call its AppKit implementation
/// directly on the calling thread. Keep our own UI out of this worker: AppKit
/// window operations require the application's main thread.
pub(crate) fn is_external_application(pid: i32) -> bool {
    pid > 0 && pid as u32 != std::process::id()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_the_current_process_and_invalid_pids() {
        let own_pid = i32::try_from(std::process::id()).expect("process PID fits pid_t");
        for pid in [own_pid, 0, -1, i32::MIN] {
            assert!(!is_external_application(pid), "must reject PID {pid}");
        }
    }

    #[test]
    fn allows_positive_pids_belonging_to_other_processes() {
        let external_pid = if std::process::id() == 1 { 2 } else { 1 };
        assert!(is_external_application(external_pid));
    }
}

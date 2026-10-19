//! Filesystem/process lease tests only: no display, capture, input or TCC calls.

use super::*;
use std::io::{BufRead, BufReader, Write};
use std::ops::{Deref, DerefMut};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const READY_MARKER: &str = "__MACRDP_IDENTITY_LEASE_READY__";
const CHILD_TIMEOUT: Duration = Duration::from_secs(5);

/// A failed assertion must not orphan a child that still holds its parent's
/// lease. The only children here run this synthetic test fixture, never CG APIs.
struct TestChild(Child);

impl TestChild {
    fn spawn(command: &mut Command) -> Self {
        Self(command.spawn().unwrap())
    }

    fn wait_success(&mut self) -> bool {
        let deadline = Instant::now() + CHILD_TIMEOUT;
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status.success();
            }
            assert!(
                Instant::now() < deadline,
                "identity fixture child exceeded its exit deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn await_ready(&mut self) {
        let output = self.0.stdout.take().unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut ready = false;
            for line in BufReader::new(output).lines() {
                match line {
                    Ok(line) if !ready && line.trim_end().ends_with(READY_MARKER) => {
                        ready = true;
                        let _ = sender.send(Ok(()));
                        // Keep draining the harness output until child exit so
                        // its final result cannot encounter a closed stdout.
                    }
                    Ok(_) => {}
                    Err(error) => {
                        let _ = sender.send(Err(error.to_string()));
                        return;
                    }
                }
            }
            if !ready {
                let _ = sender.send(Err("child exited before readiness".to_owned()));
            }
        });
        // The native Rust harness can prefix nocapture output on the same line
        // when RUST_TEST_THREADS=1. The unique suffix avoids depending on its
        // human-readable formatting, and a pipe read can never hang this test.
        receiver
            .recv_timeout(CHILD_TIMEOUT)
            .expect("identity fixture child did not become ready before its deadline")
            .expect("identity fixture child failed before readiness");
    }
}

impl Deref for TestChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl DerefMut for TestChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for TestChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

fn acquire(directory: &tempfile::TempDir, slot: &str) -> Result<(IdentityLease, bool)> {
    IdentityLease::acquire_in(open_directory(directory.path())?, slot)
}

#[test]
fn serial_survives_reconnect_but_generation_changes_and_slots_are_distinct() {
    let directory = fixture();
    let (first, created) = acquire(&directory, "rdp-port-3389").unwrap();
    assert!(created);
    assert_ne!(first.serial(), 0);
    let serial = first.serial();
    let generation = first.record.generation;
    assert!(acquire(&directory, "rdp-port-3389").is_err());
    let (other, _) = acquire(&directory, "rdp-port-3390").unwrap();
    assert_ne!(other.serial(), serial);
    drop(first);
    let (next, created) = acquire(&directory, "rdp-port-3389").unwrap();
    assert!(!created);
    assert_eq!(next.serial(), serial);
    assert_ne!(next.record.generation, generation);
}

#[test]
fn corrupted_disposable_generation_is_repaired_without_changing_serial() {
    let directory = fixture();
    let (first, _) = acquire(&directory, "default").unwrap();
    let serial = first.serial();
    drop(first);
    std::fs::write(directory.path().join("default.lease"), b"interrupted write").unwrap();
    let (next, created) = acquire(&directory, "default").unwrap();
    assert!(!created);
    assert_eq!(next.serial(), serial);
    next.validate_current().unwrap();
}

#[test]
fn incomplete_first_publication_is_replaced_and_staging_is_bounded() {
    let directory = fixture();
    let pending = directory.path().join(".default.pending");
    std::fs::write(&pending, b"interrupted").unwrap();
    std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (lease, created) = acquire(&directory, "default").unwrap();
    assert!(created);
    lease.validate_current().unwrap();
    assert!(!pending.exists());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
}

#[test]
fn invalid_stable_identity_is_never_silently_regenerated() {
    for invalid in [
        b"".as_slice(),
        b"{",
        br#"{"schema":1,"serial":0}"#,
        br#"{"schema":2,"serial":12}"#,
        br#"{"schema":1,"serial":12,"extra":true}"#,
    ] {
        let directory = fixture();
        let stable = directory.path().join("default.json");
        std::fs::write(&stable, invalid).unwrap();
        std::fs::set_permissions(&stable, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(acquire(&directory, "default").is_err());
        assert_eq!(std::fs::read(stable).unwrap(), invalid);
    }
}

#[test]
fn slots_are_bounded_canonical_names_without_path_components() {
    for valid in ["default", "rdp-port-0", "rdp-port-3389", "rdp-port-65535"] {
        validate_slot(valid).unwrap();
    }
    for invalid in [
        "",
        "../default",
        "default/",
        "rdp-port--1",
        "rdp-port-65536",
        "rdp-port-03389",
        "rdp-port-+3389",
        "rdp-port-",
        "other",
        "rdp-port-3389\n",
    ] {
        assert!(validate_slot(invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn unsafe_permissions_symlinks_and_hard_links_fail_closed() {
    let directory = fixture();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(acquire(&directory, "default").is_err());
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let target = directory.path().join("target");
    std::fs::write(&target, b"do not modify").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
    let lease_path = directory.path().join("default.lease");
    symlink(&target, &lease_path).unwrap();
    assert!(acquire(&directory, "default").is_err());
    std::fs::remove_file(&lease_path).unwrap();
    std::fs::hard_link(&target, &lease_path).unwrap();
    assert!(acquire(&directory, "default").is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"do not modify");
    std::fs::remove_file(&lease_path).unwrap();
    std::fs::write(&lease_path, b"").unwrap();
    std::fs::set_permissions(&lease_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(acquire(&directory, "default").is_err());
}

#[test]
fn generation_path_and_persisted_serial_changes_invalidate_owner() {
    let directory = fixture();
    let (lease, _) = acquire(&directory, "default").unwrap();
    let path = directory.path().join("default.lease");
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"{}").unwrap();
    assert!(lease.validate_current().is_err());
    std::fs::write(&path, original).unwrap();
    lease.validate_current().unwrap();
    let stable = directory.path().join("default.json");
    std::fs::write(stable, br#"{"schema":1,"serial":0}"#).unwrap();
    assert!(lease.validate_current().is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"replacement").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(lease.validate_current().is_err());
}

// Invoked only as a subprocess by the following test. It exercises actual exec
// inheritance, not native display APIs, and uses the caller's temporary directory.
#[test]
fn inherited_lease_child() {
    let Some(path) = std::env::var_os("MACRDP_IDENTITY_TEST_DIRECTORY") else {
        return;
    };
    let mode = std::env::var("MACRDP_IDENTITY_TEST_MODE").unwrap();
    let mut input = BufReader::new(std::io::stdin().lock());
    let mut line = String::new();
    input.read_line(&mut line).unwrap();
    let proof: IdentityProof = serde_json::from_str(&line).unwrap();
    let result = IdentityLease::inherit_in(&proof, open_directory(Path::new(&path)).unwrap(), true);
    if mode != "valid" {
        assert!(result.is_err());
        return;
    }
    let lease = result.unwrap();
    lease.validate_current().unwrap();
    assert!(
        lease.retain_until_exit().is_err(),
        "process-lifetime ownership must be bounded to one lease"
    );
    // The full-process duplicate must survive even after a native call's lease
    // value is dropped. Real selectors hold it to their immediate process exit.
    drop(lease);
    println!("{READY_MARKER}");
    std::io::stdout().flush().unwrap();
    line.clear();
    input.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "exit");
}

fn child_command(directory: &tempfile::TempDir, mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "identity::tests::inherited_lease_child",
            "--nocapture",
            "--format",
            "terse",
        ])
        .env("MACRDP_IDENTITY_TEST_DIRECTORY", directory.path())
        .env("MACRDP_IDENTITY_TEST_MODE", mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    command
}

#[test]
fn inherited_open_description_blocks_reuse_through_child_exit() {
    let directory = fixture();
    let (lease, _) = acquire(&directory, "default").unwrap();
    let serial = lease.serial();
    let mut command = child_command(&directory, "valid");
    let proof = lease.selector_proof(&mut command).unwrap();
    let mut child = TestChild::spawn(&mut command);
    drop(command);
    let mut input = child.stdin.take().unwrap();
    serde_json::to_writer(&mut input, &proof).unwrap();
    writeln!(input).unwrap();
    input.flush().unwrap();
    child.await_ready();
    drop(lease);
    assert!(
        acquire(&directory, "default").is_err(),
        "child must retain same owner lock"
    );
    writeln!(input, "exit").unwrap();
    drop(input);
    assert!(child.wait_success());
    let (replacement, created) = acquire(&directory, "default").unwrap();
    assert!(!created);
    assert_eq!(replacement.serial(), serial);
}

#[test]
fn a_separate_open_description_cannot_impersonate_the_owners_inherited_lease() {
    let directory = fixture();
    let (lease, _) = acquire(&directory, "default").unwrap();
    let mut command = child_command(&directory, "wrong-description");
    let mut proof = lease.selector_proof(&mut command).unwrap();
    let unrelated = open_at(
        &lease.directory,
        "default.lease",
        libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )
    .unwrap();
    proof.fd = unrelated.as_raw_fd();
    // This deliberately inherits the wrong description as well as the genuine
    // one, proving inode/generation checks alone are not sufficient.
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(unrelated.as_raw_fd(), libc::F_GETFD);
            if flags < 0
                || libc::fcntl(
                    unrelated.as_raw_fd(),
                    libc::F_SETFD,
                    flags & !libc::FD_CLOEXEC,
                ) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = TestChild::spawn(&mut command);
    drop(command);
    let mut input = child.stdin.take().unwrap();
    serde_json::to_writer(&mut input, &proof).unwrap();
    writeln!(input).unwrap();
    drop(input);
    assert!(child.wait_success());
    lease.validate_current().unwrap();
}

#[test]
fn special_files_and_symlinked_stable_or_pending_paths_are_rejected() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    for name in ["default.json", "default.lease", ".default.pending"] {
        let directory = fixture();
        let path = CString::new(directory.path().join(name).as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        // O_NONBLOCK ensures even a FIFO with no peer fails validation instead
        // of freezing activation before the selector's deadline starts.
        assert!(acquire(&directory, "default").is_err(), "{name}");
    }
    for name in ["default.json", ".default.pending"] {
        let directory = fixture();
        let target = directory.path().join("target");
        std::fs::write(&target, b"unchanged").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, directory.path().join(name)).unwrap();
        assert!(acquire(&directory, "default").is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"unchanged");
    }
}

#[test]
fn selector_proof_keeps_parent_descriptors_close_on_exec() {
    let directory = fixture();
    let (lease, _) = acquire(&directory, "default").unwrap();
    let original_flags = unsafe { libc::fcntl(lease.file.as_raw_fd(), libc::F_GETFD) };
    assert_ne!(original_flags & libc::FD_CLOEXEC, 0);
    let mut command = child_command(&directory, "invalid");
    let proof = lease.selector_proof(&mut command).unwrap();
    assert_ne!(
        unsafe { libc::fcntl(proof.fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
        0
    );
    assert_eq!(
        unsafe { libc::fcntl(lease.file.as_raw_fd(), libc::F_GETFD) },
        original_flags
    );
    drop(command);
    assert_eq!(unsafe { libc::fcntl(proof.fd, libc::F_GETFD) }, -1);
    lease.validate_current().unwrap();
}

#[test]
fn inherited_proof_rejects_invalid_fd_owner_generation_inode_serial_and_slot() {
    let directory = fixture();
    let (lease, _) = acquire(&directory, "default").unwrap();
    for variant in 0..7 {
        let mut command = child_command(&directory, "invalid");
        let mut proof = lease.selector_proof(&mut command).unwrap();
        match variant {
            0 => proof.fd = 2,
            1 => proof.owner_pid = 1,
            2 => proof.generation = [0; 16],
            3 => proof.generation[0] ^= 1,
            4 => proof.inode ^= 1,
            5 => proof.serial = 0,
            6 => proof.slot = "../default".to_owned(),
            _ => unreachable!(),
        }
        let mut child = TestChild::spawn(&mut command);
        drop(command);
        let mut input = child.stdin.take().unwrap();
        serde_json::to_writer(&mut input, &proof).unwrap();
        writeln!(input).unwrap();
        drop(input);
        assert!(child.wait_success(), "proof variant {variant}");
    }
    lease.validate_current().unwrap();
}

#[test]
fn immutable_publication_never_replaces_an_unexpected_final_file() {
    use std::ffi::CString;
    let directory = fixture();
    let pending = directory.path().join(".default.pending");
    let destination = directory.path().join("default.json");
    std::fs::write(&pending, b"new serial").unwrap();
    std::fs::write(&destination, b"original serial").unwrap();
    assert!(publish_noreplace(
        &open_directory(directory.path()).unwrap(),
        &CString::new(".default.pending").unwrap(),
        &CString::new("default.json").unwrap()
    )
    .is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), b"original serial");
    assert_eq!(std::fs::read(&pending).unwrap(), b"new serial");
}

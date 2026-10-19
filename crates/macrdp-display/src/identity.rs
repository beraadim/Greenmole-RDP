//! A stable monitor identity and a separate, process-scoped ownership lease.
//!
//! The serial is persisted per logical server slot. Each owner gets a fresh
//! generation, and selectors inherit the SAME flock open-file description.
//! Closing an owner therefore cannot let a replacement reuse the identity while
//! an old selector is still inside a native transaction. Never explicitly unlock
//! these descriptors: LOCK_UN would also unlock their inherited duplicates.

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::OnceLock;

const MAX_RECORD: usize = 1024;
const SCHEMA: u32 = 1;
// WindowServer can retain a mode-switched display until the helper exits, even
// after its native object is dropped. The OS closes these only on process exit.
static PROCESS_LEASE: OnceLock<File> = OnceLock::new();

pub(crate) use crate::protocol::IdentityProof;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    serial: u32,
    owner_pid: u32,
    generation: [u8; 16],
}

#[path = "identity_store.rs"]
mod store;
use store::*;

pub(crate) struct IdentityLease {
    file: File,
    directory: File,
    slot: String,
    record: Record,
    inherited: bool,
}

impl IdentityLease {
    pub(crate) fn acquire(slot: &str) -> Result<Self> {
        validate_slot(slot)?;
        let directory = identity_directory()?;
        let (lease, created) = Self::acquire_in(directory, slot)?;
        lease.retain_until_exit()?;
        eprintln!(
            "macrdp display identity: slot={slot} serial={} {}",
            lease.serial(),
            if created { "created" } else { "reused" }
        );
        Ok(lease)
    }

    fn acquire_in(directory: File, slot: &str) -> Result<(Self, bool)> {
        validate_slot(slot)?;
        validate_directory(&directory, true)?;
        let file = open_at(
            &directory,
            &lease_filename(slot),
            libc::O_RDWR | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )?;
        validate_file(&file.metadata()?)?;
        // A NEW open description must acquire the lock before reading/writing a
        // generation. A busy slot fails closed instead of inventing a monitor.
        ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "Virtual display identity slot is already in use"
        );
        let (serial, created) = stable_serial(&directory, slot)?;
        let record = Record {
            schema: SCHEMA,
            serial,
            owner_pid: std::process::id(),
            generation: random_generation()?,
        };
        let lease = Self {
            file,
            directory,
            slot: slot.to_owned(),
            record,
            inherited: false,
        };
        lease.validate_path()?;
        let bytes = serde_json::to_vec(&lease.record)?;
        ensure!(
            bytes.len() <= MAX_RECORD,
            "Display identity record is too large"
        );
        // Only the disposable generation record is rewritten. A killed write
        // cannot erase the separate immutable monitor serial.
        lease.file.write_all_at(&bytes, 0)?;
        lease.file.set_len(bytes.len() as u64)?;
        lease.file.sync_all()?;
        lease.validate_current()?;
        Ok((lease, created))
    }

    pub(crate) fn serial(&self) -> u32 {
        self.record.serial
    }

    /// Prepare inheritance without changing parent CLOEXEC flags or reserving a
    /// magic descriptor. The command owns its duplicate until spawn/drop.
    pub(crate) fn selector_proof(&self, command: &mut Command) -> Result<IdentityProof> {
        self.validate_current()?;
        ensure!(!self.inherited, "A selector cannot launch another selector");
        let inherited_file = self.file.try_clone()?;
        let fd = inherited_file.as_raw_fd();
        let metadata = inherited_file.metadata()?;
        let proof = IdentityProof {
            serial: self.serial(),
            owner_pid: self.record.owner_pid,
            generation: self.record.generation,
            fd,
            device: metadata.dev(),
            inode: metadata.ino(),
            slot: self.slot.clone(),
        };
        // SAFETY: pre_exec only performs async-signal-safe fcntl operations.
        // Owning the duplicate in this closure prevents descriptor reuse before
        // spawn. Only the post-fork child clears CLOEXEC for its next exec.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(inherited_file.as_raw_fd(), libc::F_GETFD);
                if flags < 0
                    || libc::fcntl(
                        inherited_file.as_raw_fd(),
                        libc::F_SETFD,
                        flags & !libc::FD_CLOEXEC,
                    ) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(proof)
    }

    pub(crate) fn inherit(proof: &IdentityProof) -> Result<Self> {
        Self::inherit_in(proof, identity_directory()?, true)
    }

    fn inherit_in(proof: &IdentityProof, directory: File, sticky: bool) -> Result<Self> {
        validate_slot(&proof.slot)?;
        ensure!(
            proof.fd >= 3
                && proof.serial != 0
                && proof.owner_pid > 1
                && proof.generation != [0; 16],
            "Invalid inherited display identity proof"
        );
        ensure!(
            (unsafe { libc::getppid() }) as u32 == proof.owner_pid,
            "Display identity owner is no longer this selector's parent"
        );
        // Duplicate first rather than taking ownership of an untrusted number.
        // FD_CLOEXEC also prevents this lease escaping unrelated future execs.
        let duplicate = unsafe { libc::fcntl(proof.fd, libc::F_DUPFD_CLOEXEC, 3) };
        ensure!(
            duplicate >= 0,
            "Inherited display identity descriptor is unavailable"
        );
        // SAFETY: fcntl returned an owned new descriptor.
        let file = unsafe { File::from_raw_fd(duplicate) };
        let metadata = file.metadata()?;
        validate_file(&metadata)?;
        ensure!(
            metadata.dev() == proof.device && metadata.ino() == proof.inode,
            "Inherited display identity descriptor does not match its proof"
        );
        ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "Inherited display identity does not carry its owner's lease"
        );
        // Restore CLOEXEC on the original inherited descriptor too. Its lifetime
        // is deliberately the whole selector process, not a native call scope.
        set_cloexec(proof.fd)?;
        let lease = Self {
            file,
            directory,
            slot: proof.slot.clone(),
            record: Record {
                schema: SCHEMA,
                serial: proof.serial,
                owner_pid: proof.owner_pid,
                generation: proof.generation,
            },
            inherited: true,
        };
        lease.validate_current()?;
        if sticky {
            lease.retain_until_exit()?;
        }
        Ok(lease)
    }

    pub(crate) fn validate_current(&self) -> Result<()> {
        validate_directory(&self.directory, true)?;
        validate_file(&self.file.metadata()?)?;
        self.validate_path()?;
        let current = read_record(&self.file)?;
        ensure!(
            current.serial == self.record.serial
                && current.owner_pid == self.record.owner_pid
                && current.generation == self.record.generation,
            "Display identity ownership generation changed"
        );
        let live_owner = if self.inherited {
            (unsafe { libc::getppid() }) as u32
        } else {
            std::process::id()
        };
        ensure!(
            live_owner == self.record.owner_pid && live_owner > 1,
            "Display identity owner is no longer alive"
        );
        Ok(())
    }

    fn validate_path(&self) -> Result<()> {
        let current = open_at(
            &self.directory,
            &lease_filename(&self.slot),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0,
        )?;
        let expected = self.file.metadata()?;
        let actual = current.metadata()?;
        validate_file(&actual)?;
        ensure!(
            expected.dev() == actual.dev() && expected.ino() == actual.ino(),
            "Display identity path was replaced"
        );
        ensure!(
            read_stable_serial(&self.directory, &self.slot)? == self.serial(),
            "Persisted virtual display serial changed"
        );
        Ok(())
    }

    fn retain_until_exit(&self) -> Result<()> {
        PROCESS_LEASE.set(self.file.try_clone()?).map_err(|_| {
            anyhow::anyhow!("A display helper may own only one process-lifetime identity lease")
        })
    }
}

fn read_record(file: &File) -> Result<Record> {
    let length = file.metadata()?.len();
    ensure!(
        length > 0 && length <= MAX_RECORD as u64,
        "Invalid display identity record size"
    );
    let mut bytes = vec![0; length as usize];
    file.read_exact_at(&mut bytes, 0)?;
    let record: Record =
        serde_json::from_slice(&bytes).context("Invalid display identity record")?;
    ensure!(
        record.schema == SCHEMA
            && record.serial != 0
            && record.owner_pid > 1
            && record.generation != [0; 16],
        "Invalid display identity record values"
    );
    Ok(record)
}

fn set_cloexec(fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    ensure!(flags >= 0, "Display identity descriptor is unavailable");
    ensure!(
        unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == 0,
        "Could not protect inherited display identity descriptor"
    );
    Ok(())
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;

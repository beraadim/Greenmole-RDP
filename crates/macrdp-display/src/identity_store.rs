//! Validated, pinned directory/file access and immutable monitor serial storage.
//! This module never accesses displays or changes permissions of existing files.

use super::{MAX_RECORD, SCHEMA};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::ffi::{CStr, CString};
use std::fs::{File, Metadata};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StableIdentity {
    schema: u32,
    serial: u32,
}

pub(super) fn validate_slot(slot: &str) -> Result<()> {
    let canonical = slot == "default"
        || slot.strip_prefix("rdp-port-").is_some_and(|port| {
            port.parse::<u16>()
                .ok()
                .is_some_and(|value| value.to_string() == port)
        });
    ensure!(canonical, "Invalid virtual display identity slot");
    Ok(())
}

pub(super) fn lease_filename(slot: &str) -> String {
    format!("{slot}.lease")
}

pub(super) fn read_stable_serial(directory: &File, slot: &str) -> Result<u32> {
    let file = open_at(
        directory,
        &format!("{slot}.json"),
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        0,
    )?;
    validate_file(&file.metadata()?)?;
    let length = file.metadata()?.len();
    ensure!(
        length > 0 && length <= MAX_RECORD as u64,
        "Invalid stable display identity record size"
    );
    let mut bytes = vec![0; length as usize];
    file.read_exact_at(&mut bytes, 0)?;
    let identity: StableIdentity =
        serde_json::from_slice(&bytes).context("Invalid stable display identity record")?;
    ensure!(
        identity.schema == SCHEMA && identity.serial != 0,
        "Invalid stable display identity values"
    );
    Ok(identity.serial)
}

pub(super) fn stable_serial(directory: &File, slot: &str) -> Result<(u32, bool)> {
    let final_name = CString::new(format!("{slot}.json"))?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            final_name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd >= 0 {
        // SAFETY: close this probe; bounded validated reading uses a fresh fd.
        drop(unsafe { File::from_raw_fd(fd) });
        return read_stable_serial(directory, slot).map(|serial| (serial, false));
    }
    ensure!(
        io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT),
        "Could not safely read stable display identity"
    );
    let serial = random_serial()?;
    // One bounded staging filename per slot. A previous killed first write can
    // be safely replaced only after taking the exclusive slot lease and checking
    // this is still a private same-user regular file, never a link.
    let temporary_name = CString::new(format!(".{slot}.pending"))?;
    let temporary = open_at(
        directory,
        temporary_name.to_str()?,
        libc::O_WRONLY | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0o600,
    )?;
    let result = (|| {
        validate_file(&temporary.metadata()?)?;
        temporary.set_len(0)?;
        let bytes = serde_json::to_vec(&StableIdentity {
            schema: SCHEMA,
            serial,
        })?;
        temporary.write_all_at(&bytes, 0)?;
        temporary.sync_all()?;
        // All cooperating creators hold the same slot lease. Publish complete
        // immutable bytes; a crash while writing a temporary file cannot leave
        // a truncated stable identity for the next owner to reinterpret.
        publish_noreplace(directory, &temporary_name, &final_name)?;
        ensure!(
            read_stable_serial(directory, slot)? == serial,
            "Stable display identity changed while publishing"
        );
        Ok((serial, true))
    })();
    // A failed initial creation leaves no partial final record. The private
    // temporary file has no identity role, even if a power loss orphaned it.
    unsafe {
        libc::unlinkat(directory.as_raw_fd(), temporary_name.as_ptr(), 0);
    }
    result
}

/// Publishing can never replace a final immutable identity, even if a path
/// appears unexpectedly while this process owns the cooperating slot lease.
pub(super) fn publish_noreplace(
    directory: &File,
    pending: &CStr,
    destination: &CStr,
) -> Result<()> {
    ensure!(
        unsafe {
            libc::renameatx_np(
                directory.as_raw_fd(),
                pending.as_ptr(),
                directory.as_raw_fd(),
                destination.as_ptr(),
                libc::RENAME_EXCL,
            )
        } == 0,
        "Could not publish stable display identity without replacing an existing file"
    );
    directory.sync_all()?;
    Ok(())
}

pub(super) fn random_serial() -> Result<u32> {
    loop {
        let mut bytes = [0; 4];
        getrandom::getrandom(&mut bytes).map_err(|error| {
            anyhow::anyhow!("Could not generate virtual display identity: {error}")
        })?;
        let serial = u32::from_ne_bytes(bytes);
        if serial != 0 {
            return Ok(serial);
        }
    }
}

pub(super) fn random_generation() -> Result<[u8; 16]> {
    loop {
        let mut generation = [0; 16];
        getrandom::getrandom(&mut generation).map_err(|error| {
            anyhow::anyhow!("Could not generate display ownership generation: {error}")
        })?;
        if generation != [0; 16] {
            return Ok(generation);
        }
    }
}

pub(super) fn identity_directory() -> Result<File> {
    let home = dirs::home_dir().context("Could not locate display identity home directory")?;
    let home = open_directory(&home)?;
    validate_directory(&home, false)?;
    let parent = create_directory_at(&home, ".macrdp")?;
    validate_directory(&parent, false)?;
    let directory = create_directory_at(&parent, "display-identities")?;
    validate_directory(&directory, true)?;
    Ok(directory)
}

pub(super) fn open_directory(path: &Path) -> Result<File> {
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(path.as_os_str().as_bytes())?;
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    ensure!(
        fd >= 0,
        "Could not open display identity home directory safely"
    );
    // SAFETY: open returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(super) fn create_directory_at(parent: &File, name: &str) -> Result<File> {
    let name_c = CString::new(name)?;
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name_c.as_ptr(), 0o700) };
    ensure!(
        result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EEXIST),
        "Could not create private display identity directory"
    );
    open_at(
        parent,
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0,
    )
}

pub(super) fn open_at(parent: &File, name: &str, flags: i32, mode: libc::mode_t) -> Result<File> {
    let name = CString::new(name)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NONBLOCK,
            mode as libc::c_uint,
        )
    };
    ensure!(fd >= 0, "Could not safely open display identity file");
    // SAFETY: openat returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(super) fn validate_directory(file: &File, private: bool) -> Result<()> {
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & (if private { 0o077 } else { 0o022 }) == 0,
        "Display identity directory must be owned by this user with safe permissions"
    );
    Ok(())
}

pub(super) fn validate_file(metadata: &Metadata) -> Result<()> {
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o777 == 0o600
            && metadata.nlink() == 1,
        "Display identity file must be a private, same-user, single-link regular file"
    );
    Ok(())
}

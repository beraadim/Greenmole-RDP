//! A persistent, non-secret flock prevents an orphaned recovery generation from
//! being bypassed by a new server. Never unlink it: unlink permits two owners.

use anyhow::{ensure, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

fn directory() -> PathBuf {
    // /tmp is intentionally fixed rather than an environment-controlled TMPDIR.
    // The uid-scoped child is validated without following symlinks.
    PathBuf::from(format!("/private/tmp/macrdp-private-session-{}", unsafe {
        libc::geteuid()
    }))
}

fn validate_directory(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).context("Could not inspect private-session lease directory")?;
    ensure!(
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o7777 == 0o700,
        "Private-session lease directory must be an owned directory with mode 0700"
    );
    Ok(())
}

fn open_at(path: &Path, create: bool) -> Result<File> {
    validate_directory(path)?;
    // Opening the directory with O_NOFOLLOW closes the validation/open symlink
    // race. The lease is then opened relative to that pinned directory fd.
    use std::os::fd::{AsRawFd, FromRawFd};
    let dir = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = dir.metadata()?;
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o7777 == 0o700,
        "Private-session lease directory ownership changed"
    );
    let flags =
        libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC | if create { libc::O_CREAT } else { 0 };
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            b"guard.lock\0".as_ptr().cast(),
            flags,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error())
            .context("Could not open private-session generation lease");
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o7777 == 0o600
            && metadata.nlink() == 1,
        "Private-session lease must be an owned single-link regular file with mode 0600"
    );
    Ok(file)
}

fn take(file: &File) -> Result<bool> {
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
        return Ok(false);
    }
    Err(error).context("Could not acquire private-session generation lease")
}

pub(super) fn acquire() -> Result<File> {
    acquire_at(&directory())
}

fn acquire_at(path: &Path) -> Result<File> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error).context("Could not create private-session lease directory")
        }
    }
    let file = open_at(path, true)?;
    ensure!(take(&file)?, "A previous private session is still protected or recovering; wait for lock verification before reconnecting");
    Ok(file)
}

pub(crate) fn recovery_pending() -> Result<bool> {
    pending_at(&directory())
}

fn pending_at(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).context("Could not inspect private-session generation lease")
        }
        Ok(_) => {}
    }
    let file = match open_at(path, false) {
        Ok(file) => file,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(false)
        }
        Err(error) => return Err(error),
    };
    Ok(!take(&file)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    fn fixture() -> PathBuf {
        std::env::temp_dir().join(format!(
            "macrdp-privacy-lease-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn lease_rejects_new_generation_until_previous_owner_releases_it() {
        let path = fixture();
        assert!(!pending_at(&path).unwrap());
        assert!(!path.exists(), "probing must not create an idle lease");
        let owner = acquire_at(&path).unwrap();
        assert!(pending_at(&path).unwrap());
        assert!(acquire_at(&path).is_err());
        drop(owner);
        assert!(!pending_at(&path).unwrap());
        drop(acquire_at(&path).unwrap());
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn unsafe_directory_file_modes_and_symlinks_are_rejected() {
        let path = fixture();
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(acquire_at(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        symlink("/dev/null", path.join("guard.lock")).unwrap();
        assert!(acquire_at(&path).is_err());
        fs::remove_file(path.join("guard.lock")).unwrap();
        let file = File::create(path.join("guard.lock")).unwrap();
        file.set_permissions(fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(acquire_at(&path).is_err());
        fs::remove_dir_all(path).unwrap();
    }
}

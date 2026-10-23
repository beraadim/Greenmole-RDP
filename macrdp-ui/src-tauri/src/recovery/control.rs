//! A private same-user control socket, independent of AppKit and WKWebView.
//! The endpoint accepts one bounded command and never receives credentials.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use anyhow::{ensure, Context, Result};

const REQUEST: &[u8] = b"recover\n";
const MAX_REPLY: u64 = 2048;

fn directory() -> Result<PathBuf> {
    Ok(dirs::data_local_dir()
        .context("Local application storage is unavailable")?
        .join("macrdp")
        .join("recovery"))
}

fn private_directory(path: &Path) -> Result<()> {
    // The parent is normal application storage; only this dedicated endpoint
    // directory must be private. Never chmod or follow an existing symlink.
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        use std::os::unix::fs::DirBuilderExt;
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Recovery directory must be a real directory"
    );
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0,
        "Recovery directory must be private and owned by this user"
    );
    Ok(())
}

fn owned_socket(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_socket() && metadata.uid() == unsafe { libc::geteuid() },
                "Unexpected file at the recovery socket path"
            );
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn same_user(stream: &UnixStream) -> bool {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: getpeereid writes two valid stack values for this live socket.
    unsafe {
        libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) == 0 && uid == libc::geteuid()
    }
}

pub(crate) struct Endpoint {
    stopped: Arc<AtomicBool>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Endpoint {
    pub(crate) fn install(
        recover: impl Fn() -> Result<(), String> + Send + 'static,
    ) -> Result<Self> {
        Self::at(directory()?, recover)
    }

    fn at(
        path: PathBuf,
        recover: impl Fn() -> Result<(), String> + Send + 'static,
    ) -> Result<Self> {
        private_directory(&path)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path.join("owner.lock"))?;
        let metadata = lock.metadata()?;
        ensure!(
            metadata.is_file()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o077 == 0,
            "Recovery lock must be a private regular file owned by this user"
        );
        // An existing process holds the inode lock even after socket cleanup;
        // never unlink the lock file or replace another live owner's endpoint.
        ensure!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "Another Greenmole RDP instance owns desktop recovery"
        );
        let socket_path = path.join("control.sock");
        if owned_socket(&socket_path)? {
            fs::remove_file(&socket_path)?;
        }
        let listener = UnixListener::bind(&socket_path)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let stopped = Arc::new(AtomicBool::new(false));
        let done = stopped.clone();
        let worker = std::thread::Builder::new()
            .name("local-recovery-control".into())
            .spawn(move || {
                let _lock: File = lock;
                while !done.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            if !same_user(&stream) {
                                continue;
                            }
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                            let mut line = Vec::new();
                            let accepted = BufReader::new((&stream).take(32))
                                .read_until(b'\n', &mut line)
                                .is_ok()
                                && line == REQUEST;
                            let reply = if accepted {
                                recover()
                            } else {
                                Err("Unknown recovery request".into())
                            };
                            let reply = match reply {
                                Ok(()) => "OK\n".into(),
                                Err(error) => {
                                    // Keep the framing single-line and bounded, including unexpected OS error text.
                                    let message: String = error
                                        .chars()
                                        .filter(|c| !c.is_control())
                                        .take(500)
                                        .collect();
                                    format!("ERROR {message}\n")
                                }
                            };
                            let _ = stream.write_all(reply.as_bytes());
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(25))
                        }
                        Err(error) => {
                            tracing::warn!(%error, "Local recovery control listener failed");
                            break;
                        }
                    }
                }
                let _ = fs::remove_file(&socket_path);
            })?;
        Ok(Self {
            stopped,
            worker: Mutex::new(Some(worker)),
        })
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        // A request can be waiting for bounded worker recovery. Do not block
        // application shutdown on it; process exit releases the endpoint lock.
        if let Ok(worker) = self.worker.get_mut() {
            if worker.as_ref().is_some_and(|worker| worker.is_finished()) {
                let _ = worker.take().unwrap().join();
            }
        }
    }
}

pub(crate) fn request_recovery() -> Result<(), String> {
    request_at(&directory().map_err(|e| e.to_string())?).map_err(|e| format!("{e:#}"))
}

fn request_at(path: &Path) -> Result<()> {
    private_directory(path)?;
    let socket = path.join("control.sock");
    ensure!(
        owned_socket(&socket)?,
        "Greenmole RDP is not running, or its recovery control is unavailable"
    );
    let mut stream =
        UnixStream::connect(socket).context("Could not contact the running Greenmole RDP application")?;
    ensure!(
        same_user(&stream),
        "Recovery endpoint belongs to another user"
    );
    stream.set_read_timeout(Some(Duration::from_secs(90)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(REQUEST)?;
    let mut reply = String::new();
    BufReader::new(stream.take(MAX_REPLY)).read_line(&mut reply)?;
    match reply.strip_suffix('\n') {
        Some("OK") => Ok(()),
        Some(error) if error.starts_with("ERROR ") => anyhow::bail!("{}", &error[6..]),
        _ => anyhow::bail!("The recovery endpoint did not return a valid result"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn real_socket_recovers_without_a_window_and_rejects_competing_owner() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("recovery");
        let called = Arc::new(AtomicUsize::new(0));
        let counter = called.clone();
        let endpoint = Endpoint::at(path.clone(), move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
        assert!(Endpoint::at(path.clone(), || panic!("must not replace active owner")).is_err());
        request_at(&path).unwrap();
        assert_eq!(called.load(Ordering::SeqCst), 1);
        assert_eq!(
            fs::metadata(path.join("control.sock")).unwrap().mode() & 0o777,
            0o600
        );
        drop(endpoint);
    }

    #[test]
    fn insecure_or_symlinked_directory_is_rejected_without_changing_permissions() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("recovery");
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Endpoint::at(path.clone(), || Ok(())).is_err());
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o755);
        let link = temporary.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(Endpoint::at(link, || Ok(())).is_err());
    }

    #[test]
    fn malformed_and_oversized_requests_do_not_trigger_recovery() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("recovery");
        let endpoint = Endpoint::at(path.clone(), || {
            panic!("malformed request reached recovery")
        })
        .unwrap();
        for request in [b"restart\n".as_slice(), &[b'x'; 128]] {
            let mut stream = UnixStream::connect(path.join("control.sock")).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream.write_all(request).unwrap();
            let mut reply = String::new();
            let _ = BufReader::new(stream).read_line(&mut reply);
            assert!(reply.starts_with("ERROR "));
        }
        drop(endpoint);
    }
}

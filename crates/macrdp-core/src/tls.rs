//! TLS identity selection. Custom identities are never generated or replaced.
//! Managed renewal preserves old pairs and atomically publishes a complete generation.

mod certificate;
mod names;
#[cfg(test)]
mod tests;

pub use certificate::{inspect_tls_certificate, TlsCertificateInfo};
pub use names::validate_server_names;

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;

const MANIFEST: &str = "current.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedIdentity {
    generation: String,
}

/// Resolve and validate the identity before starting the server.
/// Custom paths must be a complete existing pair and currently valid. Managed
/// renewal preserves the prior pair; changing the fingerprint is logged.
pub fn ensure_tls_files(
    cert_path: Option<&Path>,
    key_path: Option<&Path>,
    server_names: &[String],
) -> Result<(PathBuf, PathBuf)> {
    validate_server_names(server_names)?;
    let names = if cert_path.is_none() && key_path.is_none() {
        names::managed_server_names(server_names)?
    } else {
        names::normalize_names(server_names)?
    };
    ensure_tls_files_in(
        &crate::config::config_dir().join("tls"),
        cert_path,
        key_path,
        &names,
        OffsetDateTime::now_utc().unix_timestamp(),
    )
}

/// Read-only status: does not create storage, generate, renew, or modify permissions.
pub fn inspect_configured_tls_certificate(
    cert_path: Option<&Path>,
) -> Result<Option<TlsCertificateInfo>> {
    if let Some(cert) = cert_path {
        return inspect_tls_certificate(cert).map(Some);
    }
    inspect_managed_certificate(&crate::config::config_dir().join("tls"))
}

fn inspect_managed_certificate(directory: &Path) -> Result<Option<TlsCertificateInfo>> {
    let (cert, key) = managed_paths(directory)?;
    if !cert.try_exists()? && !directory.join(MANIFEST).try_exists()? {
        ensure!(
            !key.try_exists()?,
            "Managed TLS certificate is missing while its key exists"
        );
        return Ok(None);
    }
    inspect_tls_certificate(&cert).map(Some)
}

fn managed_paths(directory: &Path) -> Result<(PathBuf, PathBuf)> {
    let manifest = directory.join(MANIFEST);
    if !manifest.try_exists()? {
        return Ok((directory.join("cert.pem"), directory.join("key.pem")));
    }
    let metadata = fs::symlink_metadata(&manifest)?;
    ensure!(
        metadata.file_type().is_file() && metadata.len() <= 1024,
        "Invalid managed TLS identity manifest"
    );
    let current: ManagedIdentity = serde_json::from_slice(&fs::read(&manifest)?)
        .context("Invalid managed TLS identity manifest")?;
    ensure!(
        current.generation.starts_with("identity-")
            && current.generation.len() <= 128
            && current
                .generation
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-'),
        "Invalid managed TLS identity generation"
    );
    let generation = directory.join(current.generation);
    ensure!(
        fs::symlink_metadata(&generation)?.file_type().is_dir(),
        "Managed TLS identity generation must be a directory"
    );
    Ok((generation.join("cert.pem"), generation.join("key.pem")))
}

fn ensure_tls_files_in(
    directory: &Path,
    cert_path: Option<&Path>,
    key_path: Option<&Path>,
    names: &[String],
    now: i64,
) -> Result<(PathBuf, PathBuf)> {
    match (cert_path, key_path) {
        (Some(cert), Some(key)) => {
            let info = validate_pair(cert, key)?;
            ensure!(
                now >= info.not_before_unix,
                "Custom TLS certificate is not valid yet"
            );
            ensure!(
                now < info.not_after_unix,
                "Custom TLS certificate expired; replace the configured certificate and key"
            );
            for name in names {
                ensure!(
                    certificate::covers_server_name(&info, name),
                    "Custom TLS certificate does not cover configured server name {name}"
                );
            }
            log_identity(&info, false);
            return Ok((cert.to_owned(), key.to_owned()));
        }
        (None, None) => {}
        _ => {
            anyhow::bail!("cert_path and key_path must both be supplied for a custom TLS identity")
        }
    }

    private_directory(directory)?;
    let (cert, key) = managed_paths(directory)?;
    match (cert.try_exists()?, key.try_exists()?) {
        (false, false) if !directory.join(MANIFEST).try_exists()? => return publish_identity(directory, names, now),
        (true, true) => {}
        _ => anyhow::bail!("Managed TLS identity is incomplete; restore its missing certificate/key instead of replacing the surviving file"),
    }
    let info = validate_pair(&cert, &key)?;
    ensure!(
        now >= info.not_before_unix,
        "Managed TLS certificate is not valid yet; check the system clock"
    );
    let missing_names = names.iter().any(|name| !info.server_names.contains(name));
    if now >= info.not_after_unix || missing_names {
        let renewed = publish_identity(directory, names, now)?;
        tracing::warn!(old_fingerprint = %info.fingerprint_sha256, expired = now >= info.not_after_unix,
            names_changed = missing_names, previous_certificate = ?cert,
            "Renewed managed TLS identity; prior pair retained, clients may need to trust the new fingerprint");
        return Ok(renewed);
    }
    log_identity(&info, true);
    Ok((cert, key))
}

fn validate_pair(cert: &Path, key: &Path) -> Result<TlsCertificateInfo> {
    let info = inspect_tls_certificate(cert)?;
    let metadata = fs::symlink_metadata(key).context("TLS private key is missing")?;
    ensure!(
        metadata.file_type().is_file() && metadata.len() <= 1024 * 1024,
        "TLS private key must be a regular file under 1 MiB"
    );
    crate::credentials::protect_config_file(key)?;
    // Rustls checks that the key matches the leaf certificate before publication.
    ironrdp_server::TlsIdentityCtx::init_from_paths(cert, key)?
        .make_acceptor()
        .context("TLS certificate and private key are incompatible")?;
    Ok(info)
}

fn private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_dir(),
        "Managed TLS storage must be a directory, not a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn publish_identity(directory: &Path, names: &[String], now: i64) -> Result<(PathBuf, PathBuf)> {
    let generation = tempfile::Builder::new()
        .prefix("identity-")
        .tempdir_in(directory)?;
    private_directory(generation.path())?;
    let cert = generation.path().join("cert.pem");
    let key = generation.path().join("key.pem");
    certificate::write_generated_pair(&cert, &key, names, now)?;
    let info = validate_pair(&cert, &key)?;
    let current = ManagedIdentity {
        generation: generation
            .path()
            .file_name()
            .context("Missing TLS generation name")?
            .to_str()
            .context("Invalid TLS generation name")?
            .to_owned(),
    };
    // Sync the pair's directory before the manifest points to it. A crash exposes
    // either the old complete pair or the new one, never a mixed certificate/key.
    #[cfg(unix)]
    fs::File::open(generation.path())?.sync_all()?;
    crate::credentials::write_private_file(
        &directory.join(MANIFEST),
        &serde_json::to_vec(&current)?,
    )?;
    // Once published, never allow a later error to remove this generation.
    let _ = generation.keep();
    #[cfg(unix)]
    fs::File::open(directory)?.sync_all()?;
    log_identity(&info, true);
    Ok((cert, key))
}

fn log_identity(info: &TlsCertificateInfo, managed: bool) {
    tracing::info!(managed, fingerprint_sha256 = %info.fingerprint_sha256,
        expires_at = %info.expires_at, server_names = ?info.server_names, "TLS identity ready");
}

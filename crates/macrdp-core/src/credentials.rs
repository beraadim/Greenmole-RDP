//! Credential generation and persistence. Secret values never belong in diagnostics.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// A uniformly distributed 128-bit password, encoded without punctuation for RDP clients.
pub fn generate_password() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| anyhow::anyhow!("OS random source failed: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Tighten an existing explicitly configured credential file before reading it.
pub fn protect_config_file(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.file_type().is_file(),
        "Credential file must be a regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Replace a private file atomically; no intermediate file contains world-readable secrets.
pub fn write_private_file(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    temporary.write_all(content)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .context("Failed to replace private configuration file")?;
    Ok(())
}

const KEYCHAIN_SERVICE: &str = "com.macrdp.server.credentials";
const KEYCHAIN_ACCOUNT: &str = "desktop-server";

/// Read the desktop credential from the current user's login Keychain.
pub fn load_desktop_password() -> Result<Option<String>> {
    #[cfg(target_os = "macos")]
    {
        match security_framework::passwords::get_generic_password(
            KEYCHAIN_SERVICE,
            KEYCHAIN_ACCOUNT,
        ) {
            Ok(bytes) => Ok(Some(
                String::from_utf8(bytes).context("Keychain password is not UTF-8")?,
            )),
            // errSecItemNotFound is the only error that permits first-run initialization.
            Err(error) if error.code() == -25300 => Ok(None),
            Err(error) => Err(error).context("Unable to read macrdp password from Keychain"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT);
        anyhow::bail!("Desktop credential storage requires macOS Keychain")
    }
}

/// Store before removing any legacy plaintext copy; storage failure leaves migration retryable.
pub fn save_desktop_password(password: &str) -> Result<()> {
    anyhow::ensure!(!password.is_empty(), "A nonempty RDP password is required");
    #[cfg(target_os = "macos")]
    {
        security_framework::passwords::set_generic_password(
            KEYCHAIN_SERVICE,
            KEYCHAIN_ACCOUNT,
            password.as_bytes(),
        )
        .context("Unable to store macrdp password in Keychain")
    }
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("Desktop credential storage requires macOS Keychain")
}

/// Undo a first-time credential write when the matching preferences cannot be committed.
pub fn delete_desktop_password() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        match security_framework::passwords::delete_generic_password(
            KEYCHAIN_SERVICE,
            KEYCHAIN_ACCOUNT,
        ) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == -25300 => Ok(()),
            Err(error) => Err(error).context("Unable to remove macrdp password from Keychain"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("Desktop credential storage requires macOS Keychain")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_password_has_full_entropy_encoding() {
        let password = generate_password().unwrap();
        assert_eq!(password.len(), 32);
        assert!(password.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(password, generate_password().unwrap());
    }

    #[test]
    fn private_replacement_retains_complete_contents_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_private_file(&path, b"old").unwrap();
        write_private_file(&path, b"replacement").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn credential_read_rejects_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        std::fs::write(&target, b"private").unwrap();
        std::os::unix::fs::symlink(target, &link).unwrap();
        assert!(protect_config_file(&link).is_err());
    }
}

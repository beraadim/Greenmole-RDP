use super::*;

const NOW: i64 = 1_767_225_600; // 2026-01-01 UTC, independent of the test machine clock.
const YEAR: i64 = 365 * 24 * 60 * 60;

fn names() -> Vec<String> {
    names::names_with_hostname(&["rdp.example.test".into(), "192.0.2.7".into()], "test-mac")
        .unwrap()
}

fn legacy_pair(directory: &Path, timestamp: i64) -> (PathBuf, PathBuf) {
    let cert = directory.join("cert.pem");
    let key = directory.join("key.pem");
    certificate::write_generated_pair(&cert, &key, &names(), timestamp).unwrap();
    (cert, key)
}

#[test]
fn generated_identity_has_hostname_dns_ip_sans_expiry_and_stable_fingerprint() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = ensure_tls_files_in(dir.path(), None, None, &names(), NOW).unwrap();
    let info = inspect_tls_certificate(&cert).unwrap();
    assert_eq!(info.server_names, names());
    assert_eq!(info.not_before_unix, NOW - 300);
    assert_eq!(info.not_after_unix, NOW + YEAR);
    assert_eq!(info.fingerprint_sha256.len(), 95);
    assert!(info.expires_at.ends_with('Z'));
    assert_eq!(
        ensure_tls_files_in(dir.path(), None, None, &names(), NOW + 1).unwrap(),
        (cert.clone(), key.clone())
    );
    assert_eq!(
        inspect_tls_certificate(&cert).unwrap().fingerprint_sha256,
        info.fingerprint_sha256
    );
    assert_eq!(
        inspect_managed_certificate(dir.path())
            .unwrap()
            .unwrap()
            .fingerprint_sha256,
        info.fingerprint_sha256
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(key).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(cert.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}

#[test]
fn expired_managed_identity_is_renewed_without_destroying_old_pair() {
    let dir = tempfile::tempdir().unwrap();
    let (old_cert, old_key) = legacy_pair(dir.path(), NOW - YEAR);
    let original_cert = fs::read(&old_cert).unwrap();
    let original_key = fs::read(&old_key).unwrap();
    let renewed = ensure_tls_files_in(dir.path(), None, None, &names(), NOW).unwrap();
    assert_ne!(renewed.0, old_cert);
    assert_eq!(fs::read(old_cert).unwrap(), original_cert);
    assert_eq!(fs::read(old_key).unwrap(), original_key);
    assert_eq!(managed_paths(dir.path()).unwrap(), renewed);
    assert!(inspect_tls_certificate(&renewed.0).unwrap().not_after_unix > NOW);
    assert_eq!(
        ensure_tls_files_in(dir.path(), None, None, &names(), NOW + 1).unwrap(),
        renewed
    );
}

#[test]
fn missing_requested_sans_renew_managed_identity_once_and_retain_prior_generation() {
    let dir = tempfile::tempdir().unwrap();
    let first = ensure_tls_files_in(dir.path(), None, None, &names(), NOW).unwrap();
    let mut updated = names();
    updated.push("new-name.example.test".into());
    let second = ensure_tls_files_in(dir.path(), None, None, &updated, NOW + 1).unwrap();
    assert_ne!(first.0, second.0);
    assert!(first.0.is_file() && first.1.is_file());
    assert!(inspect_tls_certificate(&second.0)
        .unwrap()
        .server_names
        .contains(&updated.last().unwrap().clone()));
    assert_eq!(
        ensure_tls_files_in(dir.path(), None, None, &updated, NOW + 2).unwrap(),
        second
    );
}

#[test]
fn explicit_missing_expired_or_incomplete_pairs_are_never_generated_or_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("custom.pem");
    let key = dir.path().join("custom-key.pem");
    assert!(ensure_tls_files_in(dir.path(), Some(&cert), Some(&key), &[], NOW).is_err());
    assert!(ensure_tls_files_in(dir.path(), Some(&cert), None, &[], NOW).is_err());
    assert!(!cert.exists() && !key.exists());
    certificate::write_generated_pair(&cert, &key, &names(), NOW - YEAR).unwrap();
    let before = (fs::read(&cert).unwrap(), fs::read(&key).unwrap());
    assert!(ensure_tls_files_in(dir.path(), Some(&cert), Some(&key), &[], NOW).is_err());
    assert_eq!(before, (fs::read(&cert).unwrap(), fs::read(&key).unwrap()));
    assert!(!dir.path().join(MANIFEST).exists());
}

#[test]
fn incomplete_managed_pair_and_mismatched_key_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = legacy_pair(dir.path(), NOW);
    let original = fs::read(&cert).unwrap();
    fs::remove_file(&key).unwrap();
    assert!(ensure_tls_files_in(dir.path(), None, None, &names(), NOW).is_err());
    assert_eq!(fs::read(&cert).unwrap(), original);
    assert!(!key.exists());
    let other = tempfile::tempdir().unwrap();
    let (_, other_key) = legacy_pair(other.path(), NOW);
    fs::copy(other_key, &key).unwrap();
    assert!(ensure_tls_files_in(dir.path(), None, None, &names(), NOW).is_err());
    assert!(!dir.path().join(MANIFEST).exists());
}

#[test]
fn custom_names_are_checked_only_when_requested_and_wildcards_match_one_label() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = legacy_pair(dir.path(), NOW);
    assert!(ensure_tls_files_in(dir.path(), Some(&cert), Some(&key), &[], NOW).is_ok());
    assert!(ensure_tls_files_in(
        dir.path(),
        Some(&cert),
        Some(&key),
        &["wrong.example.test".into()],
        NOW
    )
    .is_err());
    assert!(ensure_tls_files_in(dir.path(), Some(&cert), Some(&key), &[], NOW - 301).is_err());
    let mut info = inspect_tls_certificate(&cert).unwrap();
    info.server_names = vec!["*.example.test".into()];
    assert!(certificate::covers_server_name(&info, "rdp.example.test"));
    assert!(!certificate::covers_server_name(&info, "example.test"));
    assert!(!certificate::covers_server_name(
        &info,
        "nested.rdp.example.test"
    ));
    info.server_names = vec!["*.0.2.7".into()];
    assert!(!certificate::covers_server_name(&info, "192.0.2.7"));
}

#[test]
fn failed_manifest_publication_cleans_unpublished_pair_and_preserves_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = legacy_pair(dir.path(), NOW);
    let before = (fs::read(&cert).unwrap(), fs::read(&key).unwrap());
    fs::create_dir(dir.path().join(MANIFEST)).unwrap(); // Deterministically reject atomic file replacement.
    assert!(publish_identity(dir.path(), &names(), NOW).is_err());
    assert_eq!(before, (fs::read(cert).unwrap(), fs::read(key).unwrap()));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);
}

#[test]
fn read_only_status_does_not_initialize_storage_and_manifest_cannot_escape_directory() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("not-created");
    assert!(inspect_managed_certificate(&absent).unwrap().is_none());
    assert!(!absent.exists());
    for generation in [
        "../outside",
        "identity-../../outside",
        "/tmp/identity-other",
    ] {
        fs::write(
            dir.path().join(MANIFEST),
            serde_json::to_vec(&ManagedIdentity {
                generation: generation.into(),
            })
            .unwrap(),
        )
        .unwrap();
        assert!(managed_paths(dir.path()).is_err());
    }
}

#[test]
fn tls_names_have_bounded_valid_syntax_and_legacy_configs_default_to_empty() {
    let parsed: crate::config::ServerConfig = toml::from_str("port = 3389").unwrap();
    assert!(parsed.tls_server_names.is_empty());
    assert!(validate_server_names(&vec!["example.test".into(); 33]).is_err());
    for invalid in [
        "",
        "rdp host",
        "*.example.test",
        "https://example.test",
        "example.test:3389",
        "-host",
        "host-",
        "a..b",
        "é.test",
    ] {
        assert!(
            validate_server_names(&[invalid.into()]).is_err(),
            "{invalid}"
        );
    }
    let names = names::names_with_hostname(
        &["RDP.EXAMPLE.TEST.".into(), "2001:db8::7".into()],
        "Test-Mac",
    )
    .unwrap();
    for expected in [
        "test-mac",
        "test-mac.local",
        "rdp.example.test",
        "2001:db8::7",
        "::1",
    ] {
        assert!(names.contains(&expected.to_owned()));
    }
}

use anyhow::{ensure, Context, Result};
use rcgen::{CertificateParams, DistinguishedName, KeyPair};
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;
use time::{Duration, OffsetDateTime};
use x509_cert::der::{DecodePem, Encode};
use x509_cert::ext::pkix::{name::GeneralName, SubjectAltName};

const CERT_VALIDITY_DAYS: i64 = 365;

/// Public identity information only; no private key or credential data.
#[derive(Clone, Debug, serde::Serialize)]
pub struct TlsCertificateInfo {
    pub fingerprint_sha256: String,
    pub expires_at: String,
    pub not_before_unix: i64,
    pub not_after_unix: i64,
    pub server_names: Vec<String>,
}

/// Inspect the leaf PEM certificate without generating or changing an identity.
pub fn inspect_tls_certificate(path: &Path) -> Result<TlsCertificateInfo> {
    let metadata = std::fs::symlink_metadata(path).context("TLS certificate is missing")?;
    ensure!(
        metadata.file_type().is_file() && metadata.len() <= 1024 * 1024,
        "TLS certificate must be a regular PEM file under 1 MiB"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1024 * 1024, "TLS certificate exceeds 1 MiB");
    let end_marker = b"-----END CERTIFICATE-----";
    let end = bytes
        .windows(end_marker.len())
        .position(|part| part == end_marker)
        .context("TLS certificate must contain a PEM certificate")?
        + end_marker.len();
    let cert =
        x509_cert::Certificate::from_pem(&bytes[..end]).context("Invalid TLS certificate")?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &cert.to_der()?);
    let fingerprint_sha256 = digest
        .as_ref()
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    let validity = cert.tbs_certificate.validity;
    let mut server_names = Vec::new();
    if let Some((_, san)) = cert.tbs_certificate.get::<SubjectAltName>()? {
        for name in san.0 {
            match name {
                GeneralName::DnsName(name) => server_names.push(name.as_str().to_ascii_lowercase()),
                GeneralName::IpAddress(bytes) => match bytes.as_bytes() {
                    [a, b, c, d] => server_names.push(Ipv4Addr::new(*a, *b, *c, *d).to_string()),
                    raw if raw.len() == 16 => {
                        let octets: [u8; 16] = raw.try_into()?;
                        server_names.push(Ipv6Addr::from(octets).to_string());
                    }
                    _ => anyhow::bail!("Invalid IP address in TLS certificate SAN"),
                },
                _ => {}
            }
        }
    }
    server_names.sort();
    server_names.dedup();
    Ok(TlsCertificateInfo {
        fingerprint_sha256,
        expires_at: validity.not_after.to_string(),
        not_before_unix: validity
            .not_before
            .to_unix_duration()
            .as_secs()
            .try_into()?,
        not_after_unix: validity.not_after.to_unix_duration().as_secs().try_into()?,
        server_names,
    })
}

pub(super) fn covers_server_name(info: &TlsCertificateInfo, name: &str) -> bool {
    info.server_names.iter().any(|san| {
        san == name
            || (name.parse::<IpAddr>().is_err()
                && san.strip_prefix("*.").is_some_and(|suffix| {
                    name.split_once('.').is_some_and(|(_, rest)| rest == suffix)
                }))
    })
}

pub(super) fn write_generated_pair(
    cert_path: &Path,
    key_path: &Path,
    names: &[String],
    now: i64,
) -> Result<()> {
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, "macrdp");
    dn.push(rcgen::DnType::OrganizationName, "macrdp");
    params.distinguished_name = dn;
    let now = OffsetDateTime::from_unix_timestamp(now)?;
    params.not_before = now - Duration::minutes(5);
    params.not_after = now + Duration::days(CERT_VALIDITY_DAYS);
    for name in names {
        params.subject_alt_names.push(match name.parse::<IpAddr>() {
            Ok(ip) => rcgen::SanType::IpAddress(ip),
            Err(_) => rcgen::SanType::DnsName(name.clone().try_into()?),
        });
    }
    let key_pair = KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;
    crate::credentials::write_private_file(key_path, key_pair.serialize_pem().as_bytes())?;
    crate::credentials::write_private_file(cert_path, cert.pem().as_bytes())?;
    Ok(())
}

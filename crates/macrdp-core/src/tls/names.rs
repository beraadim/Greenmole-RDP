use anyhow::{ensure, Context, Result};
use std::net::IpAddr;

pub fn validate_server_names(names: &[String]) -> Result<()> {
    ensure!(
        names.len() <= 32,
        "At most 32 additional TLS server names are allowed"
    );
    for name in names {
        normalize_name(name)?;
    }
    Ok(())
}

fn normalize_name(name: &str) -> Result<String> {
    if let Ok(ip) = name.parse::<IpAddr>() {
        return Ok(ip.to_string());
    }
    let dns = name.strip_suffix('.').unwrap_or(name);
    ensure!(
        !dns.is_empty() && dns.len() <= 253 && dns.is_ascii(),
        "Invalid TLS DNS name"
    );
    for label in dns.split('.') {
        ensure!(
            !label.is_empty()
                && label.len() <= 63
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-'),
            "TLS names must be DNS hostnames or IP addresses, without wildcards or ports"
        );
    }
    Ok(dns.to_ascii_lowercase())
}

pub(super) fn normalize_names(names: &[String]) -> Result<Vec<String>> {
    names.iter().map(|name| normalize_name(name)).collect()
}

pub(super) fn managed_server_names(extra: &[String]) -> Result<Vec<String>> {
    names_with_hostname(extra, &system_hostname()?)
}

pub(super) fn names_with_hostname(extra: &[String], hostname: &str) -> Result<Vec<String>> {
    validate_server_names(extra)?;
    let mut names = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
    let hostname = normalize_name(hostname).context("System hostname is not a valid TLS name")?;
    names.push(hostname.clone());
    if !hostname.contains('.') && hostname.parse::<IpAddr>().is_err() {
        names.push(format!("{hostname}.local"));
    }
    for name in extra {
        names.push(normalize_name(name)?);
    }
    names.sort();
    names.dedup();
    Ok(names)
}

fn system_hostname() -> Result<String> {
    #[cfg(unix)]
    {
        let mut bytes = [0u8; 256];
        // SAFETY: the writable buffer is valid for exactly the passed size.
        let result = unsafe { libc::gethostname(bytes.as_mut_ptr().cast(), bytes.len()) };
        ensure!(
            result == 0,
            "Unable to read system hostname: {}",
            std::io::Error::last_os_error()
        );
        let end = bytes
            .iter()
            .position(|b| *b == 0)
            .context("System hostname exceeds limit")?;
        Ok(std::str::from_utf8(&bytes[..end])?.to_owned())
    }
    #[cfg(not(unix))]
    {
        std::env::var("COMPUTERNAME").context("Unable to read system hostname")
    }
}

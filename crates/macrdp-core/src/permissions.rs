//! macOS permission checking and requesting

use crate::callbacks::PermissionStatus;

/// Check current macOS permission status (non-blocking, no prompts)
pub fn check_permissions() -> PermissionStatus {
    PermissionStatus {
        screen_capture: macrdp_capture::check_screen_recording_permission(),
        accessibility: macrdp_input::check_accessibility_permission(),
        microphone: false, // Phase 3
    }
}

/// Startup checks must never prompt after a remote display has become primary.
/// The desktop UI owns explicit setup while the service is fully stopped.
pub fn ensure_required_permissions() -> anyhow::Result<PermissionStatus> {
    validate_required_permissions(check_permissions())
}

fn validate_required_permissions(status: PermissionStatus) -> anyhow::Result<PermissionStatus> {
    let mut missing = Vec::new();
    if !status.screen_capture {
        missing.push("Screen Recording");
    }
    if !status.accessibility {
        missing.push("Accessibility");
    }
    anyhow::ensure!(missing.is_empty(),
        "macOS permissions required: {}. Complete permission setup on the physical display before starting the server.",
        missing.join(" and "));
    Ok(status)
}

/// Request all required macOS permissions (may trigger system dialogs).
/// Does NOT auto-open System Settings — the UI should handle that based on returned status.
pub fn request_permissions() -> PermissionStatus {
    tracing::info!("Checking macOS permissions...");

    // Screen Recording
    if macrdp_capture::check_screen_recording_permission() {
        tracing::info!("[OK] Screen Recording permission granted");
    } else {
        tracing::warn!("[!!] Screen Recording permission NOT granted");
        macrdp_capture::request_screen_recording_permission();
        if !macrdp_capture::check_screen_recording_permission() {
            tracing::warn!(
                "Screen Recording denied. Go to: System Settings > Privacy & Security > Screen Recording"
            );
        }
    }

    // Accessibility
    if macrdp_input::check_accessibility_permission() {
        tracing::info!("[OK] Accessibility permission granted");
    } else {
        tracing::warn!("[!!] Accessibility permission NOT granted");
        macrdp_input::request_accessibility_permission();
        if !macrdp_input::check_accessibility_permission() {
            tracing::warn!(
                "Accessibility denied. Go to: System Settings > Privacy & Security > Accessibility"
            );
        }
    }

    check_permissions()
}

/// Detect the main display's logical pixel size
pub fn detect_display_size() -> anyhow::Result<(u32, u32)> {
    macrdp_capture::detect_display_size()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_rejects_each_missing_required_permission_without_native_requests() {
        for (screen_capture, accessibility, expected) in [
            (false, true, "Screen Recording"),
            (true, false, "Accessibility"),
            (false, false, "Screen Recording and Accessibility"),
        ] {
            let error = validate_required_permissions(PermissionStatus {
                screen_capture,
                accessibility,
                microphone: false,
            })
            .unwrap_err()
            .to_string();
            assert!(error.contains(expected));
            assert!(error.contains("physical display"));
        }
        assert!(validate_required_permissions(PermissionStatus {
            screen_capture: true,
            accessibility: true,
            microphone: false,
        })
        .is_ok());
    }
}

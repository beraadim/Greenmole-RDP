//! macOS permission checking and requesting

use crate::callbacks::PermissionStatus;

/// Check current macOS permission status (non-blocking, no prompts)
pub fn check_permissions() -> PermissionStatus {
    PermissionStatus {
        screen_capture: macrdp_capture::check_screen_recording_permission(),
        accessibility: macrdp_input::check_accessibility_permission(),
        input_monitoring: macrdp_input::check_input_monitoring_permission(),
        microphone: false, // Phase 3
    }
}

/// Startup checks must never prompt after a remote display has become primary.
/// The desktop UI owns explicit setup while the service is fully stopped.
pub fn ensure_required_permissions() -> anyhow::Result<PermissionStatus> {
    ensure_session_permissions(false)
}

/// Preflight configured policy without prompting. Ordinary mode does not need
/// Input Monitoring; a guarded session must also pass its native tap checks.
pub fn ensure_session_permissions(private_session: bool) -> anyhow::Result<PermissionStatus> {
    validate_required_permissions(check_permissions(), private_session)
}

fn validate_required_permissions(
    status: PermissionStatus,
    private_session: bool,
) -> anyhow::Result<PermissionStatus> {
    let mut missing = Vec::new();
    if !status.screen_capture {
        missing.push("Screen Recording");
    }
    if !status.accessibility {
        missing.push("Accessibility");
    }
    if private_session && !status.input_monitoring {
        missing.push("Input Monitoring");
    }
    anyhow::ensure!(missing.is_empty(),
        "macOS permissions required: {}. Complete permission setup on the physical display before starting the server.",
        missing.join(" and "));
    Ok(status)
}

/// Request all required macOS permissions (may trigger system dialogs).
/// Does NOT auto-open System Settings — the UI should handle that based on returned status.
pub fn request_permissions() -> PermissionStatus {
    request_session_permissions(false)
}

/// Called only by the explicit, stopped-service setup flow. Never request
/// Input Monitoring merely because ordinary remote input is being configured.
pub fn request_session_permissions(private_session: bool) -> PermissionStatus {
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

    if private_session && !macrdp_input::check_input_monitoring_permission() {
        tracing::warn!("Input Monitoring permission is required for guarded private sessions");
        macrdp_input::request_input_monitoring_permission();
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
            let error = validate_required_permissions(
                PermissionStatus {
                    screen_capture,
                    accessibility,
                    input_monitoring: false,
                    microphone: false,
                },
                false,
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains(expected));
            assert!(error.contains("physical display"));
        }
        assert!(validate_required_permissions(
            PermissionStatus {
                screen_capture: true,
                accessibility: true,
                input_monitoring: false,
                microphone: false,
            },
            false
        )
        .is_ok());
    }

    #[test]
    fn input_monitoring_is_required_only_for_configured_private_sessions() {
        for private_session in [false, true] {
            for input_monitoring in [false, true] {
                let result = validate_required_permissions(
                    PermissionStatus {
                        screen_capture: true,
                        accessibility: true,
                        input_monitoring,
                        microphone: false,
                    },
                    private_session,
                );
                assert_eq!(result.is_ok(), !private_session || input_monitoring);
                if let Err(error) = result {
                    assert!(error.to_string().contains("Input Monitoring"));
                }
            }
        }
    }

    #[test]
    fn legacy_permission_snapshots_default_input_monitoring_to_denied() {
        let status: PermissionStatus = serde_json::from_value(serde_json::json!({
            "screen_capture": true, "accessibility": true, "microphone": false,
        }))
        .unwrap();
        assert!(!status.input_monitoring);
        assert!(validate_required_permissions(status.clone(), false).is_ok());
        assert!(validate_required_permissions(status.clone(), true).is_err());
        assert_eq!(
            serde_json::to_value(status).unwrap()["input_monitoring"],
            false
        );
    }
}

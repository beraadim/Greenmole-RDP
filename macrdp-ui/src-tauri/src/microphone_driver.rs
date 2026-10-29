//! Explicit local installation of the virtual input device. Merely inspecting
//! settings never requests administrator authentication or restarts Core Audio.
use crate::state::AppState;
use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::State;

#[derive(Serialize)]
pub struct MicrophoneDriverStatus {
    installed: bool,
    ready: bool,
    installation_available: bool,
    removal_available: bool,
    message: String,
}

#[tauri::command]
pub async fn get_microphone_driver_status(
    app: tauri::AppHandle,
) -> Result<MicrophoneDriverStatus, String> {
    driver_status(app).await
}

#[tauri::command]
pub async fn install_microphone_driver(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<MicrophoneDriverStatus, String> {
    change_driver(app, state.inner(), true).await
}

#[tauri::command]
pub async fn remove_microphone_driver(
    app: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<MicrophoneDriverStatus, String> {
    change_driver(app, state.inner(), false).await
}

async fn change_driver(
    app: tauri::AppHandle,
    state: &Arc<AppState>,
    install: bool,
) -> Result<MicrophoneDriverStatus, String> {
    // Keep service startup serialized with the entire explicit action, including
    // the native authentication dialog. Repeated button clicks cannot queue prompts.
    let _lifecycle = state.lifecycle_lock.try_lock().map_err(|_| {
        "Another service or driver operation is pending. Wait for it to finish.".to_string()
    })?;
    if macrdp_core::server_resources_pending()
        || state
            .server_handle
            .lock()
            .await
            .as_ref()
            .is_some_and(|handle| !handle.cleanup_complete())
    {
        return Err(
            "Stop the RDP service and wait for cleanup before changing its microphone driver."
                .into(),
        );
    }
    run_driver_action(&app, install).await?;
    driver_status(app).await
}

async fn driver_status(app: tauri::AppHandle) -> Result<MicrophoneDriverStatus, String> {
    let query = DriverQuery::acquire()?;
    // A blocked HAL query cannot strand the settings page or accumulate detached
    // threads on refresh. Admission remains held until the actual call returns,
    // even after its async caller has timed out or closed the settings page.
    let task = tokio::task::spawn_blocking(move || {
        let _query = query;
        inspect_driver_status(&app)
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), task)
        .await
        .map_err(|_| "Core Audio did not respond to the microphone status check. Try again after audio recovers.".to_string())?
        .map_err(|_| "The microphone driver status check did not complete.".to_string())?
}

static DRIVER_QUERY_ACTIVE: AtomicBool = AtomicBool::new(false);

struct DriverQuery;

impl DriverQuery {
    fn acquire() -> Result<Self, String> {
        DRIVER_QUERY_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| "A microphone status check is still waiting for Core Audio. Try again after audio recovers.".to_string())
    }
}

impl Drop for DriverQuery {
    fn drop(&mut self) {
        DRIVER_QUERY_ACTIVE.store(false, Ordering::Release);
    }
}

#[cfg(target_os = "macos")]
const DRIVER_DESTINATION: &str = "/Library/Audio/Plug-Ins/HAL/GreenmoleMicrophone.driver";

#[cfg(target_os = "macos")]
fn bundled_driver(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    use tauri::Manager;
    app.path()
        .resource_dir()
        .map(|directory| directory.join("microphone/GreenmoleMicrophone.driver"))
        .map_err(|_| "Cannot locate this app's bundled microphone driver.".to_string())
}

#[cfg(target_os = "macos")]
fn is_driver_directory(path: &std::path::Path) -> bool {
    // This is an availability hint; the elevated installer independently checks
    // every payload, signature, owner and destination before it writes anything.
    let directory =
        std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir());
    directory
        && ["Contents/Info.plist", "Contents/MacOS/GreenmoleMicrophone"]
            .iter()
            .all(|relative| {
                std::fs::symlink_metadata(path.join(relative))
                    .is_ok_and(|metadata| metadata.file_type().is_file())
            })
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, PartialEq, Eq)]
enum InstalledDriver {
    Missing,
    NeedsRepair,
    Readable,
}

#[cfg(any(target_os = "macos", test))]
fn inspect_installed_driver(path: &std::path::Path) -> InstalledDriver {
    use std::os::unix::fs::PermissionsExt;

    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return InstalledDriver::Missing
        }
        Err(_) => return InstalledDriver::NeedsRepair,
        Ok(_) => {}
    }
    // _coreaudiod loads this root-owned bundle as a different user. Testing
    // metadata rather than this app's read access catches an executable left at
    // 0700 even when the app itself can inspect its inode. Check every fixed
    // component without following symlinks; the installer owns any repairs.
    for (relative, directory, mode) in [
        ("", true, 0o755),
        ("Contents", true, 0o755),
        ("Contents/MacOS", true, 0o755),
        ("Contents/_CodeSignature", true, 0o755),
        ("Contents/Info.plist", false, 0o644),
        ("Contents/_CodeSignature/CodeResources", false, 0o644),
        ("Contents/MacOS/GreenmoleMicrophone", false, 0o755),
    ] {
        let Ok(metadata) = std::fs::symlink_metadata(path.join(relative)) else {
            return InstalledDriver::NeedsRepair;
        };
        let kind = metadata.file_type();
        if (directory && !kind.is_dir())
            || (!directory && !kind.is_file())
            || metadata.permissions().mode() & 0o7777 != mode
        {
            return InstalledDriver::NeedsRepair;
        }
    }
    InstalledDriver::Readable
}

#[cfg(any(target_os = "macos", test))]
const DRIVER_REPAIR_MESSAGE: &str = "The installed microphone driver has inaccessible or unexpected file permissions or bundle contents, so Core Audio may be unable to load it. Use an updated Greenmole RDP app, stop the RDP service, then choose Reinstall driver to repair the installation.";

#[cfg(target_os = "macos")]
fn inspect_driver_status(app: &tauri::AppHandle) -> Result<MicrophoneDriverStatus, String> {
    let installation = inspect_installed_driver(std::path::Path::new(DRIVER_DESTINATION));
    let installed = installation != InstalledDriver::Missing;
    let installation_available =
        compiled_driver_digest().is_some() && is_driver_directory(&bundled_driver(app)?);
    let ready = installation == InstalledDriver::Readable && macrdp_microphone::driver_available();
    let message = if installation == InstalledDriver::NeedsRepair {
        DRIVER_REPAIR_MESSAGE
    } else if ready {
        "Greenmole Remote Microphone is available in Core Audio and its driver protocol is compatible. The device is silent without an active redirected microphone."
    } else if installed {
        "The driver is installed but its compatible device is not available in Core Audio. Reinstall it, then restart this Mac if the device is still missing."
    } else if !installation_available {
        "This build does not include a validated microphone driver installer. Install a complete Greenmole RDP app bundle."
    } else {
        "Install the virtual microphone driver once to make client audio available to Mac apps."
    };
    Ok(MicrophoneDriverStatus {
        installed,
        ready,
        installation_available,
        removal_available: true,
        message: message.into(),
    })
}

#[cfg(not(target_os = "macos"))]
fn inspect_driver_status(_app: &tauri::AppHandle) -> Result<MicrophoneDriverStatus, String> {
    Ok(MicrophoneDriverStatus {
        installed: false,
        ready: false,
        installation_available: false,
        removal_available: false,
        message: "The virtual microphone driver is available on macOS only.".into(),
    })
}

#[cfg(target_os = "macos")]
async fn run_driver_action(app: &tauri::AppHandle, install: bool) -> Result<(), String> {
    // Embed the reviewed installer text in the executable. Opening a mutable
    // script by path after an administrator approves would create a swap race.
    let script = if install {
        include_str!("../../../native/microphone/install-microphone-driver.sh")
    } else {
        include_str!("../../../native/microphone/uninstall-microphone-driver.sh")
    };
    let mut shell_command = format!("/bin/sh -c {} --", shell_quote(script));
    if install {
        // The payload digest is compiled into this executable, never taken from
        // mutable app resources or an IPC argument. Identifier-only ad-hoc code
        // signatures cannot establish trust in a privileged HAL plug-in.
        let digest = compiled_driver_digest().ok_or(
            "This build has no validated microphone driver installer. Install a complete Greenmole RDP app bundle.",
        )?;
        let source = bundled_driver(app)?;
        if !is_driver_directory(&source) {
            return Err("This app does not contain a complete microphone driver. Install a complete Greenmole RDP app bundle.".into());
        }
        let path = source
            .to_str()
            .ok_or("The microphone driver bundle path is not valid Unicode.")?;
        shell_command.push(' ');
        shell_command.push_str(&shell_quote(path));
        shell_command.push(' ');
        shell_command.push_str(&shell_quote(digest));
    }
    let applescript = format!(
        "do shell script {} with administrator privileges",
        applescript_string(&shell_command)
    );
    tracing::info!(install, "Local microphone driver action requested");
    let output = tokio::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(applescript)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|_| "Could not open the macOS administrator authentication dialog.".to_string())?;
    if !output.status.success() {
        // Never surface unfiltered AppleScript stderr: it can include the entire
        // command. Passwords are handled only by the native macOS dialog.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = installer_error(&stderr);
        tracing::warn!(
            install,
            exit_code = output.status.code(),
            reason,
            "Local microphone driver action failed"
        );
        return Err(reason.into());
    }
    tracing::info!(install, "Local microphone driver action completed");
    Ok(())
}

#[cfg(target_os = "macos")]
fn compiled_driver_digest() -> Option<&'static str> {
    option_env!("MACRDP_MICROPHONE_DIGEST").filter(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[cfg(not(target_os = "macos"))]
async fn run_driver_action(_app: &tauri::AppHandle, _install: bool) -> Result<(), String> {
    Err("The virtual microphone driver is available on macOS only.".into())
}

#[cfg(any(target_os = "macos", test))]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(any(target_os = "macos", test))]
fn applescript_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(any(target_os = "macos", test))]
fn installer_error(stderr: &str) -> &'static str {
    if stderr.contains("(-128)") {
        return "Administrator authentication was cancelled. The microphone driver was not changed.";
    }
    for (marker, message) in [
        ("source_missing", "The bundled microphone driver is missing. Reinstall the complete app."),
        ("source_invalid", "The bundled microphone driver is incomplete or unsafe. Reinstall the complete app."),
        ("signature_invalid", "The microphone driver's code signature could not be verified. Reinstall the complete app."),
        ("destination_invalid", "The existing microphone driver location is unsafe. Check its ownership and permissions locally."),
        ("stage_failed", "The microphone driver could not be staged. Check available disk space and local administrator access."),
        ("install_failed", "The microphone driver could not be installed. Check local administrator access and try again."),
        ("remove_failed", "The microphone driver could not be removed. Check local administrator access and try again."),
        ("audio_restart_failed", "The driver changed, but Core Audio could not be restarted. Restart this Mac before using audio."),
    ] {
        if stderr.contains(&format!("GM_MIC_ERROR:{marker}")) {
            return message;
        }
    }
    "The microphone driver operation failed. Verify administrator authentication and use a complete, signed Greenmole RDP app bundle."
}

#[cfg(test)]
mod tests {
    use super::*;

    fn driver_fixture(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        for relative in ["", "Contents", "Contents/MacOS", "Contents/_CodeSignature"] {
            let directory = path.join(relative);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        for (relative, mode) in [
            ("Contents/Info.plist", 0o644),
            ("Contents/_CodeSignature/CodeResources", 0o644),
            ("Contents/MacOS/GreenmoleMicrophone", 0o755),
        ] {
            let file = path.join(relative);
            std::fs::write(&file, b"synthetic fixture only").unwrap();
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(mode)).unwrap();
        }
    }

    #[test]
    fn installed_driver_classification_identifies_private_or_writable_payloads() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("Fixture.driver");
        assert_eq!(inspect_installed_driver(&path), InstalledDriver::Missing);
        driver_fixture(&path);
        assert_eq!(inspect_installed_driver(&path), InstalledDriver::Readable);
        for (relative, broken_mode) in [
            ("Contents/MacOS/GreenmoleMicrophone", 0o700),
            ("Contents", 0o700),
            ("Contents/MacOS", 0o750),
            ("Contents/_CodeSignature", 0o750),
            ("Contents/Info.plist", 0o600),
            ("Contents/_CodeSignature/CodeResources", 0o600),
            ("Contents/MacOS/GreenmoleMicrophone", 0o777),
        ] {
            let component = path.join(relative);
            let original = std::fs::metadata(&component).unwrap().permissions();
            std::fs::set_permissions(&component, std::fs::Permissions::from_mode(broken_mode))
                .unwrap();
            assert_eq!(
                inspect_installed_driver(&path),
                InstalledDriver::NeedsRepair,
                "{relative}"
            );
            std::fs::set_permissions(component, original).unwrap();
        }
        assert_eq!(inspect_installed_driver(&path), InstalledDriver::Readable);
        assert!(DRIVER_REPAIR_MESSAGE.contains("updated Greenmole RDP app"));
        assert!(DRIVER_REPAIR_MESSAGE.contains("Reinstall driver"));
    }

    #[test]
    fn missing_or_symlinked_installed_payload_requires_reinstallation() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("Fixture.driver");
        driver_fixture(&path);
        let executable = path.join("Contents/MacOS/GreenmoleMicrophone");
        std::fs::remove_file(&executable).unwrap();
        assert_eq!(
            inspect_installed_driver(&path),
            InstalledDriver::NeedsRepair
        );
        let external = temporary.path().join("external");
        std::fs::write(&external, b"not a driver").unwrap();
        symlink(external, executable).unwrap();
        assert_eq!(
            inspect_installed_driver(&path),
            InstalledDriver::NeedsRepair
        );
    }

    #[test]
    fn pending_status_query_cannot_accumulate_workers_on_refresh() {
        let query = DriverQuery::acquire().unwrap();
        assert!(DriverQuery::acquire().is_err());
        drop(query);
        assert!(DriverQuery::acquire().is_ok());
    }

    #[test]
    fn installer_arguments_remain_literal_in_the_shell() {
        let value = "/a b/c'd;$(printf injected) \"quoted\" \\path\nnew line";
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("printf '%s' {}", shell_quote(value)))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, value.as_bytes());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn applescript_keeps_quoted_shell_arguments_literal_without_authentication() {
        let value = "a b'c;$(printf injected) \"quoted\" \\path\nnew line";
        let command = format!("/usr/bin/printf '%s' {}", shell_quote(value));
        let output = std::process::Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(format!(
                "do shell script {} without altering line endings",
                applescript_string(&command)
            ))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{value}\n")
        );
    }

    #[test]
    fn installer_errors_never_return_untrusted_command_output() {
        assert!(installer_error("private path and command (-128)").contains("cancelled"));
        assert!(
            installer_error("GM_MIC_ERROR:signature_invalid private value").contains("signature")
        );
        assert!(!installer_error("private unknown failure").contains("private"));
    }
}

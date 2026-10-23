//! Preserve the existing login-item identity across the product rename. A user
//! may install the renamed bundle at a new path, so refresh only an already
//! enabled registration after a normal app launch, never from a helper process.

use std::path::{Component, Path};
use tauri::plugin::TauriPlugin;
#[cfg(target_os = "macos")]
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_autostart::ManagerExt;

pub(crate) const LEGACY_APP_NAME: &str = "macrdp";

pub(crate) fn plugin<R: tauri::Runtime>() -> TauriPlugin<R> {
    let builder = tauri_plugin_autostart::Builder::new().app_name(LEGACY_APP_NAME);
    #[cfg(target_os = "macos")]
    let builder = builder.macos_launcher(MacosLauncher::LaunchAgent);
    builder.build()
}

/// Retain the user's opt-in and registration name while updating its executable
/// path. Missing/disabled registrations are not created or enabled implicitly.
pub(crate) fn refresh_existing_registration(app: &tauri::AppHandle, saved_enabled: bool) {
    if !saved_enabled || cfg!(debug_assertions) {
        return;
    }
    // Never replace a stable login target with a mounted installer, translocated
    // app or development artifact. Canonicalization also rejects aliases into
    // these temporary locations without changing any registration.
    let Ok(executable) = std::env::current_exe().and_then(|path| path.canonicalize()) else {
        tracing::warn!(
            "Could not inspect the installed executable; launch-at-login registration is unchanged"
        );
        return;
    };
    if !installed_app_path(&executable, false) {
        return;
    }
    let registration = app.autolaunch();
    match refresh_with(
        saved_enabled,
        || registration.is_enabled(),
        || registration.enable(),
    ) {
        Ok(true) => {
            tracing::info!("Refreshed the existing Greenmole RDP launch-at-login registration")
        }
        Ok(false) => {}
        Err(stage) => tracing::warn!(
            stage = stage.label(),
            "Could not refresh the existing launch-at-login registration; preferences are unchanged"
        ),
    }
}

fn installed_app_path(executable: &Path, debug_build: bool) -> bool {
    if debug_build || !executable.is_absolute() {
        return false;
    }
    // The autostart dependency embeds the path directly in XML text. Avoid
    // replacing a valid entry with an unrepresentable or unescaped path.
    let Some(text) = executable.to_str() else {
        return false;
    };
    if text
        .chars()
        .any(|ch| ch.is_control() || matches!(ch, '&' | '<' | '>'))
    {
        return false;
    }
    if [
        "/Volumes",
        "/tmp",
        "/private/tmp",
        "/var/folders",
        "/private/var/folders",
    ]
    .iter()
    .any(|root| executable.starts_with(root))
        || executable.components().any(|part| match part {
            Component::Normal(name) => name == "AppTranslocation" || name == "target",
            Component::CurDir | Component::ParentDir => true,
            _ => false,
        })
    {
        return false;
    }
    let Some(macos) = executable.parent() else {
        return false;
    };
    let Some(contents) = macos.parent() else {
        return false;
    };
    let Some(bundle) = contents.parent() else {
        return false;
    };
    executable
        .file_name()
        .is_some_and(|name| name == "macrdp-ui")
        && macos.file_name().is_some_and(|name| name == "MacOS")
        && contents.file_name().is_some_and(|name| name == "Contents")
        && bundle
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.len() > 4 && name.ends_with(".app"))
}

#[derive(Debug, PartialEq, Eq)]
enum RefreshError {
    Inspect,
    Update,
}

impl RefreshError {
    fn label(&self) -> &'static str {
        match self {
            Self::Inspect => "inspect_registration",
            Self::Update => "update_registration",
        }
    }
}

fn refresh_with<E>(
    saved_enabled: bool,
    is_enabled: impl FnOnce() -> Result<bool, E>,
    enable: impl FnOnce() -> Result<(), E>,
) -> Result<bool, RefreshError> {
    if !saved_enabled {
        return Ok(false);
    }
    if !is_enabled().map_err(|_| RefreshError::Inspect)? {
        return Ok(false);
    }
    enable().map_err(|_| RefreshError::Update)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn stable_installed_bundles_keep_their_existing_login_identity() {
        for path in [
            "/Applications/Greenmole RDP.app/Contents/MacOS/macrdp-ui",
            "/Users/example/Applications/Greenmole RDP.app/Contents/MacOS/macrdp-ui",
            "/Users/example/Desktop/macrdp.app/Contents/MacOS/macrdp-ui",
        ] {
            assert!(installed_app_path(Path::new(path), false));
        }
    }

    #[test]
    fn mounted_translocated_or_temporary_bundles_cannot_replace_login_target() {
        for path in [
            "/Volumes/Greenmole RDP/Greenmole RDP.app/Contents/MacOS/macrdp-ui",
            "/Users/example/AppTranslocation/id/Greenmole RDP.app/Contents/MacOS/macrdp-ui",
            "/tmp/Greenmole RDP.app/Contents/MacOS/macrdp-ui",
            "/private/tmp/Greenmole RDP.app/Contents/MacOS/macrdp-ui",
            "/private/var/folders/id/Greenmole RDP.app/Contents/MacOS/macrdp-ui",
            "/var/folders/id/Greenmole RDP.app/Contents/MacOS/macrdp-ui",
        ] {
            assert!(!installed_app_path(Path::new(path), false));
        }
    }

    #[test]
    fn development_and_unbundled_executables_cannot_replace_login_target() {
        for path in [
            "/Users/example/code/target/release/bundle/macos/Greenmole RDP.app/Contents/MacOS/macrdp-ui",
            "/Applications/Greenmole RDP.app/Contents/MacOS/other-helper",
            "/Users/example/code/macrdp-ui",
            "Greenmole RDP.app/Contents/MacOS/macrdp-ui",
        ] {
            assert!(!installed_app_path(Path::new(path), false));
        }
        assert!(!installed_app_path(
            Path::new("/Applications/Greenmole RDP.app/Contents/MacOS/macrdp-ui"),
            true
        ));
    }

    #[test]
    fn paths_requiring_xml_escaping_do_not_replace_existing_registration() {
        for path in [
            "/Users/Example & Another/Desktop/Greenmole RDP.app/Contents/MacOS/macrdp-ui",
            "/Applications/<test>.app/Contents/MacOS/macrdp-ui",
            "/Applications/Line\nBreak.app/Contents/MacOS/macrdp-ui",
        ] {
            assert!(!installed_app_path(Path::new(path), false));
        }
    }

    #[test]
    fn saved_opt_out_never_inspects_or_changes_system_registration() {
        let result = refresh_with::<()>(
            false,
            || panic!("Opt-out must not inspect the registration"),
            || panic!("Opt-out must not refresh the registration"),
        );
        assert_eq!(result, Ok(false));
    }

    #[test]
    fn missing_registration_does_not_create_an_entry_from_saved_preferences() {
        let inspections = Cell::new(0);
        let result = refresh_with::<()>(
            true,
            || {
                inspections.set(inspections.get() + 1);
                Ok(false)
            },
            || panic!("Missing registration must not be enabled"),
        );
        assert_eq!(result, Ok(false));
        assert_eq!(inspections.get(), 1);
    }

    #[test]
    fn existing_opt_in_is_refreshed_exactly_once() {
        let inspections = Cell::new(0);
        let updates = Cell::new(0);
        let result = refresh_with::<()>(
            true,
            || {
                inspections.set(inspections.get() + 1);
                Ok(true)
            },
            || {
                updates.set(updates.get() + 1);
                Ok(())
            },
        );
        assert_eq!(result, Ok(true));
        assert_eq!(inspections.get(), 1);
        assert_eq!(updates.get(), 1);
    }

    #[test]
    fn inspection_failure_stops_before_any_update() {
        let result = refresh_with::<()>(
            true,
            || Err(()),
            || panic!("Failed inspection must not update the registration"),
        );
        assert_eq!(result, Err(RefreshError::Inspect));
    }

    #[test]
    fn update_failure_is_reported_without_retry() {
        let updates = Cell::new(0);
        let result = refresh_with::<()>(
            true,
            || Ok(true),
            || {
                updates.set(updates.get() + 1);
                Err(())
            },
        );
        assert_eq!(result, Err(RefreshError::Update));
        assert_eq!(updates.get(), 1);
    }
}

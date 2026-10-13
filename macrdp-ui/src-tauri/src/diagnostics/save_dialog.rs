use std::path::PathBuf;

/// Explicit user-selected destinations avoid implicit access to protected Desktop.
#[cfg(target_os = "macos")]
pub(super) async fn destination(app: &tauri::AppHandle) -> Result<Option<PathBuf>, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = sender.send(select_on_main_thread());
    })
    .map_err(|error| error.to_string())?;
    receiver.await.map_err(|error| error.to_string())?
}

#[cfg(target_os = "macos")]
fn select_on_main_thread() -> Result<Option<PathBuf>, String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSModalResponseOK, NSSavePanel};
    use objc2_foundation::NSString;
    let mtm = MainThreadMarker::new().ok_or("Save dialog requires the main thread")?;
    let panel = NSSavePanel::savePanel(mtm);
    panel.setTitle(Some(&NSString::from_str("Export macrdp diagnostics")));
    panel.setNameFieldStringValue(&NSString::from_str(&format!(
        "macrdp-diagnostics-{}.json",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    )));
    panel.setCanCreateDirectories(true);
    if panel.runModal() != NSModalResponseOK {
        return Ok(None);
    }
    let url = panel.URL().ok_or("Save dialog returned no destination")?;
    if !url.isFileURL() {
        return Err("Choose a local file destination".into());
    }
    let path = PathBuf::from(
        url.path()
            .ok_or("Save destination has no path")?
            .to_string(),
    );
    if !path.is_absolute() {
        return Err("Save destination must be absolute".into());
    }
    Ok(Some(path))
}

#[cfg(not(target_os = "macos"))]
pub(super) async fn destination(_: &tauri::AppHandle) -> Result<Option<PathBuf>, String> {
    Err("Diagnostics export requires the macOS Save dialog".into())
}

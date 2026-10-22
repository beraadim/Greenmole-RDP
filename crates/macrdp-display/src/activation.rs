//! Runtime-checked private enable/disable API, used only by disposable selectors.
//! Unlike moving a screen offscreen, disabling removes it from the online
//! desktop. The CGVirtualDisplay object and identity lease stay in its owner.

use anyhow::{bail, ensure, Context, Result};
use core_graphics::display::{
    CGBeginDisplayConfiguration, CGCancelDisplayConfiguration, CGCompleteDisplayConfiguration,
    CGConfigureOption, CGDisplay,
};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::identity::IdentityLease;
use crate::virtual_display::{PRODUCT_ID, VENDOR_ID};

type ConfigureEnabled =
    unsafe extern "C" fn(core_graphics::display::CGDisplayConfigRef, u32, bool) -> i32;
type GetDisplayList = unsafe extern "C" fn(u32, *mut u32, *mut u32) -> i32;

#[derive(Clone, Copy)]
struct Api {
    configure_enabled: ConfigureEnabled,
    get_display_list: GetDisplayList,
}

fn api() -> Result<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        load_api(|name| {
            // CoreGraphics is already framework-linked and forwards these symbols
            // to SkyLight. Do not load an arbitrary path or guess a missing ABI.
            // Signatures match CGSConfigureDisplayEnabled(CGDisplayConfigRef,
            // CGDirectDisplayID, bool) and CGSGetDisplayList(CGDisplayCount,...).
            unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr().cast()) }
        })
    })
    .as_ref()
    .context("This macOS version does not expose retained display activation")
}

fn load_api(resolve: impl Fn(&[u8]) -> *mut libc::c_void) -> Option<Api> {
    let enabled = resolve(b"CGSConfigureDisplayEnabled\0");
    let list = resolve(b"CGSGetDisplayList\0");
    if enabled.is_null() || list.is_null() {
        return None;
    }
    // SAFETY: callers resolve only the exact known private C symbols using
    // their declared signatures. Missing symbols are rejected as a unit.
    Some(unsafe {
        Api {
            configure_enabled: std::mem::transmute::<*mut libc::c_void, ConfigureEnabled>(enabled),
            get_display_list: std::mem::transmute::<*mut libc::c_void, GetDisplayList>(list),
        }
    })
}

pub(crate) fn available() -> bool {
    api().is_ok()
}

/// Public online enumeration omits a disabled retained screen. Never use
/// absence from that list to infer that its object or owner has been removed.
pub(crate) fn all_display_ids() -> Result<Vec<u32>> {
    let api = api()?;
    let mut capacity = 16u32;
    for _ in 0..5 {
        let mut ids = vec![0; capacity as usize];
        let mut count = 0;
        // SAFETY: buffer and count have the advertised capacity and ABI.
        let status = unsafe { (api.get_display_list)(capacity, ids.as_mut_ptr(), &mut count) };
        ensure!(
            status == 0,
            "Could not enumerate retained display slots: {status}"
        );
        if count < capacity {
            ids.truncate(count as usize);
            return Ok(ids);
        }
        capacity *= 2;
    }
    bail!("Retained display slot enumeration exceeded its bound")
}

fn validate_owned(display_id: u32, identity: &IdentityLease) -> Result<CGDisplay> {
    identity.validate_current()?;
    ensure!(
        display_id != 0 && all_display_ids()?.contains(&display_id),
        "The retained virtual display no longer exists"
    );
    let display = CGDisplay::new(display_id);
    ensure!(
        !display.is_builtin()
            && !display.is_in_mirror_set()
            && display.vendor_number() == VENDOR_ID
            && display.model_number() == PRODUCT_ID
            && identity.serial() != 0
            && display.serial_number() == identity.serial(),
        "Activation target does not match the exact owned virtual display"
    );
    Ok(display)
}

pub(crate) fn configure_enabled(
    display_id: u32,
    identity: &IdentityLease,
    enabled: bool,
) -> Result<()> {
    let api = api()?;
    validate_owned(display_id, identity)?;
    if !enabled {
        let active = CGDisplay::active_displays()
            .map_err(|error| anyhow::anyhow!("Could not verify the restored desktop: {error}"))?;
        ensure!(
            CGDisplay::main().id != display_id && active.iter().any(|id| *id != display_id),
            "Restore a surviving local primary before disabling the virtual display"
        );
    }
    let online = crate::macos::online_display_ids()?.contains(&display_id);
    if online != enabled {
        // SAFETY: the inherited identity/generation and exact native target
        // were verified. This transaction changes only that virtual display's
        // enabled state, never a physical display or permanent preferences.
        unsafe {
            let mut configuration = std::ptr::null_mut();
            let status = CGBeginDisplayConfiguration(&mut configuration);
            ensure!(
                status == 0 && !configuration.is_null(),
                "Could not begin display activation: {status}"
            );
            let staged = (api.configure_enabled)(configuration, display_id, enabled);
            if staged != 0 {
                let cancelled = CGCancelDisplayConfiguration(configuration);
                bail!("Could not stage virtual display enabled={enabled}: {staged}; cancel status {cancelled}");
            }
            if let Err(error) = identity.validate_current() {
                let cancelled = CGCancelDisplayConfiguration(configuration);
                return Err(error).context(format!(
                    "Activation owner changed; cancel status {cancelled}"
                ));
            }
            // Complete consumes the configuration even if it reports failure.
            let status = CGCompleteDisplayConfiguration(
                configuration,
                CGConfigureOption::ConfigureForSession,
            );
            ensure!(
                status == 0,
                "Could not commit virtual display enabled={enabled}: {status}"
            );
        }
    }
    let deadline = Instant::now() + Duration::from_millis(1000);
    loop {
        validate_owned(display_id, identity)?;
        let online = crate::macos::online_display_ids()?.contains(&display_id);
        let active = CGDisplay::active_displays()
            .map_err(|error| anyhow::anyhow!("Could not verify display activation: {error}"))?
            .contains(&display_id);
        if online == enabled && active == enabled {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Virtual display enabled={enabled} did not settle"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn enabled_stub(
        _: core_graphics::display::CGDisplayConfigRef,
        _: u32,
        _: bool,
    ) -> i32 {
        0
    }
    unsafe extern "C" fn list_stub(_: u32, _: *mut u32, _: *mut u32) -> i32 {
        0
    }

    #[test]
    fn optional_api_is_unavailable_unless_both_exact_symbols_resolve() {
        assert!(load_api(|_| std::ptr::null_mut()).is_none());
        assert!(load_api(|name| if name == b"CGSConfigureDisplayEnabled\0" {
            enabled_stub as *mut libc::c_void
        } else {
            std::ptr::null_mut()
        })
        .is_none());
        assert!(load_api(|name| if name == b"CGSGetDisplayList\0" {
            list_stub as *mut libc::c_void
        } else {
            std::ptr::null_mut()
        })
        .is_none());
        assert!(load_api(|name| if name == b"CGSGetDisplayList\0" {
            list_stub as *mut libc::c_void
        } else {
            enabled_stub as *mut libc::c_void
        })
        .is_some());
    }
}

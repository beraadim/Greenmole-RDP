//! Small, checked wrappers around the public Accessibility C API.
//! Native references never leave the worker that created them.

use super::geometry::Frame;
use super::process_filter::is_external_application;
use crate::window_move::Budget;
use core_foundation::base::{CFType, TCFType};
use core_foundation::string::CFString;
use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex};
use core_foundation_sys::base::{CFEqual, CFGetTypeID, CFTypeRef};
use core_foundation_sys::number::{CFBooleanGetTypeID, CFBooleanGetValue};
use core_graphics::geometry::{CGPoint, CGSize};
use std::marker::PhantomData;
use std::rc::Rc;

const TIMEOUT_SECONDS: f32 = 0.10;
const MAX_WINDOWS_PER_APP: isize = 256;
const AX_UNSUPPORTED_ATTRIBUTE: i32 = -25205;
const AX_NO_VALUE: i32 = -25212;
const AX_POINT: u32 = 1;
const AX_SIZE: u32 = 2;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
    fn AXUIElementGetTypeID() -> usize;
    fn AXUIElementSetMessagingTimeout(element: CFTypeRef, seconds: f32) -> i32;
    fn AXUIElementCopyAttributeValue(
        element: CFTypeRef,
        attribute: CFTypeRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementCopyAttributeValues(
        element: CFTypeRef,
        attribute: CFTypeRef,
        index: isize,
        max_values: isize,
        values: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementSetAttributeValue(
        element: CFTypeRef,
        attribute: CFTypeRef,
        value: CFTypeRef,
    ) -> i32;
    fn AXUIElementIsAttributeSettable(
        element: CFTypeRef,
        attribute: CFTypeRef,
        settable: *mut u8,
    ) -> i32;
    fn AXValueGetTypeID() -> usize;
    fn AXValueGetType(value: CFTypeRef) -> u32;
    fn AXValueGetValue(value: CFTypeRef, kind: u32, output: *mut std::ffi::c_void) -> bool;
    fn AXValueCreate(kind: u32, value: *const std::ffi::c_void) -> CFTypeRef;
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AxError(pub i32);

impl std::fmt::Display for AxError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Accessibility error {}", self.0)
    }
}

impl std::error::Error for AxError {}

type AxResult<T> = Result<T, AxError>;

/// CFType follows the create/copy/get ownership rules. The marker explicitly
/// prevents sending or sharing AX references, even if a future CF crate permits it.
pub(crate) struct Element {
    value: CFType,
    budget: Budget,
    _worker_only: PhantomData<Rc<()>>,
}

pub(crate) fn trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

impl Element {
    fn raw(&self) -> CFTypeRef {
        self.value.as_CFTypeRef()
    }

    fn from_value(value: CFType, budget: &Budget) -> AxResult<Self> {
        check_budget(budget)?;
        if value.type_of() != unsafe { AXUIElementGetTypeID() } {
            return Err(AxError(-25201));
        }
        let element = Self {
            value,
            budget: budget.clone(),
            _worker_only: PhantomData,
        };
        // AX timeouts are per exact reference, not per PID or equal AX object.
        status(unsafe { AXUIElementSetMessagingTimeout(element.raw(), TIMEOUT_SECONDS) })?;
        Ok(element)
    }

    fn copy(&self, attribute: &str) -> AxResult<Option<CFType>> {
        check_budget(&self.budget)?;
        let key = CFString::new(attribute);
        let mut value = std::ptr::null();
        let result =
            unsafe { AXUIElementCopyAttributeValue(self.raw(), key.as_CFTypeRef(), &mut value) };
        if result == AX_UNSUPPORTED_ATTRIBUTE || result == AX_NO_VALUE {
            return Ok(None);
        }
        status(result)?;
        if value.is_null() {
            return Err(AxError(-25201));
        }
        Ok(Some(unsafe { CFType::wrap_under_create_rule(value) }))
    }

    fn boolean(&self, attribute: &str) -> AxResult<Option<bool>> {
        self.copy(attribute)?
            .map(|value| {
                if value.type_of() != unsafe { CFBooleanGetTypeID() } {
                    return Err(AxError(-25201));
                }
                Ok(unsafe { CFBooleanGetValue(value.as_CFTypeRef().cast()) })
            })
            .transpose()
    }

    fn string_is(&self, attribute: &str, expected: &str) -> AxResult<bool> {
        let expected = CFString::new(expected);
        Ok(self
            .copy(attribute)?
            .map(|value| unsafe { CFEqual(value.as_CFTypeRef(), expected.as_CFTypeRef()) != 0 })
            .unwrap_or(false))
    }

    fn settable(&self, attribute: &str) -> AxResult<bool> {
        check_budget(&self.budget)?;
        let key = CFString::new(attribute);
        let mut settable = 0;
        let result = unsafe {
            AXUIElementIsAttributeSettable(self.raw(), key.as_CFTypeRef(), &mut settable)
        };
        if result == AX_UNSUPPORTED_ATTRIBUTE {
            return Ok(false);
        }
        status(result)?;
        Ok(settable != 0)
    }

    /// Keep minimized and native fullscreen windows in their existing state and
    /// Space. Missing optional fullscreen metadata is normal for older apps.
    pub fn movable(&self) -> AxResult<Option<bool>> {
        if !self.string_is("AXRole", "AXWindow")?
            || self.boolean("AXMinimized")? == Some(true)
            || self.boolean("AXFullScreen")? == Some(true)
            || !self.settable("AXPosition")?
        {
            return Ok(None);
        }
        Ok(Some(self.settable("AXSize")?))
    }

    pub fn frame(&self) -> AxResult<Frame> {
        let mut point = CGPoint::new(0.0, 0.0);
        let mut size = CGSize::new(0.0, 0.0);
        self.read_value("AXPosition", AX_POINT, (&mut point as *mut CGPoint).cast())?;
        self.read_value("AXSize", AX_SIZE, (&mut size as *mut CGSize).cast())?;
        let frame = Frame {
            x: point.x,
            y: point.y,
            width: size.width,
            height: size.height,
        };
        if !frame.valid() {
            return Err(AxError(-25201));
        }
        Ok(frame)
    }

    fn read_value(
        &self,
        attribute: &str,
        kind: u32,
        output: *mut std::ffi::c_void,
    ) -> AxResult<()> {
        let value = self.copy(attribute)?.ok_or(AxError(AX_NO_VALUE))?;
        if value.type_of() != unsafe { AXValueGetTypeID() }
            || unsafe { AXValueGetType(value.as_CFTypeRef()) } != kind
        {
            return Err(AxError(-25201));
        }
        if !unsafe { AXValueGetValue(value.as_CFTypeRef(), kind, output) } {
            return Err(AxError(-25201));
        }
        Ok(())
    }

    fn write_value(
        &self,
        attribute: &str,
        kind: u32,
        input: *const std::ffi::c_void,
    ) -> AxResult<()> {
        check_budget(&self.budget)?;
        let value = unsafe { AXValueCreate(kind, input) };
        if value.is_null() {
            return Err(AxError(-25201));
        }
        let value = unsafe { CFType::wrap_under_create_rule(value) };
        let attribute = CFString::new(attribute);
        status(unsafe {
            AXUIElementSetAttributeValue(self.raw(), attribute.as_CFTypeRef(), value.as_CFTypeRef())
        })
    }

    pub fn set_frame(&self, frame: Frame, resizable: bool) -> AxResult<()> {
        let point = CGPoint::new(frame.x, frame.y);
        let size = CGSize::new(frame.width, frame.height);
        // Shrink before moving so an oversized window can fit on the target.
        if resizable {
            let _ = self.write_value("AXSize", AX_SIZE, (&size as *const CGSize).cast());
        }
        let final_position =
            self.write_value("AXPosition", AX_POINT, (&point as *const CGPoint).cast());
        // Attempt position even if a size constraint rejects resizing. AX may
        // report success while the app silently enforces another frame.
        let actual = self.frame()?;
        if actual.close_to(frame) {
            return Ok(());
        }
        final_position?;
        Err(AxError(-25200))
    }
}

fn status(value: i32) -> AxResult<()> {
    if value == 0 {
        Ok(())
    } else {
        Err(AxError(value))
    }
}

fn check_budget(budget: &Budget) -> AxResult<()> {
    if budget.expired() {
        Err(AxError(-25204))
    } else {
        Ok(())
    }
}

pub(crate) fn windows(pid: i32, budget: &Budget) -> AxResult<Vec<Element>> {
    // Enforce the boundary here as well as in discovery so future callers
    // cannot invoke our own AppKit implementation from this worker thread.
    if !is_external_application(pid) {
        return Ok(Vec::new());
    }
    check_budget(budget)?;
    let raw = unsafe { AXUIElementCreateApplication(pid) };
    if raw.is_null() {
        return Err(AxError(-25201));
    }
    let application = Element::from_value(unsafe { CFType::wrap_under_create_rule(raw) }, budget)?;
    let attribute = CFString::new("AXWindows");
    let mut array = std::ptr::null();
    let result = unsafe {
        AXUIElementCopyAttributeValues(
            application.raw(),
            attribute.as_CFTypeRef(),
            0,
            MAX_WINDOWS_PER_APP,
            &mut array,
        )
    };
    if result == AX_UNSUPPORTED_ATTRIBUTE || result == AX_NO_VALUE {
        return Ok(Vec::new());
    }
    status(result)?;
    if array.is_null() {
        return Err(AxError(-25201));
    }
    let array = unsafe { CFType::wrap_under_create_rule(array) };
    if array.type_of() != unsafe { CFArrayGetTypeID() } {
        return Err(AxError(-25201));
    }
    let count = unsafe { CFArrayGetCount(array.as_CFTypeRef().cast()) }.min(MAX_WINDOWS_PER_APP);
    let mut elements = Vec::with_capacity(count as usize);
    for index in 0..count {
        let raw = unsafe { CFArrayGetValueAtIndex(array.as_CFTypeRef().cast(), index) };
        if raw.is_null() || unsafe { CFGetTypeID(raw) } != unsafe { AXUIElementGetTypeID() } {
            continue;
        }
        if let Ok(element) =
            Element::from_value(unsafe { CFType::wrap_under_get_rule(raw) }, budget)
        {
            elements.push(element);
        }
    }
    Ok(elements)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_rejects_local_and_invalid_pids_before_accessibility_access() {
        std::thread::Builder::new()
            .name("macrdp-windows-self-pid-regression".into())
            .spawn(|| {
                let own_pid = i32::try_from(std::process::id()).expect("process PID fits pid_t");
                let (request, _preparation) = crate::window_move::Request::new(|_| Ok(0));
                // No Accessibility permission or GUI is needed: these calls
                // must finish without creating or querying any native AX object.
                for pid in [own_pid, 0, -1, i32::MIN] {
                    assert!(windows(pid, &request.budget)
                        .expect("excluded PID is harmless")
                        .is_empty());
                }
            })
            .expect("spawn AX boundary regression worker")
            .join()
            .expect("AX boundary regression worker finished");
    }
}

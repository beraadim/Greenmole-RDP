//! Small runtime-checked bridge to the private CGVirtualDisplay classes.
//!
//! Apple does not publish these interfaces as a supported SDK API. Verify all
//! selectors and ABI encodings before use, and report incompatibility instead
//! of substituting capture of a physical display. These objects never leave the
//! display worker. Session-scoped primary layout changes are handled separately
//! by the display owner; this bridge never configures mirroring or physical modes.

use anyhow::{anyhow, ensure, Context, Result};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{msg_send, sel};
use objc2_foundation::{NSArray, NSPoint, NSSize, NSString};
use std::ffi::CStr;

use crate::transition::{self, Details, Phase};
use crate::{MAX_VIRTUAL_HEIGHT, MAX_VIRTUAL_WIDTH};
pub(crate) const VENDOR_ID: u32 = 0x4D52;
pub(crate) const PRODUCT_ID: u32 = 1;

extern "C" {
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut AnyObject;
}

struct Classes {
    descriptor: &'static AnyClass,
    display: &'static AnyClass,
    settings: &'static AnyClass,
    mode: &'static AnyClass,
}

macro_rules! verify {
    ($class:expr, $selector:expr, $args:ty, $result:ty) => {
        $class
            .verify_sel::<$args, $result>($selector)
            .map_err(|error| {
                anyhow!(
                    "Unsupported virtual display API {}: {error}",
                    $class.name().to_string_lossy()
                )
            })?
    };
}

fn find_class(name: &'static [u8]) -> Result<&'static AnyClass> {
    let name = CStr::from_bytes_with_nul(name).expect("constant Objective-C class name");
    AnyClass::get(name).with_context(|| {
        format!(
            "Virtual display API {} is unavailable on this macOS version",
            name.to_string_lossy()
        )
    })
}

impl Classes {
    fn load() -> Result<Self> {
        let classes = Self {
            descriptor: find_class(b"CGVirtualDisplayDescriptor\0")?,
            display: find_class(b"CGVirtualDisplay\0")?,
            settings: find_class(b"CGVirtualDisplaySettings\0")?,
            mode: find_class(b"CGVirtualDisplayMode\0")?,
        };
        for class in [
            classes.descriptor,
            classes.display,
            classes.settings,
            classes.mode,
        ] {
            verify!(class.metaclass(), sel!(alloc), (), *mut AnyObject);
        }
        for class in [classes.descriptor, classes.settings] {
            verify!(class, sel!(init), (), *mut AnyObject);
        }
        let descriptor = classes.descriptor;
        verify!(descriptor, sel!(setQueue:), (*mut AnyObject,), ());
        verify!(descriptor, sel!(setName:), (*mut AnyObject,), ());
        verify!(descriptor, sel!(setMaxPixelsWide:), (u32,), ());
        verify!(descriptor, sel!(setMaxPixelsHigh:), (u32,), ());
        verify!(descriptor, sel!(setSizeInMillimeters:), (NSSize,), ());
        verify!(descriptor, sel!(setRedPrimary:), (NSPoint,), ());
        verify!(descriptor, sel!(setGreenPrimary:), (NSPoint,), ());
        verify!(descriptor, sel!(setBluePrimary:), (NSPoint,), ());
        verify!(descriptor, sel!(setWhitePoint:), (NSPoint,), ());
        verify!(descriptor, sel!(setVendorID:), (u32,), ());
        verify!(descriptor, sel!(setProductID:), (u32,), ());
        verify!(descriptor, sel!(setSerialNum:), (u32,), ());
        // Newer macOS versions expose a second serial-number property.
        if descriptor.instance_method(sel!(setSerialNumber:)).is_some() {
            verify!(descriptor, sel!(setSerialNumber:), (u32,), ());
        }
        verify!(
            classes.display,
            sel!(initWithDescriptor:),
            (*mut AnyObject,),
            *mut AnyObject
        );
        verify!(classes.display, sel!(displayID), (), u32);
        verify!(
            classes.display,
            sel!(applySettings:),
            (*mut AnyObject,),
            Bool
        );
        verify!(classes.settings, sel!(setModes:), (*mut AnyObject,), ());
        verify!(classes.settings, sel!(setHiDPI:), (u32,), ());
        verify!(
            classes.mode,
            sel!(initWithWidth:height:refreshRate:),
            (u32, u32, f64),
            *mut AnyObject
        );
        Ok(classes)
    }
}

fn instantiate(class: &AnyClass) -> Result<Retained<AnyObject>> {
    // SAFETY: Classes::load validated alloc/init signatures. Retained takes
    // ownership according to Objective-C's alloc/init method-family rules.
    unsafe {
        let allocated: Allocated<AnyObject> = msg_send![class, alloc];
        let object: Option<Retained<AnyObject>> = msg_send![allocated, init];
        object.with_context(|| format!("Could not initialize {}", class.name().to_string_lossy()))
    }
}

pub(crate) struct VirtualDisplay {
    // Drop the display before its supporting descriptor/settings objects.
    display: Retained<AnyObject>,
    _descriptor: Retained<AnyObject>,
    settings: Option<Retained<AnyObject>>,
    classes: Classes,
    id: u32,
    identity: crate::identity::IdentityLease,
}

impl VirtualDisplay {
    pub(crate) fn new(width: u16, height: u16, slot: &str) -> Result<Self> {
        transition::native(
            Phase::VirtualCreate,
            Details::default().size(width, height),
            || Self::create(width, height, slot),
        )
    }

    fn create(width: u16, height: u16, slot: &str) -> Result<Self> {
        let identity = crate::identity::IdentityLease::acquire(slot)?;
        // A disposable selector checks collisions without warming this owner's
        // CoreGraphics mode cache before the virtual display becomes active.
        crate::helper::DisplayHelper::validate_identity(&identity)?;
        identity.validate_current()?;
        let classes = Classes::load()?;
        let descriptor = instantiate(classes.descriptor)?;
        let name = NSString::from_str("macRDP Remote Display");
        let serial = identity.serial();
        // SAFETY: This is a process-lifetime dispatch queue, not an Objective-C
        // object transferred from another Rust thread. All setter ABIs were
        // validated above; the descriptor retains its queue and name.
        let display = unsafe {
            let queue = dispatch_get_global_queue(0, 0);
            ensure!(
                !queue.is_null(),
                "Could not obtain a virtual display dispatch queue"
            );
            let _: () = msg_send![&*descriptor, setQueue: queue];
            let _: () = msg_send![&*descriptor, setName: &*name];
            let _: () = msg_send![&*descriptor, setMaxPixelsWide: u32::from(MAX_VIRTUAL_WIDTH)];
            let _: () = msg_send![&*descriptor, setMaxPixelsHigh: u32::from(MAX_VIRTUAL_HEIGHT)];
            let _: () = msg_send![&*descriptor, setSizeInMillimeters: NSSize::new(600.0, 337.5)];
            let _: () = msg_send![&*descriptor, setRedPrimary: NSPoint::new(0.64, 0.33)];
            let _: () = msg_send![&*descriptor, setGreenPrimary: NSPoint::new(0.30, 0.60)];
            let _: () = msg_send![&*descriptor, setBluePrimary: NSPoint::new(0.15, 0.06)];
            let _: () = msg_send![&*descriptor, setWhitePoint: NSPoint::new(0.3127, 0.3290)];
            let _: () = msg_send![&*descriptor, setVendorID: VENDOR_ID];
            let _: () = msg_send![&*descriptor, setProductID: PRODUCT_ID];
            let _: () = msg_send![&*descriptor, setSerialNum: serial];
            if classes
                .descriptor
                .instance_method(sel!(setSerialNumber:))
                .is_some()
            {
                let _: () = msg_send![&*descriptor, setSerialNumber: serial];
            }
            let allocated: Allocated<AnyObject> = msg_send![classes.display, alloc];
            let display: Option<Retained<AnyObject>> =
                msg_send![allocated, initWithDescriptor: &*descriptor];
            display.context("macOS refused to create the virtual display")?
        };
        // SAFETY: display is retained and displayID's u32 return was checked.
        let id: u32 = unsafe { msg_send![&*display, displayID] };
        ensure!(
            id != 0,
            "Virtual display creation returned an invalid display ID"
        );
        let mut owner = Self {
            display,
            _descriptor: descriptor,
            settings: None,
            classes,
            id,
            identity,
        };
        owner.apply_mode(width, height)?;
        eprintln!("macrdp virtual display created: display_id={id} serial={serial}");
        tracing::info!(
            display_id = id,
            width,
            height,
            "Created persistent virtual display"
        );
        Ok(owner)
    }

    pub(crate) fn id(&self) -> u32 {
        self.id
    }

    pub(crate) fn identity(&self) -> &crate::identity::IdentityLease {
        &self.identity
    }

    pub(crate) fn validate_owned_id(&self) -> Result<()> {
        self.identity.validate_current()?;
        // The native object, rather than an old public display-list entry,
        // proves that this exact owner still holds the ID while disabled.
        let current: u32 = unsafe { msg_send![&*self.display, displayID] };
        ensure!(
            current != 0 && current == self.id,
            "Retained display object changed its identity"
        );
        Ok(())
    }

    pub(crate) fn apply_mode(&mut self, width: u16, height: u16) -> Result<()> {
        transition::native(
            Phase::ModeApply,
            Details::display(self.id).size(width, height),
            || self.apply_mode_inner(width, height),
        )
    }

    fn apply_mode_inner(&mut self, width: u16, height: u16) -> Result<()> {
        // No CoreGraphics display query may precede first activation: Tahoe can
        // otherwise cache the not-yet-active display's missing modes forever.
        if self.settings.is_some() {
            ensure!(
                !core_graphics::display::CGDisplay::new(self.id).is_in_mirror_set(),
                "Refusing to change a mirrored virtual display mode"
            );
        }
        let logical_width = u32::from(width);
        let logical_height = u32::from(height);
        let settings = instantiate(self.classes.settings)?;
        // SAFETY: The private signatures were verified before creating this
        // owner. All objects remain owned by this worker; NSArray retains mode.
        unsafe {
            let allocated: Allocated<AnyObject> = msg_send![self.classes.mode, alloc];
            let mode: Option<Retained<AnyObject>> = msg_send![allocated,
                initWithWidth: logical_width, height: logical_height, refreshRate: 60.0f64];
            let mode = mode.context("macOS refused the virtual display mode")?;
            let modes = NSArray::from_slice(&[&*mode]);
            // Remote viewports use equal logical and backing pixel dimensions.
            let _: () = msg_send![&*settings, setHiDPI: 0u32];
            let _: () = msg_send![&*settings, setModes: &*modes];
            let accepted: Bool = msg_send![&*self.display, applySettings: &*settings];
            ensure!(
                accepted.as_bool(),
                "macOS rejected virtual display mode {width}x{height}"
            );
        }
        self.settings = Some(settings);
        Ok(())
    }
}

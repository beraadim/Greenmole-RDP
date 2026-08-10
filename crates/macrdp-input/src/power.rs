use anyhow::{anyhow, Result};
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::string::CFStringRef;
use std::time::{Duration, Instant};

type IOPMAssertionID = u32;
type IOPMAssertionLevel = u32;
type IOPMUserActiveType = u32;
type IOReturn = i32;

const K_IOPM_ASSERTION_LEVEL_ON: IOPMAssertionLevel = 255;
const K_IOPM_USER_ACTIVE_REMOTE: IOPMUserActiveType = 1;
const K_IORETURN_SUCCESS: IOReturn = 0;
const WAKE_ACTIVITY_MIN_INTERVAL: Duration = Duration::from_secs(2);
const DISPLAY_SLEEP_ASSERTION_TYPE: &str = "PreventUserIdleDisplaySleep";
const DISPLAY_SLEEP_REASON: &str = "macrdp remote session active";
const USER_ACTIVITY_REASON: &str = "macrdp remote input";

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: CFStringRef,
        assertion_level: IOPMAssertionLevel,
        assertion_name: CFStringRef,
        assertion_id: *mut IOPMAssertionID,
    ) -> IOReturn;

    fn IOPMAssertionDeclareUserActivity(
        assertion_name: CFStringRef,
        user_type: IOPMUserActiveType,
        assertion_id: *mut IOPMAssertionID,
    ) -> IOReturn;

    fn IOPMAssertionRelease(assertion_id: IOPMAssertionID);
}

/// Keeps the display awake for an active remote session and nudges it awake
/// when fresh remote input arrives.
pub struct RemoteSessionWake {
    display_sleep_assertion_id: Option<IOPMAssertionID>,
    user_activity_assertion_id: IOPMAssertionID,
    last_user_activity: Option<Instant>,
}

impl RemoteSessionWake {
    pub fn new() -> Result<Self> {
        let mut display_sleep_assertion_id = 0;
        let assertion_type = CFString::new(DISPLAY_SLEEP_ASSERTION_TYPE);
        let assertion_name = CFString::new(DISPLAY_SLEEP_REASON);
        let result = unsafe {
            IOPMAssertionCreateWithName(
                assertion_type.as_concrete_TypeRef(),
                K_IOPM_ASSERTION_LEVEL_ON,
                assertion_name.as_concrete_TypeRef(),
                &mut display_sleep_assertion_id,
            )
        };

        if result != K_IORETURN_SUCCESS {
            return Err(anyhow!(
                "IOPMAssertionCreateWithName failed with IOReturn {result}"
            ));
        }

        Ok(Self {
            display_sleep_assertion_id: Some(display_sleep_assertion_id),
            user_activity_assertion_id: 0,
            last_user_activity: None,
        })
    }

    pub fn wake_display_for_session_start(&mut self) -> Result<()> {
        self.declare_user_activity("session-start", true)
    }

    pub fn wake_display_for_input(&mut self, trigger: &str) -> Result<()> {
        self.declare_user_activity(trigger, false)
    }

    fn declare_user_activity(&mut self, trigger: &str, force: bool) -> Result<()> {
        let now = Instant::now();
        if !force
            && self
                .last_user_activity
                .is_some_and(|last| now.duration_since(last) < WAKE_ACTIVITY_MIN_INTERVAL)
        {
            return Ok(());
        }

        let assertion_name = CFString::new(USER_ACTIVITY_REASON);
        let mut assertion_id = self.user_activity_assertion_id;
        let result = unsafe {
            IOPMAssertionDeclareUserActivity(
                assertion_name.as_concrete_TypeRef(),
                K_IOPM_USER_ACTIVE_REMOTE,
                &mut assertion_id,
            )
        };

        if result != K_IORETURN_SUCCESS {
            return Err(anyhow!(
                "IOPMAssertionDeclareUserActivity failed with IOReturn {result}"
            ));
        }

        self.user_activity_assertion_id = assertion_id;
        self.last_user_activity = Some(now);
        tracing::debug!(trigger, "Declared remote user activity for display wake");
        Ok(())
    }
}

impl Drop for RemoteSessionWake {
    fn drop(&mut self) {
        if self.user_activity_assertion_id != 0 {
            unsafe {
                IOPMAssertionRelease(self.user_activity_assertion_id);
            }
        }

        if let Some(assertion_id) = self.display_sleep_assertion_id.take() {
            unsafe {
                IOPMAssertionRelease(assertion_id);
            }
        }
    }
}

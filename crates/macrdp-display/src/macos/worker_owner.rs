//! Server-lifetime ownership is distinct from an active remote desktop session.
//! A parked helper retains its exact native object and lease, but cannot provide
//! capture geometry or accept window actions until activation succeeds.

use super::*;

pub(super) enum WorkerOwner {
    Main(NativeDisplay),
    Virtual {
        helper: Option<crate::helper::DisplayHelper>,
        initial_width: u16,
        initial_height: u16,
        identity_slot: String,
        retain_between_sessions: bool,
    },
}

impl WorkerOwner {
    pub(super) fn move_windows(
        &mut self,
        budget: crate::window_move::Budget,
        own_windows: Box<dyn FnOnce(DisplayGeometry) -> Result<usize> + Send>,
    ) -> Result<crate::WindowMoveReport> {
        budget.check()?;
        ensure!(
            matches!(self, Self::Virtual { helper: Some(helper), .. } if !helper.is_parked()),
            "Connect a remote client before moving windows to its virtual display"
        );
        let geometry = self.geometry()?;
        budget.check()?;
        crate::manual_windows::move_windows(geometry, budget, own_windows)
    }

    pub(super) fn begin_remote_session(&mut self) -> Result<DisplayGeometry> {
        match self {
            Self::Main(owner) => owner.begin_remote_session(),
            Self::Virtual {
                helper,
                initial_width,
                initial_height,
                identity_slot,
                ..
            } => {
                if helper.is_none() {
                    *helper = Some(crate::helper::DisplayHelper::new(
                        *initial_width,
                        *initial_height,
                        identity_slot,
                    )?);
                }
                helper
                    .as_mut()
                    .expect("initialized session display")
                    .begin_remote_session()
            }
        }
    }

    pub(super) fn end_remote_session(&mut self) -> Result<()> {
        match self {
            Self::Main(owner) => owner.end_remote_session().map(|_| ()),
            Self::Virtual {
                helper,
                retain_between_sessions,
                ..
            } => {
                let retained = *retain_between_sessions;
                park_or_retire(
                    helper,
                    retained,
                    |owner| {
                        if owner.is_parked() {
                            return Ok(());
                        }
                        let result = owner.park_remote_session();
                        if let Err(error) = &result {
                            tracing::warn!(%error, display_id = owner.id(),
                            "Retained display parking declined or failed; removing the owner before another session");
                        }
                        result
                    },
                    |helper| retire(helper, retained),
                )
            }
        }
    }

    pub(super) fn geometry(&mut self) -> Result<DisplayGeometry> {
        match self {
            Self::Main(owner) => owner.geometry(),
            Self::Virtual { helper, .. } => helper
                .as_mut()
                .context("No remote display exists while disconnected")?
                .geometry(),
        }
    }

    pub(super) fn prepare_capture(&mut self, width: u16, height: u16) -> Result<DisplayGeometry> {
        match self {
            Self::Main(owner) => owner.prepare_capture(width, height),
            Self::Virtual { helper, .. } => helper
                .as_mut()
                .context("Cannot prepare capture before the remote connection begins")?
                .prepare_capture(width, height),
        }
    }

    pub(super) fn prepare_remote_capture(
        &mut self,
        width: u16,
        height: u16,
    ) -> Result<DisplayGeometry> {
        if let Self::Virtual {
            helper,
            identity_slot,
            ..
        } = self
        {
            if helper.is_none() {
                *helper = Some(crate::helper::DisplayHelper::new(
                    width,
                    height,
                    identity_slot,
                )?);
            }
        }
        let result = self
            .begin_remote_session()
            .and_then(|_| self.prepare_capture(width, height));
        if result.is_err() {
            // Failed activation always retires the exact helper. Parking a
            // partially initialized owner would turn an error into idle state.
            if let Err(error) = self.shutdown() {
                tracing::warn!(%error, "Could not fully clean up failed remote display activation");
            }
        }
        result
    }

    pub(super) fn display_id(&self) -> u32 {
        match self {
            Self::Main(owner) => owner.display_id,
            Self::Virtual { helper, .. } => helper.as_ref().map_or(0, |owner| owner.id()),
        }
    }

    pub(super) fn shutdown(&mut self) -> Result<()> {
        match self {
            Self::Main(owner) => owner.end_remote_session().map(|_| ()),
            Self::Virtual {
                helper,
                retain_between_sessions,
                ..
            } => retire(helper, *retain_between_sessions),
        }
    }
}

fn park_or_retire<T>(
    owner: &mut Option<T>,
    retained: bool,
    park: impl FnOnce(&mut T) -> Result<()>,
    retire: impl FnOnce(&mut Option<T>) -> Result<()>,
) -> Result<()> {
    if retained {
        if let Some(owner) = owner.as_mut() {
            if park(owner).is_ok() {
                return Ok(());
            }
        }
    }
    retire(owner)
}

fn retire(helper: &mut Option<crate::helper::DisplayHelper>, retained: bool) -> Result<()> {
    let Some(owner) = helper.take() else {
        return Ok(());
    };
    let display_id = owner.id();
    transition::observed(Phase::OwnerRetire, Details::display(display_id), || {
        retire_owned(
            owner,
            |owner| {
                if owner.is_parked() {
                    Ok(())
                } else {
                    owner.end_remote_session().map(|_| ())
                }
            },
            || wait_for_display_removal(display_id, retained),
        )
    })
}

fn retire_owned<T>(
    mut owner: T,
    restore: impl FnOnce(&mut T) -> Result<()>,
    verify: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let result = restore(&mut owner);
    // Drop closes the owner input and reaps its exact process. A disabled
    // display being absent from public enumeration is not a retirement proof.
    drop(owner);
    let removed = verify();
    result.and(removed)
}

fn wait_for_display_removal(display_id: u32, retained: bool) -> Result<()> {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        // If the private symbols are unavailable no park operation could have
        // hidden a display. Keep ordinary removal usable on that macOS version.
        // A present API returning an enumeration error still fails closed.
        let ids = if retained && crate::activation::available() {
            crate::activation::all_display_ids()?
        } else {
            online_display_ids()?
        };
        if !ids.contains(&display_id) {
            tracing::info!(display_id, "Removed the disconnected remote display");
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Virtual display {display_id} was not removed within 3 seconds"
        );
        std::thread::sleep(SETTLE_POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Owner<'a>(&'a RefCell<Vec<&'static str>>);
    impl Drop for Owner<'_> {
        fn drop(&mut self) {
            self.0.borrow_mut().push("reaped");
        }
    }

    #[test]
    fn removal_is_verified_only_after_the_exact_owner_is_dropped_even_when_restore_fails() {
        let order = RefCell::new(Vec::new());
        let error = retire_owned(
            Owner(&order),
            |_| {
                order.borrow_mut().push("restore");
                bail!("restoration failed")
            },
            || {
                assert_eq!(*order.borrow(), vec!["restore", "reaped"]);
                order.borrow_mut().push("verify");
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "restoration failed");
        assert_eq!(*order.borrow(), vec!["restore", "reaped", "verify"]);
    }

    #[test]
    fn failed_removal_prevents_successful_retirement_acknowledgment() {
        let order = RefCell::new(Vec::new());
        assert!(retire_owned(
            Owner(&order),
            |_| Ok(()),
            || {
                assert_eq!(*order.borrow(), vec!["reaped"]);
                bail!("native slot remains")
            }
        )
        .unwrap_err()
        .to_string()
        .contains("native slot remains"));
    }

    #[test]
    fn semantic_parking_refusal_falls_back_to_full_retirement_without_a_cleanup_error() {
        let order = RefCell::new(Vec::new());
        let mut owner = Some(Owner(&order));
        park_or_retire(
            &mut owner,
            true,
            |_| {
                order.borrow_mut().push("park refused");
                bail!("window evacuation unavailable")
            },
            |owner| {
                retire_owned(
                    owner.take().unwrap(),
                    |_| {
                        order.borrow_mut().push("restore acknowledged");
                        Ok(())
                    },
                    || {
                        assert_eq!(
                            *order.borrow(),
                            vec!["park refused", "restore acknowledged", "reaped"]
                        );
                        order.borrow_mut().push("removed");
                        Ok(())
                    },
                )
            },
        )
        .unwrap();
        assert!(owner.is_none());
        assert_eq!(
            *order.borrow(),
            vec!["park refused", "restore acknowledged", "reaped", "removed"]
        );
    }

    #[test]
    fn successful_parking_preserves_owner_and_never_runs_retirement() {
        let order = RefCell::new(Vec::new());
        let mut owner = Some(Owner(&order));
        park_or_retire(
            &mut owner,
            true,
            |_| Ok(()),
            |_| panic!("parked display must stay allocated"),
        )
        .unwrap();
        assert!(owner.is_some());
        assert!(order.borrow().is_empty());
    }
}

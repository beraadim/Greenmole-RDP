//! Value-only planning for temporary primary-display arrangements.

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Placement {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Placement {
    fn translated(self, x: i32, y: i32) -> Result<Self> {
        Ok(Self {
            x: self
                .x
                .checked_sub(x)
                .context("Display x coordinate overflow")?,
            y: self
                .y
                .checked_sub(y)
                .context("Display y coordinate overflow")?,
            ..self
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    pub primary: u32,
    pub displays: Vec<Placement>,
}

pub(crate) fn validate(displays: &[Placement], primary: u32, virtual_id: u32) -> Result<()> {
    ensure!(
        !displays.is_empty() && displays.len() <= 64,
        "Invalid display layout size"
    );
    let mut ids = std::collections::BTreeSet::new();
    for display in displays {
        ensure!(
            display.id != 0 && ids.insert(display.id),
            "Duplicate or invalid display ID"
        );
        ensure!(
            display.width > 0 && display.height > 0,
            "Invalid display layout dimensions"
        );
        ensure!(
            display.x.checked_add(display.width).is_some()
                && display.y.checked_add(display.height).is_some(),
            "Display layout bounds overflow"
        );
    }
    ensure!(
        ids.contains(&virtual_id),
        "Owned virtual display is absent from layout"
    );
    ensure!(
        displays
            .iter()
            .any(|d| d.id == primary && d.x == 0 && d.y == 0),
        "Primary display must be present at the global origin"
    );
    Ok(())
}

impl Snapshot {
    pub fn promoted(&self, virtual_id: u32) -> Result<Vec<Placement>> {
        let target = self
            .displays
            .iter()
            .find(|d| d.id == virtual_id)
            .context("Owned virtual display is absent from snapshot")?;
        let displays = self
            .displays
            .iter()
            .map(|d| d.translated(target.x, target.y))
            .collect::<Result<Vec<_>>>()?;
        validate(&displays, virtual_id, virtual_id)?;
        Ok(displays)
    }

    /// Preserve exact saved physical origins if topology is unchanged. If a
    /// source screen disappeared, anchor the remaining saved layout to a live
    /// physical primary; macOS resolves any gaps left by unplugged screens.
    pub fn restored(&self, live: &[Placement], virtual_id: u32) -> Result<(u32, Vec<Placement>)> {
        let target = live
            .iter()
            .find(|d| d.id == virtual_id)
            .context("Owned virtual display disappeared during restoration")?;
        let primary = live
            .iter()
            .find(|d| d.id == self.primary && d.id != virtual_id)
            .or_else(|| live.iter().find(|d| d.id != virtual_id))
            .context("No physical display remains for layout restoration")?
            .id;
        let anchor = self
            .displays
            .iter()
            .find(|d| d.id == primary)
            .or_else(|| live.iter().find(|d| d.id == primary))
            .unwrap();
        let live_anchor = live.iter().find(|d| d.id == primary).unwrap();
        let mut displays = live
            .iter()
            .filter(|d| d.id != virtual_id)
            .map(|current| {
                let saved = self.displays.iter().find(|saved| saved.id == current.id);
                let (source, origin) = saved
                    .map(|saved| (saved, anchor))
                    .unwrap_or((current, live_anchor));
                Placement {
                    width: current.width,
                    height: current.height,
                    ..*source
                }
                .translated(origin.x, origin.y)
            })
            .collect::<Result<Vec<_>>>()?;
        // The virtual resolution may differ from the snapshot. Park it beside
        // the leftmost live physical screen without overlapping that screen.
        let leftmost = displays.iter().min_by_key(|d| d.x).unwrap();
        displays.push(Placement {
            x: leftmost
                .x
                .checked_sub(target.width)
                .context("Virtual parking origin overflow")?,
            y: leftmost.y,
            ..*target
        });
        validate(&displays, primary, virtual_id)?;
        Ok((primary, displays))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn screen(id: u32, x: i32, y: i32, width: i32, height: i32) -> Placement {
        Placement {
            id,
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn promotion_preserves_relative_layout_with_negative_origins() {
        let snapshot = Snapshot {
            primary: 1,
            displays: vec![
                screen(1, 0, 0, 2560, 1600),
                screen(2, -1280, 120, 1280, 720),
            ],
        };
        let promoted = snapshot.promoted(2).unwrap();
        assert_eq!(
            promoted,
            vec![
                screen(1, 1280, -120, 2560, 1600),
                screen(2, 0, 0, 1280, 720)
            ]
        );
    }

    #[test]
    fn restore_keeps_physical_origins_after_virtual_resize() {
        let snapshot = Snapshot {
            primary: 1,
            displays: vec![screen(1, 0, 0, 2560, 1600), screen(2, -1280, 0, 1280, 720)],
        };
        let live = vec![screen(1, 1920, 0, 2560, 1600), screen(2, 0, 0, 1920, 1080)];
        let (primary, restored) = snapshot.restored(&live, 2).unwrap();
        assert_eq!(primary, 1);
        assert_eq!(
            restored,
            vec![screen(1, 0, 0, 2560, 1600), screen(2, -1920, 0, 1920, 1080)]
        );
    }

    #[test]
    fn retained_activation_uses_the_physical_layout_saved_before_enabling() {
        // The user selected a different physical primary and arranged screens
        // while the virtual display was disabled. Enabling can revive another
        // remembered arrangement; it must not replace this session's baseline.
        let before_enable = Snapshot {
            primary: 3,
            displays: vec![
                screen(1, -2560, 120, 2560, 1600),
                screen(3, 0, 0, 1920, 1080),
                screen(4, 1920, -200, 2560, 1440),
            ],
        };
        let remembered = vec![
            screen(1, 0, 0, 2560, 1600),
            screen(2, -1280, 0, 1280, 720),
            screen(3, 2560, 0, 1920, 1080),
            screen(4, 4480, 0, 2560, 1440),
        ];
        let (primary, restored) = before_enable.restored(&remembered, 2).unwrap();
        assert_eq!(primary, 3);
        assert_eq!(&restored[..3], before_enable.displays.as_slice());
        assert_eq!(restored[3], screen(2, -3840, 120, 1280, 720));
        let session = Snapshot {
            primary,
            displays: restored.clone(),
        };
        let promoted = session.promoted(2).unwrap();
        let (restored_primary, after_session) = session.restored(&promoted, 2).unwrap();
        assert_eq!(restored_primary, primary);
        assert_eq!(after_session, restored);
    }

    #[test]
    fn missing_primary_returns_to_remaining_physical_screen() {
        let snapshot = Snapshot {
            primary: 1,
            displays: vec![
                screen(1, 0, 0, 2560, 1600),
                screen(2, -1280, 0, 1280, 720),
                screen(3, 2560, -200, 1920, 1080),
            ],
        };
        let live = vec![
            screen(2, 0, 0, 1280, 720),
            screen(3, 3840, -200, 1920, 1080),
        ];
        let (primary, restored) = snapshot.restored(&live, 2).unwrap();
        assert_eq!(primary, 3);
        assert_eq!(
            restored,
            vec![screen(3, 0, 0, 1920, 1080), screen(2, -1280, 0, 1280, 720)]
        );
        assert!(snapshot.restored(&live[..1], 2).is_err());
    }

    #[test]
    fn rejects_duplicate_ids_and_coordinate_overflow() {
        assert!(validate(&[screen(2, 0, 0, 1, 1), screen(2, 1, 0, 1, 1)], 2, 2).is_err());
        assert!(validate(&[screen(2, 0, 0, 1, 1), screen(1, i32::MAX, 0, 1, 1)], 2, 2).is_err());
    }
}

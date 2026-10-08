use anyhow::{bail, Context, Result};

use crate::DisplayGeometry;

/// Resolve output pixels from the selected display, never a different physical monitor.
/// Explicit width/height take precedence over automatic mode; legacy scale values
/// remain based on logical points. Backend limits are applied by the controller.
pub fn resolve_desktop_size(
    mode: &str,
    geometry: &DisplayGeometry,
    configured_width: u32,
    configured_height: u32,
) -> Result<(u16, u16)> {
    let dimensions = if mode == "auto" {
        match (configured_width, configured_height) {
            (0, 0) => (geometry.pixel_width, geometry.pixel_height),
            (0, _) | (_, 0) => bail!("Configure both width and height, or leave both at zero"),
            size => size,
        }
    } else if let Some((width, height)) = mode.split_once('x') {
        (
            width.parse().context("Invalid resolution width")?,
            height.parse().context("Invalid resolution height")?,
        )
    } else if let Ok(scale) = mode.parse::<u32>() {
        if !(1..=4).contains(&scale) {
            bail!("Legacy display scale must be between 1 and 4");
        }
        let width = geometry.logical_width * f64::from(scale);
        let height = geometry.logical_height * f64::from(scale);
        if !width.is_finite()
            || !height.is_finite()
            || width < 1.0
            || height < 1.0
            || width > f64::from(u16::MAX)
            || height > f64::from(u16::MAX)
        {
            bail!("Scaled display size is outside RDP bounds");
        }
        (width.round() as u32, height.round() as u32)
    } else {
        bail!("Resolution must be auto, WIDTHxHEIGHT, or a legacy scale from 1 to 4");
    };
    let width = u16::try_from(dimensions.0).context("Resolution width exceeds RDP bounds")?;
    let height = u16::try_from(dimensions.1).context("Resolution height exceeds RDP bounds")?;
    if width == 0 || height == 0 {
        bail!("Resolution dimensions must be greater than zero");
    }
    Ok((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display() -> DisplayGeometry {
        DisplayGeometry {
            display_id: 42,
            origin_x: -1920.0,
            origin_y: 100.0,
            logical_width: 1920.0,
            logical_height: 1080.0,
            pixel_width: 3840,
            pixel_height: 2160,
        }
    }

    #[test]
    fn automatic_size_uses_selected_display_pixels_and_explicit_overrides() {
        assert_eq!(
            resolve_desktop_size("auto", &display(), 0, 0).unwrap(),
            (3840, 2160)
        );
        assert_eq!(
            resolve_desktop_size("auto", &display(), 1600, 900).unwrap(),
            (1600, 900)
        );
        assert_eq!(
            resolve_desktop_size("1280x720", &display(), 0, 0).unwrap(),
            (1280, 720)
        );
        assert_eq!(
            resolve_desktop_size("1", &display(), 0, 0).unwrap(),
            (1920, 1080)
        );
    }

    #[test]
    fn invalid_sizes_are_rejected_before_casting_or_allocating() {
        for mode in [
            "0x1080",
            "1920x0",
            "65536x1080",
            "4294967296x1",
            "garbage",
            "0",
            "5",
        ] {
            assert!(
                resolve_desktop_size(mode, &display(), 0, 0).is_err(),
                "{mode}"
            );
        }
        assert!(resolve_desktop_size("auto", &display(), 0, 100).is_err());
        assert!(resolve_desktop_size("auto", &display(), 65536, 100).is_err());
    }
}

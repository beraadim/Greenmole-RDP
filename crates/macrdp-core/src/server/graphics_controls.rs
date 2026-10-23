//! Publish a profile under the existing graphics mutex, then wake once. This
//! changes only the same live targets as individual controls; negotiated chroma
//! and native capture/display ownership continue following their existing policy.
use crate::GraphicsSettings;
use ironrdp_server::{gfx::GfxState, BitmapQuality};

pub(super) fn apply(state: &mut GfxState, settings: &GraphicsSettings) -> Option<BitmapQuality> {
    let quality_changed = state.target_quality.as_deref() != Some(settings.quality.as_str());
    let changed = quality_changed
        || state.target_frame_rate != Some(settings.frame_rate)
        || state.target_bitrate != Some(settings.bitrate_mbps * 1_000_000)
        || state.encoder_pref.as_deref() != Some(settings.encoder.as_str());
    state.target_frame_rate = Some(settings.frame_rate);
    state.target_bitrate = Some(settings.bitrate_mbps * 1_000_000);
    state.target_quality = Some(settings.quality.clone());
    state.manual_bitmap_mode = settings.encoder == "bitmap";
    state.encoder_pref = Some(settings.encoder.clone());
    // This field is read by the existing next-connection negotiation, never used
    // to replace the active connection's agreed AVC420/AVC444 format.
    state.chroma_mode = Some(settings.chroma_mode.clone());
    if changed {
        state.notify_controls_changed();
    }
    quality_changed
        .then(|| BitmapQuality::from_config(&settings.quality).expect("validated graphics profile"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> GraphicsSettings {
        GraphicsSettings {
            frame_rate: 30,
            bitrate_mbps: 8,
            quality: "balanced".into(),
            encoder: "auto".into(),
            chroma_mode: "avc420".into(),
        }
    }

    #[test]
    fn grouped_targets_wake_once_without_touching_negotiation_or_display() {
        let mut state = GfxState::new(1920, 1080, false, false);
        let mut changes = state.controls_receiver();
        state.session_uses_gfx = true;
        state.total_bytes_sent = 123;
        let settings = profile();
        assert!(apply(&mut state, &settings).is_some());
        assert!(changes.has_changed().unwrap());
        changes.borrow_and_update();
        assert_eq!(state.target_frame_rate, Some(30));
        assert_eq!(state.target_bitrate, Some(8_000_000));
        assert_eq!(state.target_quality.as_deref(), Some("balanced"));
        assert_eq!(state.encoder_pref.as_deref(), Some("auto"));
        assert!(!state.manual_bitmap_mode);
        assert!(state.session_uses_gfx);
        assert_eq!(state.total_bytes_sent, 123);
        assert!(apply(&mut state, &settings).is_none());
        assert!(!changes.has_changed().unwrap());
    }

    #[test]
    fn explicit_bitmap_and_chroma_follow_existing_targets() {
        let mut state = GfxState::new(1920, 1080, false, false);
        let mut settings = profile();
        settings.encoder = "bitmap".into();
        settings.quality = "lossless".into();
        settings.chroma_mode = "avc444".into();
        apply(&mut state, &settings);
        assert!(state.manual_bitmap_mode);
        assert_eq!(state.chroma_mode.as_deref(), Some("avc444"));
        assert_eq!(state.target_quality.as_deref(), Some("lossless"));
    }
}

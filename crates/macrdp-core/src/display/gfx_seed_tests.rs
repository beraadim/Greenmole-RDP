use super::*;

#[test]
fn seed_policy_distinguishes_initial_rgb_from_opaque_hardware_recovery() {
    assert!(raw_seed_required(true, false, true, false, false));
    assert!(raw_seed_required(true, false, true, true, false));
    assert!(!raw_seed_required(true, false, true, true, true));
    assert!(raw_seed_required(true, false, false, true, true));
    assert!(raw_seed_required(true, false, true, false, true));
    assert!(!raw_seed_required(true, true, true, false, false));
    assert!(!raw_seed_required(true, true, true, true, true));
    assert!(!raw_seed_required(false, false, true, false, false));
}

#[test]
fn full_rgb_seed_validates_visible_layout_before_materializing() {
    assert!(full_bgra_update(&[], 0, 1, 0).is_none());
    assert!(full_bgra_update(&[], 1, 0, 4).is_none());
    assert!(full_bgra_update(&[0; 16], 2, 2, 7).is_none());
    assert!(full_bgra_update(&[0; 15], 2, 2, 8).is_none());
    assert!(full_bgra_update(&[], u32::MAX, 1, usize::MAX).is_none());
    assert!(full_bgra_update(&[], 1, 2, usize::MAX).is_none());

    // Trailing row padding is optional; the complete visible rectangle is not.
    let pixels = [7u8; 20];
    let update = full_bgra_update(&pixels, 2, 2, 12).unwrap();
    assert_eq!((update.width, update.height), (2, 2));
    assert_eq!(update.rects.len(), 1);
    assert_eq!(update.rects[0].pixel_data.as_ref(), &[7; 16]);
}

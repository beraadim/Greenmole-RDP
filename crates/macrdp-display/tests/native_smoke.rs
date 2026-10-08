//! Opt-in native validation: creates and removes temporary extended displays.
//! Run this single test in a logged-in macOS session with a stable display layout.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Native display smoke test skipped: macOS is required");
}

#[cfg(target_os = "macos")]
fn main() {
    // Must precede every CoreGraphics query, including the test's own baseline.
    if let Some(result) = macrdp_display::run_display_helper_if_requested() {
        if let Err(error) = result {
            eprintln!("Display helper failed: {error:#}");
            std::process::exit(1);
        }
        std::process::exit(0);
    }
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args
        .first()
        .is_some_and(|arg| arg == "--macrdp-display-observer")
    {
        assert_eq!(args.len(), 2, "observer requires one request");
        native::observe(&args[1]);
        return;
    }
    if args
        .first()
        .is_some_and(|arg| arg == "--macrdp-display-inventory-observer")
    {
        assert_eq!(args.len(), 2, "inventory observer requires one request");
        native::observe_inventory(&args[1]);
        return;
    }
    if args
        .first()
        .is_some_and(|arg| arg == "--macrdp-display-layout-observer")
    {
        assert_eq!(args.len(), 2, "layout observer requires one request");
        native::observe_layout(&args[1]);
        return;
    }
    if args
        .first()
        .is_some_and(|arg| arg == "--macrdp-display-removal-observer")
    {
        assert_eq!(args.len(), 2, "removal observer requires one display ID");
        native::observe_removal(args[1].parse().expect("invalid display ID"));
        return;
    }
    if !args.iter().any(|arg| arg == "--ignored") {
        eprintln!("Native display smoke test skipped; pass --ignored to create temporary extended displays");
        return;
    }
    native::run();
}

#[cfg(target_os = "macos")]
mod native {

    use core_graphics::display::CGDisplay;
    use macrdp_display::{DisplayBackend, DisplayController, DisplayGeometry};
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGGetOnlineDisplayList(
            max_displays: u32,
            online_displays: *mut u32,
            display_count: *mut u32,
        ) -> i32;
    }

    fn observe_from_fresh_process(display_id: u32, expected: (u64, u64, u64, u64)) {
        // Tahoe may omit newly created displays from the creator's mode cache.
        // A fresh process independently checks the actual backing pixel mode;
        // it never creates a display, applies settings, or injects input.
        let result = Command::new(std::env::current_exe().unwrap())
            .arg("--macrdp-display-observer")
            .arg(serde_json::to_string(&(display_id, expected)).unwrap())
            .output()
            .expect("could not start read-only native mode observer");
        assert!(
            result.status.success(),
            "fresh mode observer failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }

    pub fn observe(request: &str) {
        let (display_id, expected): (u32, (u64, u64, u64, u64)) =
            serde_json::from_str(request).expect("invalid native observer request");
        assert!(CGDisplay::active_displays().unwrap().contains(&display_id));
        assert_eq!(native_mode_size(display_id), expected);
    }

    fn observe_layout_from_fresh_process(primary: u32, expected: DisplayGeometry) {
        let result = Command::new(std::env::current_exe().unwrap())
            .arg("--macrdp-display-layout-observer")
            .arg(serde_json::to_string(&(primary, expected)).unwrap())
            .output()
            .expect("could not start read-only layout observer");
        assert!(
            result.status.success(),
            "fresh layout observer failed:\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    pub fn observe_layout(request: &str) {
        let (primary, expected): (u32, DisplayGeometry) = serde_json::from_str(request).unwrap();
        assert!(CGDisplay::active_displays()
            .unwrap()
            .contains(&expected.display_id));
        assert_eq!(CGDisplay::main().id, primary);
        let bounds = CGDisplay::new(expected.display_id).bounds();
        assert_eq!(
            (bounds.origin.x, bounds.origin.y),
            (expected.origin_x, expected.origin_y)
        );
        assert_eq!(
            (bounds.size.width, bounds.size.height),
            (expected.logical_width, expected.logical_height)
        );
        let mode = CGDisplay::new(expected.display_id).display_mode().unwrap();
        assert_eq!(
            (mode.pixel_width(), mode.pixel_height()),
            (
                u64::from(expected.pixel_width),
                u64::from(expected.pixel_height)
            )
        );
    }

    fn observe_inventory_from_fresh_process(expected: &[u32]) {
        let result = Command::new(std::env::current_exe().unwrap())
            .arg("--macrdp-display-inventory-observer")
            .arg(serde_json::to_string(expected).unwrap())
            .output()
            .expect("could not start read-only inventory observer");
        assert!(
            result.status.success(),
            "fresh inventory observer failed:\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    pub fn observe_inventory(request: &str) {
        let mut expected: Vec<u32> = serde_json::from_str(request).unwrap();
        let mut active = CGDisplay::active_displays().unwrap();
        active.sort_unstable();
        expected.sort_unstable();
        assert_eq!(
            active, expected,
            "an idle controller added an active display"
        );
    }

    pub fn observe_removal(display_id: u32) {
        let active = CGDisplay::active_displays().expect("could not enumerate active displays");
        let mut online = [0u32; 64];
        let mut count = 0;
        // SAFETY: both output pointers reference writable buffers with the
        // advertised capacity. Query membership rather than flags for a stale ID:
        // CoreGraphics can report true flags for an ID absent from both lists.
        let status =
            unsafe { CGGetOnlineDisplayList(online.len() as u32, online.as_mut_ptr(), &mut count) };
        assert_eq!(status, 0, "could not enumerate online displays");
        assert!(
            (count as usize) < online.len(),
            "online display list may be truncated"
        );
        let online = &online[..count as usize];
        assert!(
            !active.contains(&display_id) && !online.contains(&display_id),
            "virtual display {display_id} remains present: active IDs={active:?}, online IDs={online:?}"
        );
    }

    fn native_mode_size(display_id: u32) -> (u64, u64, u64, u64) {
        let mode = CGDisplay::new(display_id)
            .display_mode()
            .expect("selected display must have a native mode");
        (
            mode.width(),
            mode.height(),
            mode.pixel_width(),
            mode.pixel_height(),
        )
    }

    fn assert_virtual_mode(
        controller: &DisplayController,
        geometry: DisplayGeometry,
        display_id: u32,
        pixels: (u16, u16),
    ) {
        let display = CGDisplay::new(display_id);
        assert_eq!(geometry.display_id, display_id, "resize recreated display");
        assert!(display.is_online());
        assert!(display.is_active());
        assert!(!display.is_in_mirror_set());
        assert_eq!(
            (geometry.pixel_width, geometry.pixel_height),
            (u32::from(pixels.0), u32::from(pixels.1))
        );
        assert_eq!(
            (geometry.logical_width, geometry.logical_height),
            (f64::from(pixels.0), f64::from(pixels.1))
        );
        observe_from_fresh_process(
            display_id,
            (
                u64::from(pixels.0),
                u64::from(pixels.1),
                u64::from(pixels.0),
                u64::from(pixels.1),
            ),
        );
        let bounds = display.bounds();
        assert!(geometry.origin_x.is_finite() && geometry.origin_y.is_finite());
        assert_eq!(geometry.origin_x, bounds.origin.x);
        assert_eq!(geometry.origin_y, bounds.origin.y);
        assert_eq!(geometry.logical_width, bounds.size.width);
        assert_eq!(geometry.logical_height, bounds.size.height);
        assert_eq!(controller.geometry().unwrap(), geometry);
        eprintln!("Validated virtual display: {geometry:?}");
    }

    fn assert_display_removed(display_id: u32) {
        // WindowServer can finish removal asynchronously, and the test process's
        // CoreGraphics cache can retain the old online flag even after removal.
        // Each read-only observer starts with fresh display state.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let result = Command::new(std::env::current_exe().unwrap())
                .arg("--macrdp-display-removal-observer")
                .arg(display_id.to_string())
                .output()
                .expect("could not start read-only native removal observer");
            if result.status.success() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "virtual display {display_id} remained online after its final controller was dropped:\n{}",
                String::from_utf8_lossy(&result.stderr)
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    pub fn run() {
        // One test keeps all topology changes serial. This does not capture screens,
        // inject input, launch a server, or change any physical display's mode.
        let main_id = CGDisplay::main().id;
        assert_ne!(
            main_id, 0,
            "a logged-in session with a main display is required"
        );
        let main_mode = native_mode_size(main_id);
        let original_ids = CGDisplay::active_displays().unwrap();
        let main = DisplayController::new(DisplayBackend::default(), 641, 481).unwrap();
        assert_eq!(main.backend(), DisplayBackend::Main);
        let original_main = main.geometry().unwrap();
        assert_eq!(original_main.display_id, main_id);
        for size in [(641, 481), (3840, 2160), (320, 200)] {
            assert_eq!(main.prepare_capture(size.0, size.1).unwrap(), original_main);
            assert_eq!(native_mode_size(main_id), main_mode);
        }

        {
            let controller = DisplayController::new(DisplayBackend::Virtual, 1920, 1080).unwrap();
            assert!(
                controller.geometry().is_err(),
                "idle virtual controller exposed capture geometry"
            );
            observe_inventory_from_fresh_process(&original_ids);
            assert_eq!(CGDisplay::main().id, main_id);
            let geometry = controller
                .queue_remote_capture(1920, 1080)
                .unwrap()
                .wait()
                .unwrap();
            let display_id = geometry.display_id;
            assert_ne!(display_id, main_id);
            assert_virtual_mode(&controller, geometry, display_id, (1920, 1080));

            let remaining = controller.clone();
            drop(controller);
            assert_virtual_mode(
                &remaining,
                remaining.geometry().unwrap(),
                display_id,
                (1920, 1080),
            );

            let sizes = [
                ((1280, 720), (1280, 720)),
                ((1921, 1081), (1920, 1080)),
                ((1922, 1082), (1920, 1080)),
                ((1928, 1088), (1928, 1088)),
                ((1936, 1096), (1936, 1096)),
                ((2560, 1305), (2560, 1304)),
                ((2560, 1440), (2560, 1440)),
                ((3840, 2160), (3840, 2160)),
                ((320, 200), (320, 200)),
                ((1280, 720), (1280, 720)),
            ];
            let remote_geometry = remaining.begin_remote_session().unwrap();
            assert_eq!(CGDisplay::main().id, display_id);
            assert_eq!(
                (remote_geometry.origin_x, remote_geometry.origin_y),
                (0.0, 0.0)
            );
            assert_eq!(remaining.begin_remote_session().unwrap(), remote_geometry);
            observe_layout_from_fresh_process(display_id, remote_geometry);
            for (requested, expected) in sizes {
                let geometry = remaining
                    .queue_remote_capture(requested.0, requested.1)
                    .unwrap()
                    .wait()
                    .unwrap();
                assert_virtual_mode(&remaining, geometry, display_id, expected);
                assert_eq!(
                    CGDisplay::main().id,
                    display_id,
                    "remote primary display changed"
                );
                assert_eq!((geometry.origin_x, geometry.origin_y), (0.0, 0.0));
                assert_eq!(
                    native_mode_size(main_id),
                    main_mode,
                    "physical mode changed"
                );
                assert_eq!(main.geometry().unwrap().display_id, main_id);
                observe_layout_from_fresh_process(display_id, geometry);
                observe_from_fresh_process(main_id, main_mode);
            }

            remaining.end_remote_session().unwrap();
            assert_display_removed(display_id);
            assert!(remaining.geometry().is_err());
            observe_inventory_from_fresh_process(&original_ids);
            remaining.end_remote_session().unwrap();
            assert_eq!(CGDisplay::main().id, main_id);
            assert_eq!(
                main.geometry().unwrap(),
                original_main,
                "physical geometry was not restored"
            );
            assert_eq!(native_mode_size(main_id), main_mode);
            observe_layout_from_fresh_process(main_id, original_main);

            // Owner EOF/drop must restore the primary even if graceful session
            // cleanup was skipped. No physical display mode may be changed.
            let reconnect = remaining
                .queue_remote_capture(1280, 720)
                .unwrap()
                .wait()
                .unwrap();
            assert_eq!(CGDisplay::main().id, reconnect.display_id);
            assert_virtual_mode(&remaining, reconnect, reconnect.display_id, (1280, 720));

            drop(remaining);
            assert_display_removed(reconnect.display_id);
            observe_inventory_from_fresh_process(&original_ids);
            assert_eq!(CGDisplay::main().id, main_id);
            assert_eq!(native_mode_size(main_id), main_mode);
            assert_eq!(main.geometry().unwrap(), original_main);
            observe_layout_from_fresh_process(main_id, original_main);
            eprintln!("Virtual display {display_id} released cleanly");
        }

        drop(main);
        assert!(CGDisplay::new(main_id).is_online());
        assert_eq!(native_mode_size(main_id), main_mode);
    }
}

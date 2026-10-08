use super::*;

/// This test never starts the all-application worker. The explicitly supplied
/// PID must belong to a disposable fixture application with one ordinary window.
/// A caller must also provide an already-running, disposable virtual display.
#[test]
#[ignore = "requires a disposable GUI fixture, virtual display, and Accessibility permission"]
fn controlled_fixture_moves_and_restores() {
    assert!(
        accessibility::trusted(),
        "Native fixture test needs Accessibility permission; no windows were changed"
    );
    let pid: i32 = std::env::var("MACRDP_WINDOW_FIXTURE_PID")
        .expect("Set MACRDP_WINDOW_FIXTURE_PID to a disposable fixture app PID")
        .parse()
        .expect("Fixture PID must be numeric");
    let display_id: u32 = std::env::var("MACRDP_WINDOW_FIXTURE_DISPLAY_ID")
        .expect("Set MACRDP_WINDOW_FIXTURE_DISPLAY_ID to the disposable virtual display")
        .parse()
        .expect("Fixture display ID must be numeric");
    assert!(pid > 0);
    assert_ne!(
        display_id,
        CGDisplay::main().id,
        "Fixture target must not be the main physical display"
    );
    let bounds = CGDisplay::new(display_id).bounds();
    let target = DisplayGeometry {
        display_id,
        origin_x: bounds.origin.x,
        origin_y: bounds.origin.y,
        logical_width: bounds.size.width,
        logical_height: bounds.size.height,
        pixel_width: bounds.size.width as u32,
        pixel_height: bounds.size.height as u32,
    };
    assert!(Frame::from(target).valid());

    let element = accessibility::windows(pid)
        .expect("Read disposable fixture windows")
        .into_iter()
        .find(|window| matches!(window.movable(), Ok(Some(_))))
        .expect("Fixture must contain one ordinary movable window");
    let original = element.frame().expect("Read fixture original frame");
    let resizable = element.movable().unwrap().unwrap();
    assert_eq!(
        original.intersection(Frame::from(target)),
        0.0,
        "Fixture must start on a physical display"
    );
    // A second retained reference restores the fixture even if an assertion
    // fails after a partially successful AX write. No other PID is ever touched.
    struct RestoreFixture {
        window: Element,
        frame: Frame,
        resizable: bool,
    }
    impl Drop for RestoreFixture {
        fn drop(&mut self) {
            let _ = self.window.set_frame(self.frame, self.resizable);
        }
    }
    let fixture = accessibility::windows(pid)
        .unwrap()
        .into_iter()
        .find(|window| window.same(&element))
        .expect("Fixture AX identity is stable");
    let cleanup = RestoreFixture {
        window: fixture,
        frame: original,
        resizable,
    };
    let shared = Shared::default();
    shared.lock().desired.target = Some(target);
    let mut session = Session::new(target).unwrap();
    // Disable global discovery entirely: only the supplied fixture is queued.
    session.next_discovery = Instant::now() + Duration::from_secs(3600);
    session.pending.push_back((pid, element));
    for _ in 0..3 {
        session.reconcile(&shared, shared.desired());
        if session
            .records
            .first()
            .map(|record| record.settled)
            .unwrap_or(false)
        {
            break;
        }
    }
    assert_eq!(session.records.len(), 1);
    assert!(
        session.records[0].settled,
        "Fixture did not accept the target frame"
    );
    assert!(cleanup
        .window
        .frame()
        .unwrap()
        .fits_inside(Frame::from(target)));
    assert_eq!(session.restore(), 0, "Fixture restoration failed");
    assert!(cleanup.window.frame().unwrap().close_to(original));
}

//! These tests use in-memory handles only. They must never move live windows,
//! request Accessibility permission, or create/reconfigure a real display.
use super::engine::*;
use crate::manual_windows::geometry::Frame;
use crate::window_move::Budget;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Clone)]
struct FakeWindow(Rc<RefCell<State>>);

struct State {
    id: u32,
    frame: Frame,
    movable: bool,
    resizable: bool,
    closed: bool,
    fail_write_after_resize: bool,
    writes: usize,
    resize_writes: usize,
    inspected_deadlines: Vec<Instant>,
}

impl Window for FakeWindow {
    type Error = &'static str;
    fn same_window(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
    fn inspect(&mut self, budget: &Budget) -> Result<Option<(Frame, bool)>, Self::Error> {
        assert!(
            !budget.expired(),
            "expired phase must not perform a native operation"
        );
        let mut state = self.0.borrow_mut();
        state.inspected_deadlines.push(budget.deadline);
        if state.closed {
            return Err("closed handle");
        }
        Ok(state.movable.then_some((state.frame, state.resizable)))
    }
    fn set_frame(
        &mut self,
        frame: Frame,
        resizable: bool,
        budget: &Budget,
    ) -> Result<(), Self::Error> {
        assert!(!budget.expired());
        let mut state = self.0.borrow_mut();
        state.writes += 1;
        if resizable {
            state.resize_writes += 1;
            state.frame.width = frame.width;
            state.frame.height = frame.height;
        }
        if state.fail_write_after_resize {
            return Err("position rejected");
        }
        state.frame.x = frame.x;
        state.frame.y = frame.y;
        Ok(())
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> Frame {
    Frame {
        x,
        y,
        width,
        height,
    }
}
fn screen(id: u8, x: f64, width: f64, primary: bool) -> Screen {
    Screen {
        key: DisplayKey([id; 16]),
        frame: rect(x, 0.0, width, 1080.0),
        primary,
    }
}
fn topology() -> Vec<Screen> {
    vec![
        screen(1, 0.0, 1920.0, true),
        screen(2, -1600.0, 1600.0, false),
        screen(3, 1920.0, 2560.0, false),
    ]
}
fn budget() -> Budget {
    Budget::for_duration(Duration::from_secs(3))
}
fn expired() -> Budget {
    let mut budget = budget();
    budget.deadline = Instant::now() - Duration::from_secs(1);
    budget
}
fn fake(id: u32, frame: Frame) -> FakeWindow {
    FakeWindow(Rc::new(RefCell::new(State {
        id,
        frame,
        movable: true,
        resizable: true,
        closed: false,
        fail_write_after_resize: false,
        writes: 0,
        resize_writes: 0,
        inspected_deadlines: Vec::new(),
    })))
}
fn snapshot(windows: &[FakeWindow]) -> Session<FakeWindow> {
    let mut session = Session::new(topology());
    let mut report = PhaseReport::default();
    for window in windows {
        session.capture_window(window.clone(), &budget(), &mut report);
    }
    assert_eq!(report.saved, windows.len());
    session
}

#[test]
fn three_monitors_gather_only_secondaries_and_restore_every_original_frame() {
    let originals = [
        rect(80.0, 100.0, 1000.0, 700.0),
        rect(-1500.0, 70.0, 1300.0, 850.0),
        rect(2100.0, 200.0, 1800.0, 800.0),
    ];
    let windows: Vec<_> = originals
        .iter()
        .enumerate()
        .map(|(i, frame)| fake(i as u32, *frame))
        .collect();
    let mut session = snapshot(&windows);
    let remote = rect(0.0, 0.0, 1280.0, 720.0);
    // Emulate the primary window following macOS's primary-screen switch.
    windows[0].0.borrow_mut().frame = rect(40.0, 50.0, 900.0, 600.0);
    let report = session.gather(remote, &budget());
    assert_eq!((report.moved, report.skipped), (2, 1));
    assert_eq!(
        windows[0].0.borrow().writes,
        0,
        "original primary must never be forcibly gathered"
    );
    for window in &windows[1..] {
        assert!(window.0.borrow().frame.fits_inside(remote));
        assert!(window.0.borrow().frame.y >= 32.0);
    }
    let restored = session.restore(&topology(), &budget());
    assert_eq!(
        (restored.moved, restored.failed, restored.fallback),
        (3, 0, 0)
    );
    for (window, original) in windows.iter().zip(originals) {
        assert_eq!(window.0.borrow().frame, original);
    }
}

#[test]
fn reconnect_and_resize_never_overwrite_the_pre_session_geometry() {
    let original = rect(-1500.0, 80.0, 1200.0, 800.0);
    let window = fake(1, original);
    let mut session = snapshot(&[window.clone()]);
    session.gather(rect(0.0, 0.0, 800.0, 600.0), &budget());
    let user_resize = rect(50.0, 40.0, 500.0, 400.0);
    window.0.borrow_mut().frame = user_resize;
    assert_eq!(
        session
            .gather(rect(0.0, 0.0, 640.0, 480.0), &budget())
            .inspected,
        0
    );
    assert_eq!(window.0.borrow().frame, user_resize);
    session.restore(&topology(), &budget());
    assert_eq!(window.0.borrow().frame, original);
    let writes = window.0.borrow().writes;
    assert_eq!(session.restore(&topology(), &budget()).inspected, 0);
    assert_eq!(
        session
            .gather(rect(0.0, 0.0, 800.0, 600.0), &budget())
            .inspected,
        0
    );
    assert_eq!(
        window.0.borrow().writes,
        writes,
        "disconnect and teardown are idempotent"
    );
}

#[test]
fn failed_partial_gather_keeps_snapshot_for_disconnect_recovery() {
    let original = rect(-1500.0, 100.0, 1300.0, 900.0);
    let window = fake(1, original);
    let mut session = snapshot(&[window.clone()]);
    window.0.borrow_mut().fail_write_after_resize = true;
    assert_eq!(
        session
            .gather(rect(0.0, 0.0, 640.0, 480.0), &budget())
            .failed,
        1
    );
    assert_ne!(
        window.0.borrow().frame,
        original,
        "simulate AX resize succeeding before position fails"
    );
    window.0.borrow_mut().fail_write_after_resize = false;
    assert_eq!(session.restore(&topology(), &budget()).moved, 1);
    assert_eq!(window.0.borrow().frame, original);
}

#[test]
fn closed_windows_and_new_windows_with_reused_process_metadata_are_never_confused() {
    let original = rect(-1500.0, 80.0, 800.0, 600.0);
    let old = fake(42, original);
    let mut session = snapshot(&[old.clone()]);
    session.gather(rect(0.0, 0.0, 1280.0, 720.0), &budget());
    old.0.borrow_mut().closed = true;
    // The numeric identity intentionally matches. Only the retained object is
    // available to restoration; a replacement window cannot be rediscovered.
    let replacement = fake(42, rect(100.0, 50.0, 700.0, 500.0));
    assert_eq!(old.0.borrow().id, replacement.0.borrow().id);
    assert_eq!(session.restore(&topology(), &budget()).failed, 1);
    assert_eq!(replacement.0.borrow().writes, 0);
}

#[test]
fn fresh_phase_deadlines_and_expired_work_never_reuse_capture_budget() {
    let window = fake(1, rect(-1500.0, 80.0, 800.0, 600.0));
    let mut session = Session::new(topology());
    let capture_budget = budget();
    session.capture_window(window.clone(), &capture_budget, &mut PhaseReport::default());
    let mut gather_budget = budget();
    gather_budget.deadline = capture_budget.deadline + Duration::from_secs(10);
    session.gather(rect(0.0, 0.0, 1280.0, 720.0), &gather_budget);
    let mut restore_budget = budget();
    restore_budget.deadline = gather_budget.deadline + Duration::from_secs(10);
    session.restore(&topology(), &restore_budget);
    assert_eq!(
        window.0.borrow().inspected_deadlines,
        vec![
            capture_budget.deadline,
            gather_budget.deadline,
            restore_budget.deadline
        ]
    );

    let mut expired_session = snapshot(&[window.clone()]);
    let writes = window.0.borrow().writes;
    assert!(
        expired_session
            .gather(rect(0.0, 0.0, 1280.0, 720.0), &expired())
            .timed_out
    );
    assert!(expired_session.restore(&topology(), &expired()).timed_out);
    assert_eq!(window.0.borrow().writes, writes);
    // A completed/expired disconnect must not execute stale deferred writes.
    assert_eq!(expired_session.restore(&topology(), &budget()).inspected, 0);
}

#[test]
fn disappeared_monitor_falls_back_safely_and_changed_origin_is_reanchored() {
    let original = rect(-1500.0, 80.0, 1200.0, 800.0);
    let window = fake(1, original);
    let mut session = snapshot(&[window.clone()]);
    session.gather(rect(0.0, 0.0, 640.0, 480.0), &budget());
    // Different UUID at the same display bounds must not be mistaken for the
    // original monitor, even if CoreGraphics recycles its numeric display ID.
    let live = vec![
        screen(1, 0.0, 1920.0, true),
        screen(9, -1600.0, 1600.0, false),
    ];
    assert_eq!(session.restore(&live, &budget()).fallback, 1);
    assert!(window.0.borrow().frame.fits_inside(live[0].frame));
    assert!(window.0.borrow().frame.y >= 32.0);

    let window = fake(2, original);
    let mut session = snapshot(&[window.clone()]);
    let live = vec![screen(2, 0.0, 1600.0, true)];
    let restored = session.restore(&live, &budget());
    assert_eq!(restored.fallback, 0);
    assert_eq!(window.0.borrow().frame, rect(100.0, 80.0, 1200.0, 800.0));
}

#[test]
fn fixed_size_and_minimized_or_fullscreen_windows_preserve_their_state() {
    let window = fake(1, rect(-1500.0, 60.0, 1400.0, 900.0));
    window.0.borrow_mut().resizable = false;
    let mut session = snapshot(&[window.clone()]);
    session.gather(rect(0.0, 0.0, 640.0, 480.0), &budget());
    assert_eq!(window.0.borrow().resize_writes, 0);
    assert_eq!(window.0.borrow().frame.width, 1400.0);
    assert!(window.0.borrow().frame.y >= 32.0);
    window.0.borrow_mut().movable = false;
    assert_eq!(session.restore(&topology(), &budget()).skipped, 1);
    assert_eq!(window.0.borrow().writes, 1);
}

#[test]
fn capture_deduplicates_handles_and_keeps_partial_snapshot_without_any_discovery_on_restore() {
    let good = fake(1, rect(-1500.0, 80.0, 800.0, 600.0));
    let bad = fake(2, rect(2000.0, 80.0, 800.0, 600.0));
    bad.0.borrow_mut().closed = true;
    let mut session = Session::new(topology());
    let mut report = PhaseReport::default();
    session.capture_window(good.clone(), &budget(), &mut report);
    session.capture_window(good.clone(), &budget(), &mut report);
    session.capture_window(bad, &budget(), &mut report);
    session.capture_window(
        fake(3, rect(20_000.0, 0.0, 800.0, 600.0)),
        &budget(),
        &mut report,
    );
    session.capture_window(
        fake(4, rect(2000.0, 80.0, 800.0, 600.0)),
        &expired(),
        &mut report,
    );
    assert_eq!(
        (
            report.saved,
            report.failed,
            report.skipped,
            report.timed_out
        ),
        (1, 1, 1, true)
    );
    assert_eq!(
        session
            .gather(rect(0.0, 0.0, 640.0, 480.0), &budget())
            .moved,
        1
    );
    assert_eq!(session.restore(&topology(), &budget()).moved, 1);
}

#[test]
fn failure_before_gather_still_restores_primary_and_no_physical_display_never_moves_offscreen() {
    let main = fake(1, rect(100.0, 60.0, 800.0, 600.0));
    let mut session = snapshot(&[main.clone()]);
    main.0.borrow_mut().frame.x = -4000.0;
    assert_eq!(session.restore(&topology(), &budget()).moved, 1);
    assert_eq!(main.0.borrow().frame.x, 100.0);
    let mut no_physical = snapshot(&[main.clone()]);
    assert_eq!(no_physical.restore(&[], &budget()).failed, 1);
    assert_eq!(main.0.borrow().writes, 1);
}

//! Payload-free observations of the native posting boundary, scoped to one
//! remote input callback. These are API return timings, never delivery receipts
//! from WindowServer or the application handling the event.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::time::Instant;

use core_graphics::event::CGEventType;

const MAX_POST_SAMPLES: usize = 64;

/// Only counters, durations and monotonic times cross this boundary. Post
/// categories group event types without retaining buttons, keys or coordinates.
#[derive(Debug, Default)]
pub struct PostObservation {
    pub post_attempts: u64,
    pub post_returns: u64,
    /// Preparation failures before CGEventPost was invoked. CGEventPost itself
    /// returns void and cannot report that the OS accepted or consumed input.
    pub post_errors: u64,
    pub post_samples_dropped: u64,
    pub keyboard_posts: u64,
    pub mouse_move_posts: u64,
    pub mouse_button_down_posts: u64,
    pub mouse_button_up_posts: u64,
    pub mouse_drag_posts: u64,
    pub mouse_scroll_posts: u64,
    pub other_posts: u64,
    pub last_post_at: Option<Instant>,
    pub post_ms: Vec<f64>,
}

/// Fixed families only: never retain a raw native event type or input payload.
#[derive(Clone, Copy)]
pub(crate) enum PostEventKind {
    Keyboard,
    MouseMove,
    MouseButtonDown,
    MouseButtonUp,
    MouseDrag,
    MouseScroll,
    Other,
}

impl PostEventKind {
    pub(crate) fn from_type(event_type: CGEventType) -> Self {
        match event_type {
            CGEventType::KeyDown | CGEventType::KeyUp | CGEventType::FlagsChanged => Self::Keyboard,
            CGEventType::MouseMoved => Self::MouseMove,
            CGEventType::LeftMouseDown
            | CGEventType::RightMouseDown
            | CGEventType::OtherMouseDown => Self::MouseButtonDown,
            CGEventType::LeftMouseUp | CGEventType::RightMouseUp | CGEventType::OtherMouseUp => {
                Self::MouseButtonUp
            }
            CGEventType::LeftMouseDragged
            | CGEventType::RightMouseDragged
            | CGEventType::OtherMouseDragged => Self::MouseDrag,
            CGEventType::ScrollWheel => Self::MouseScroll,
            _ => Self::Other,
        }
    }

    fn record_return(self, observation: &mut PostObservation) {
        let total = match self {
            Self::Keyboard => &mut observation.keyboard_posts,
            Self::MouseMove => &mut observation.mouse_move_posts,
            Self::MouseButtonDown => &mut observation.mouse_button_down_posts,
            Self::MouseButtonUp => &mut observation.mouse_button_up_posts,
            Self::MouseDrag => &mut observation.mouse_drag_posts,
            Self::MouseScroll => &mut observation.mouse_scroll_posts,
            Self::Other => &mut observation.other_posts,
        };
        *total = total.saturating_add(1);
    }
}

thread_local! {
    static ACTIVE: RefCell<Option<PostObservation>> = const { RefCell::new(None) };
}

/// Scope stays on its originating thread and restores an outer observer even
/// during unwinding. Cleanup releases run outside these callback scopes.
pub struct PostObservationScope {
    previous: Option<PostObservation>,
    active: bool,
    _thread_bound: PhantomData<Rc<()>>,
}

impl PostObservationScope {
    pub fn begin() -> Self {
        Self {
            previous: ACTIVE.with(|active| active.replace(Some(PostObservation::default()))),
            active: true,
            _thread_bound: PhantomData,
        }
    }

    pub fn finish(mut self) -> PostObservation {
        self.active = false;
        ACTIVE
            .with(|active| active.replace(self.previous.take()))
            .unwrap_or_default()
    }
}

impl Drop for PostObservationScope {
    fn drop(&mut self) {
        if self.active {
            ACTIVE.with(|active| active.replace(self.previous.take()));
        }
    }
}

pub(crate) fn preparation_failed() {
    ACTIVE.with(|active| {
        if let Some(observation) = active.borrow_mut().as_mut() {
            observation.post_errors = observation.post_errors.saturating_add(1);
        }
    });
}

/// Keep the observer borrow out of the native call. A nested scope or callback
/// cannot deadlock a thread-local RefCell while the OS is processing the call.
pub(crate) fn observe_post(kind: PostEventKind, post: impl FnOnce()) {
    let observed = ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        if let Some(observation) = active.as_mut() {
            observation.post_attempts = observation.post_attempts.saturating_add(1);
            true
        } else {
            false
        }
    });
    if !observed {
        post();
        return;
    }
    let started = Instant::now();
    post();
    let returned = Instant::now();
    ACTIVE.with(|active| {
        if let Some(observation) = active.borrow_mut().as_mut() {
            observation.post_returns = observation.post_returns.saturating_add(1);
            kind.record_return(observation);
            observation.last_post_at = Some(returned);
            if observation.post_ms.len() < MAX_POST_SAMPLES {
                observation
                    .post_ms
                    .push(returned.duration_since(started).as_secs_f64() * 1000.0);
            } else {
                observation.post_samples_dropped =
                    observation.post_samples_dropped.saturating_add(1);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observes_only_scoped_posts_and_retains_totals_when_samples_are_full() {
        observe_post(PostEventKind::Other, || {});
        preparation_failed();
        let scope = PostObservationScope::begin();
        for _ in 0..MAX_POST_SAMPLES + 7 {
            observe_post(PostEventKind::Other, || {});
        }
        preparation_failed();
        let observed = scope.finish();
        assert_eq!(observed.post_attempts, (MAX_POST_SAMPLES + 7) as u64);
        assert_eq!(observed.post_returns, observed.post_attempts);
        assert_eq!(observed.other_posts, observed.post_returns);
        assert_eq!(observed.post_errors, 1);
        assert_eq!(observed.post_samples_dropped, 7);
        assert_eq!(observed.post_ms.len(), MAX_POST_SAMPLES);
        assert!(observed
            .post_ms
            .iter()
            .all(|ms| ms.is_finite() && *ms >= 0.0));
        assert!(observed.last_post_at.is_some());
        assert_eq!(PostObservationScope::begin().finish().post_attempts, 0);
    }

    #[test]
    fn nested_and_unwound_scopes_restore_the_outer_observation() {
        let outer = PostObservationScope::begin();
        observe_post(PostEventKind::Other, || {});
        let inner = PostObservationScope::begin();
        observe_post(PostEventKind::Other, || {});
        assert_eq!(inner.finish().post_returns, 1);
        let _ = std::panic::catch_unwind(|| {
            let _scope = PostObservationScope::begin();
            observe_post(PostEventKind::Other, || {});
            panic!("synthetic unwinding");
        });
        observe_post(PostEventKind::Other, || {});
        assert_eq!(outer.finish().post_returns, 2);
        assert_eq!(PostObservationScope::begin().finish().post_returns, 0);
    }

    #[test]
    fn threads_cannot_observe_each_others_posts() {
        let scope = PostObservationScope::begin();
        std::thread::spawn(|| {
            observe_post(PostEventKind::Other, || {});
            preparation_failed();
            let own = PostObservationScope::begin();
            observe_post(PostEventKind::Other, || {});
            assert_eq!(own.finish().post_returns, 1);
        })
        .join()
        .unwrap();
        assert_eq!(scope.finish().post_returns, 0);
    }

    #[test]
    fn posting_families_count_returns_only_and_saturate() {
        let scope = PostObservationScope::begin();
        let _ = std::panic::catch_unwind(|| {
            observe_post(PostEventKind::MouseButtonDown, || {
                panic!("synthetic post unwind")
            });
        });
        ACTIVE.with(|active| active.borrow_mut().as_mut().unwrap().mouse_drag_posts = u64::MAX);
        observe_post(PostEventKind::MouseDrag, || {});
        let observed = scope.finish();
        assert_eq!(observed.post_attempts, 2);
        assert_eq!(observed.post_returns, 1);
        assert_eq!(observed.mouse_button_down_posts, 0);
        assert_eq!(observed.mouse_drag_posts, u64::MAX);
    }
}

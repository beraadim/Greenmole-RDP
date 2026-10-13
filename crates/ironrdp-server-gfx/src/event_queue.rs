//! Bounded producer queues scoped to one connection. Overflow fails the
//! connection instead of silently losing a reliable protocol message. Clipboard
//! failures instead quarantine that optional channel until reconnect. PCM audio
//! has a separate duration budget and deadline, so stalls do not replay old sound.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ironrdp_cliprdr::backend::ClipboardMessage;
use ironrdp_rdpsnd::server::RdpsndServerMessage;
use tokio::sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore};

use super::{Credentials, ServerEvent};

const DATA_CAPACITY: usize = 256;
const DATA_BYTES: usize = 32 * 1024 * 1024;
const CONTROL_CAPACITY: usize = 32;
const AUDIO_BUDGET_US: u32 = 200_000;
const MAX_AUDIO_AGE: Duration = Duration::from_millis(200);
const CLIPBOARD_OPEN: u8 = 0;
const CLIPBOARD_PUBLISHING: u8 = 1;
const CLIPBOARD_CLOSED: u8 = 2;

#[cfg(test)]
#[path = "clipboard_queue_tests.rs"]
mod clipboard_tests;

#[derive(Default)]
pub struct AudioQueueStats {
    dropped: AtomicU64,
    stale: AtomicU64,
    queue_age_us: AtomicU64,
    queued_us: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AudioQueueSnapshot {
    pub dropped_frames: u64,
    pub stale_frames: u64,
    pub queue_age_ms: f64,
    pub queued_ms: f64,
}

impl AudioQueueStats {
    pub fn snapshot(&self) -> AudioQueueSnapshot {
        AudioQueueSnapshot {
            dropped_frames: self.dropped.load(Ordering::Relaxed),
            stale_frames: self.stale.load(Ordering::Relaxed),
            queue_age_ms: self.queue_age_us.load(Ordering::Relaxed) as f64 / 1000.0,
            queued_ms: self.queued_us.load(Ordering::Relaxed) as f64 / 1000.0,
        }
    }
}

struct QueuedAudio {
    captured_at: Instant,
    duration_us: u32,
    stats: Arc<AudioQueueStats>,
    _duration: OwnedSemaphorePermit,
}

impl Drop for QueuedAudio {
    fn drop(&mut self) {
        self.stats
            .queued_us
            .fetch_sub(u64::from(self.duration_us), Ordering::Relaxed);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionStatus {
    Open,
    Failed(&'static str),
    Closed,
}

struct Session {
    active: AtomicBool,
    status: watch::Sender<SessionStatus>,
    clipboard: AtomicU8,
    clipboard_failure: OnceLock<&'static str>,
    clipboard_status: watch::Sender<bool>,
}

impl Session {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            active: AtomicBool::new(true),
            status: watch::channel(SessionStatus::Open).0,
            clipboard: AtomicU8::new(CLIPBOARD_OPEN),
            clipboard_failure: OnceLock::new(),
            clipboard_status: watch::channel(false).0,
        })
    }

    fn close(&self) {
        self.clipboard.store(CLIPBOARD_CLOSED, Ordering::Release);
        self.active.store(false, Ordering::Release);
        self.status.send_replace(SessionStatus::Closed);
    }

    fn fail(&self, reason: &'static str) {
        self.clipboard.store(CLIPBOARD_CLOSED, Ordering::Release);
        if self.active.swap(false, Ordering::AcqRel) {
            self.status.send_replace(SessionStatus::Failed(reason));
            tracing::warn!(reason, "Disconnecting client after output queue exhaustion");
        }
    }

    fn quarantine_clipboard(&self, reason: &'static str) {
        let reason = self.clipboard_failure.get_or_init(|| reason);
        if self.clipboard.swap(CLIPBOARD_CLOSED, Ordering::AcqRel) != CLIPBOARD_CLOSED {
            self.clipboard_status.send_replace(true);
            tracing::warn!(
                reason,
                "Clipboard disabled for this connection; reconnect to restore clipboard"
            );
        }
    }
}

struct ClipboardPublication<'a>(&'a Session);

impl Drop for ClipboardPublication<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0
                .quarantine_clipboard("clipboard publication panicked");
        } else {
            // Quarantine may have happened while AppKit was busy. Completing
            // an already admitted native call must never reopen that channel.
            let _ = self.0.clipboard.compare_exchange(
                CLIPBOARD_PUBLISHING,
                CLIPBOARD_OPEN,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
}

pub(super) struct QueuedEvent {
    event: ServerEvent,
    admission: EventAdmission,
}

/// Retain queue budgets and capture age until the event has passed both the
/// server lock and serialized socket-write admission.
pub(super) struct EventAdmission {
    session: Arc<Session>,
    clipboard: bool,
    _bytes: OwnedSemaphorePermit,
    audio: Option<QueuedAudio>,
}

impl QueuedEvent {
    pub(super) fn into_parts(self) -> (ServerEvent, EventAdmission) {
        (self.event, self.admission)
    }
}

impl EventAdmission {
    pub(super) fn is_deliverable_at(&self, now: Instant) -> bool {
        if !self.session.active.load(Ordering::Acquire) {
            return false;
        }
        if self.clipboard && self.session.clipboard.load(Ordering::Acquire) == CLIPBOARD_CLOSED {
            return false;
        }
        if let Some(audio) = &self.audio {
            let age = now.saturating_duration_since(audio.captured_at);
            audio.stats.queue_age_us.store(
                age.as_micros().min(u128::from(u64::MAX)) as u64,
                Ordering::Relaxed,
            );
            if age > MAX_AUDIO_AGE {
                audio.stats.stale.fetch_add(1, Ordering::Relaxed);
                return false;
            }
        }
        true
    }
}

/// Synchronous, bounded event sender usable from native callbacks. Producers
/// receive a connection-scoped clone; a disconnected producer cannot enqueue
/// into the next client's session. Reliable messages rejected by `send` fail
/// their session; disposable PCM uses `send_audio_wave` with a freshness limit.
#[derive(Clone)]
pub struct ServerEventTx {
    data: mpsc::Sender<QueuedEvent>,
    addresses: mpsc::Sender<ServerEvent>,
    credentials: watch::Sender<Option<Credentials>>,
    quit: watch::Sender<bool>,
    budget: Arc<Semaphore>,
    audio_budget: Arc<Semaphore>,
    audio_stats: Arc<AudioQueueStats>,
    current: Arc<Mutex<Arc<Session>>>,
    scope: Option<Arc<Session>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerEventSendError(pub &'static str);

impl std::fmt::Display for ServerEventSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ServerEventSendError {}

pub struct ServerEventRx {
    pub(super) controls: ControlReceiver,
    pub(super) data: DataReceiver,
}

pub(super) struct ControlReceiver {
    addresses: mpsc::Receiver<ServerEvent>,
    credentials: watch::Receiver<Option<Credentials>>,
    quit: watch::Receiver<bool>,
}

pub(super) struct DataReceiver {
    data: mpsc::Receiver<QueuedEvent>,
    deferred: Option<QueuedEvent>,
}

/// Dropping this guard invalidates callbacks even when a connection future is
/// cancelled during authentication, activation, or a partially written PDU.
pub(super) struct SessionGuard(Arc<Session>);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.0.close();
    }
}

pub(super) fn channel() -> (ServerEventTx, ServerEventRx) {
    channel_with_limits(DATA_CAPACITY, DATA_BYTES)
}

fn channel_with_limits(capacity: usize, bytes: usize) -> (ServerEventTx, ServerEventRx) {
    let (data_tx, data_rx) = mpsc::channel(capacity);
    let (addresses_tx, addresses_rx) = mpsc::channel(CONTROL_CAPACITY);
    let (credentials_tx, credentials_rx) = watch::channel(None);
    let (quit_tx, quit_rx) = watch::channel(false);
    (
        ServerEventTx {
            data: data_tx,
            addresses: addresses_tx,
            credentials: credentials_tx,
            quit: quit_tx,
            budget: Arc::new(Semaphore::new(bytes)),
            audio_budget: Arc::new(Semaphore::new(AUDIO_BUDGET_US as usize)),
            audio_stats: Arc::default(),
            current: Arc::new(Mutex::new(Session::new())),
            scope: None,
        },
        ServerEventRx {
            controls: ControlReceiver {
                addresses: addresses_rx,
                credentials: credentials_rx,
                quit: quit_rx,
            },
            data: DataReceiver {
                data: data_rx,
                deferred: None,
            },
        },
    )
}

impl ServerEventTx {
    pub fn audio_stats(&self) -> Arc<AudioQueueStats> {
        self.audio_stats.clone()
    }

    /// Enqueue disposable PCM with its original local capture time. `false`
    /// means this packet was intentionally dropped to preserve live playback;
    /// reliable channel messages still use `send` and fail on overflow.
    pub fn send_audio_wave(
        &self,
        data: Vec<u8>,
        timestamp_ms: u32,
        duration: Duration,
        captured_at: Instant,
    ) -> Result<bool, ServerEventSendError> {
        if self.data.is_closed() {
            return Err(ServerEventSendError("session is closed"));
        }
        let current = self.current.lock().unwrap();
        let session = self.scope.clone().unwrap_or_else(|| current.clone());
        if !session.active.load(Ordering::Acquire) {
            return Err(ServerEventSendError("session is closed"));
        }
        if captured_at.elapsed() > MAX_AUDIO_AGE {
            self.audio_stats.stale.fetch_add(1, Ordering::Relaxed);
            return Ok(false);
        }
        let duration_us = u32::try_from(duration.as_nanos().div_ceil(1000))
            .ok()
            .filter(|value| (1..=AUDIO_BUDGET_US).contains(value));
        let permit =
            duration_us.and_then(|us| self.audio_budget.clone().try_acquire_many_owned(us).ok());
        let Some(duration_permit) = permit else {
            self.audio_stats.dropped.fetch_add(1, Ordering::Relaxed);
            return Ok(false);
        };
        let event = ServerEvent::Rdpsnd(RdpsndServerMessage::Wave(data, timestamp_ms));
        let bytes = event_bytes(&event)
            .and_then(|n| u32::try_from(n).ok())
            .and_then(|n| self.budget.clone().try_acquire_many_owned(n).ok());
        let Some(bytes) = bytes else {
            self.audio_stats.dropped.fetch_add(1, Ordering::Relaxed);
            return Ok(false);
        };
        let duration_us = duration_us.unwrap();
        self.audio_stats
            .queued_us
            .fetch_add(u64::from(duration_us), Ordering::Relaxed);
        let queued = QueuedEvent {
            event,
            admission: EventAdmission {
                session,
                clipboard: false,
                _bytes: bytes,
                audio: Some(QueuedAudio {
                    captured_at,
                    duration_us,
                    stats: self.audio_stats.clone(),
                    _duration: duration_permit,
                }),
            },
        };
        match self.data.try_send(queued) {
            Ok(()) => Ok(true),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.audio_stats.dropped.fetch_add(1, Ordering::Relaxed);
                Ok(false)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(ServerEventSendError("session is closed"))
            }
        }
    }

    fn session(&self) -> Arc<Session> {
        self.scope
            .clone()
            .unwrap_or_else(|| self.current.lock().unwrap().clone())
    }

    pub(super) fn end_session(&self) {
        self.session().close();
    }

    pub(super) fn start_session(&self) -> (SessionGuard, Self) {
        let session = Session::new();
        let mut current = self.current.lock().unwrap();
        current.close();
        *current = session.clone();
        let mut sender = self.clone();
        sender.scope = Some(session.clone());
        (SessionGuard(session), sender)
    }

    pub fn is_closed(&self) -> bool {
        self.data.is_closed() || !self.session().active.load(Ordering::Acquire)
    }

    /// A clipboard failure is permanent for this connection. Reconnection gives
    /// producers a fresh gate; stale callbacks keep their old, closed gate.
    pub fn quarantine_clipboard(&self, reason: &'static str) {
        self.session().quarantine_clipboard(reason);
    }

    pub fn clipboard_failure(&self) -> Option<&'static str> {
        self.session().clipboard_failure.get().copied()
    }

    pub(super) async fn clipboard_quarantined(&self) {
        let session = self.session();
        let mut status = session.clipboard_status.subscribe();
        loop {
            if *status.borrow_and_update() {
                return;
            }
            if status.changed().await.is_err() {
                return;
            }
        }
    }

    pub fn is_clipboard_closed(&self) -> bool {
        self.is_closed() || self.clipboard_failure().is_some()
    }

    /// Admit one final native publication without holding a lock across AppKit.
    /// Quarantine never waits for native code. An already admitted publication
    /// may finish, but all later publications are permanently rejected.
    pub fn with_clipboard_active<T>(&self, publish: impl FnOnce() -> T) -> Option<T> {
        let session = self.session();
        if self.data.is_closed() || !session.active.load(Ordering::Acquire) {
            return None;
        }
        session
            .clipboard
            .compare_exchange(
                CLIPBOARD_OPEN,
                CLIPBOARD_PUBLISHING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()?;
        let _publication = ClipboardPublication(&session);
        Some(publish())
    }

    /// Completes on disconnect or overflow, including when no producer sends.
    pub async fn closed(&self) {
        let session = self.session();
        let mut status = session.status.subscribe();
        loop {
            if *status.borrow_and_update() != SessionStatus::Open {
                return;
            }
            tokio::select! {
                _ = self.data.closed() => return,
                changed = status.changed() => if changed.is_err() { return; },
            }
        }
    }

    pub(super) async fn failure(&self) -> &'static str {
        let session = self.session();
        let mut status = session.status.subscribe();
        loop {
            let current = *status.borrow_and_update();
            match current {
                SessionStatus::Failed(reason) => return reason,
                SessionStatus::Closed => std::future::pending::<()>().await,
                SessionStatus::Open => {}
            }
            if status.changed().await.is_err() {
                return "session producer channel closed";
            }
        }
    }

    pub fn send(&self, event: ServerEvent) -> Result<(), ServerEventSendError> {
        if self.data.is_closed()
            || self
                .scope
                .as_ref()
                .is_some_and(|s| !s.active.load(Ordering::Acquire))
        {
            return Err(ServerEventSendError("session is closed"));
        }
        match event {
            // Shutdown and latest credentials have dedicated, coalescing
            // slots. Data congestion can never crowd out these controls.
            ServerEvent::Quit(_) => {
                self.quit.send_replace(true);
                Ok(())
            }
            ServerEvent::SetCredentials(credentials) => {
                self.credentials.send_replace(Some(credentials));
                Ok(())
            }
            event @ ServerEvent::GetLocalAddr(_) => self
                .addresses
                .try_send(event)
                .map_err(|_| ServerEventSendError("server query queue is full or closed")),
            ServerEvent::Clipboard(ClipboardMessage::Error(_)) => {
                self.quarantine_clipboard("clipboard producer reported an error");
                Err(ServerEventSendError(
                    "clipboard is disabled for this connection",
                ))
            }
            ServerEvent::Rdpsnd(RdpsndServerMessage::Error(_)) => {
                self.session().fail("channel producer reported an error");
                Err(ServerEventSendError("channel producer reported an error"))
            }
            event => {
                // Serialize publication with session replacement. A callback
                // that passed the early check before disconnect must not fill
                // the next session's queue after its stale-data drain.
                let current = self.current.lock().unwrap();
                let session = self.scope.clone().unwrap_or_else(|| current.clone());
                if !session.active.load(Ordering::Acquire) {
                    return Err(ServerEventSendError("session is closed"));
                }
                let clipboard = is_clipboard_event(&event);
                if clipboard && session.clipboard.load(Ordering::Acquire) == CLIPBOARD_CLOSED {
                    return Err(ServerEventSendError(
                        "clipboard is disabled for this connection",
                    ));
                }
                let charge = event_bytes(&event).and_then(|n| u32::try_from(n).ok());
                let permit =
                    charge.and_then(|n| self.budget.clone().try_acquire_many_owned(n).ok());
                let Some(permit) = permit else {
                    if clipboard {
                        session.quarantine_clipboard("clipboard output byte budget exceeded");
                    } else {
                        session.fail("output byte budget exceeded");
                    }
                    return Err(ServerEventSendError("output byte budget exceeded"));
                };
                self.data
                    .try_send(QueuedEvent {
                        event,
                        admission: EventAdmission {
                            session: session.clone(),
                            clipboard,
                            _bytes: permit,
                            audio: None,
                        },
                    })
                    .map_err(|_| {
                        if clipboard {
                            session.quarantine_clipboard(
                                "clipboard output event queue full or closed",
                            );
                        } else {
                            session.fail("output event queue full or closed");
                        }
                        ServerEventSendError("output event queue full or closed")
                    })
            }
        }
    }
}

fn is_clipboard_event(event: &ServerEvent) -> bool {
    matches!(
        event,
        ServerEvent::Clipboard(_)
            | ServerEvent::ClipboardFileContents(_)
            | ServerEvent::ClipboardFileContentsRequest(_)
    )
}

fn event_bytes(event: &ServerEvent) -> Option<usize> {
    let payload = match event {
        ServerEvent::Rdpsnd(RdpsndServerMessage::Wave(data, _)) => data.capacity(),
        ServerEvent::ClipboardFileContents(response) => response.data().len(),
        ServerEvent::Clipboard(ClipboardMessage::SendFormatData(response)) => response.data().len(),
        ServerEvent::Clipboard(ClipboardMessage::SendInitiateCopy(formats)) => {
            let mut size = formats
                .capacity()
                .checked_mul(size_of::<ironrdp_cliprdr::pdu::ClipboardFormat>())?;
            for format in formats {
                size = size.checked_add(format.name().map_or(0, |name| name.value().len()))?;
            }
            size
        }
        _ => 0,
    };
    size_of::<QueuedEvent>().checked_add(payload)
}

impl ControlReceiver {
    fn try_recv(&mut self) -> Result<ServerEvent, mpsc::error::TryRecvError> {
        if *self.quit.borrow() {
            return Ok(ServerEvent::Quit("server shutdown requested".into()));
        }
        if self.credentials.has_changed().unwrap_or(false) {
            if let Some(credentials) = self.credentials.borrow_and_update().clone() {
                return Ok(ServerEvent::SetCredentials(credentials));
            }
        }
        self.addresses.try_recv()
    }

    pub(super) async fn recv(&mut self) -> Option<ServerEvent> {
        loop {
            if let Ok(event) = self.try_recv() {
                return Some(event);
            }
            tokio::select! {
                biased;
                result = self.quit.changed() => if result.is_err() { return None; },
                result = self.credentials.changed() => {
                    if result.is_err() { return None; }
                    if let Some(credentials) = self.credentials.borrow_and_update().clone() {
                        return Some(ServerEvent::SetCredentials(credentials));
                    }
                },
                event = self.addresses.recv() => return event,
            }
        }
    }
}

impl DataReceiver {
    /// A resize may close admission immediately after recv completes. Retain
    /// that one event, including its byte/audio permits, for the next activation.
    pub(super) fn defer(&mut self, queued: QueuedEvent) {
        assert!(
            self.deferred.is_none(),
            "only one event can await resize admission"
        );
        self.deferred = Some(queued);
    }

    pub(super) async fn recv(&mut self) -> Option<QueuedEvent> {
        loop {
            let queued = match self.deferred.take() {
                Some(queued) => queued,
                None => self.data.recv().await?,
            };
            if queued.admission.is_deliverable_at(Instant::now()) {
                return Some(queued);
            }
        }
    }

    fn try_recv(&mut self) -> Result<ServerEvent, mpsc::error::TryRecvError> {
        loop {
            let queued = match self.deferred.take() {
                Some(queued) => queued,
                None => self.data.try_recv()?,
            };
            if queued.admission.is_deliverable_at(Instant::now()) {
                return Ok(queued.event);
            }
        }
    }

    pub(super) fn discard_pending(&mut self) {
        self.deferred = None;
        while self.data.try_recv().is_ok() {}
    }
}

impl ServerEventRx {
    pub async fn recv(&mut self) -> Option<ServerEvent> {
        tokio::select! {
            biased;
            event = self.controls.recv() => match event {
                Some(event) => Some(event),
                None => self.data.recv().await.map(|queued| queued.event),
            },
            event = self.data.recv() => event.map(|queued| queued.event),
        }
    }

    pub fn try_recv(&mut self) -> Result<ServerEvent, mpsc::error::TryRecvError> {
        if let Ok(event) = self.controls.try_recv() {
            return Ok(event);
        }
        self.data.try_recv()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wave(bytes: usize) -> ServerEvent {
        ServerEvent::Rdpsnd(RdpsndServerMessage::Wave(vec![0; bytes], 0))
    }

    #[tokio::test]
    async fn dequeue_retains_deadline_and_budget_until_dispatch_finishes() {
        let (sender, mut receiver) = channel();
        let now = Instant::now();
        sender
            .send_audio_wave(vec![0; 3840], 0, Duration::from_millis(20), now)
            .unwrap();
        let (event, admission) = receiver.data.recv().await.unwrap().into_parts();
        assert!(matches!(event, ServerEvent::Rdpsnd(_)));
        assert_eq!(sender.audio_stats().snapshot().queued_ms, 20.0);
        assert!(admission.is_deliverable_at(now));
        // This interval represents waiting for the server state lock.
        assert!(!admission.is_deliverable_at(now + Duration::from_millis(201)));
        assert_eq!(sender.audio_stats().snapshot().stale_frames, 1);
        drop(admission);
        assert_eq!(sender.audio_stats().snapshot().queued_ms, 0.0);
    }

    #[tokio::test]
    async fn resize_deferred_event_keeps_budget_and_is_cleared_before_a_new_session() {
        let (sender, mut receiver) = channel();
        let (session, scoped) = sender.start_session();
        scoped
            .send_audio_wave(vec![0; 3840], 0, Duration::from_millis(20), Instant::now())
            .unwrap();
        let queued = receiver.data.recv().await.unwrap();
        receiver.data.defer(queued);
        assert_eq!(sender.audio_stats().snapshot().queued_ms, 20.0);
        drop(session);
        let (_session, fresh) = sender.start_session();
        receiver.data.discard_pending();
        assert_eq!(sender.audio_stats().snapshot().queued_ms, 0.0);
        fresh.send(wave(10)).unwrap();
        let (event, _admission) = receiver.data.recv().await.unwrap().into_parts();
        let ServerEvent::Rdpsnd(RdpsndServerMessage::Wave(data, _)) = event else {
            panic!("missing new session output");
        };
        assert_eq!(data.len(), 10);
    }

    #[test]
    fn live_pcm_duration_budget_drops_audio_without_failing_reliable_channels() {
        let (sender, mut receiver) = channel();
        let now = Instant::now();
        for timestamp in 0..10 {
            assert!(sender
                .send_audio_wave(
                    vec![0; 3840],
                    timestamp * 20,
                    Duration::from_millis(20),
                    now
                )
                .unwrap());
        }
        assert!(!sender
            .send_audio_wave(vec![0; 3840], 200, Duration::from_millis(20), now)
            .unwrap());
        assert_eq!(sender.audio_stats().snapshot().queued_ms, 200.0);
        assert_eq!(sender.audio_stats().snapshot().dropped_frames, 1);
        assert!(!sender.is_closed());
        assert!(matches!(receiver.try_recv(), Ok(ServerEvent::Rdpsnd(_))));
        assert_eq!(sender.audio_stats().snapshot().queued_ms, 180.0);
        assert!(sender
            .send_audio_wave(vec![0; 3840], 220, Duration::from_millis(20), now)
            .unwrap());
        receiver.data.discard_pending();
        assert_eq!(sender.audio_stats().snapshot().queued_ms, 0.0);
    }

    #[test]
    fn stale_pcm_never_reaches_client_and_budget_is_reclaimed() {
        let (sender, mut receiver) = channel();
        let now = Instant::now();
        sender
            .send_audio_wave(vec![0; 3840], 0, Duration::from_millis(20), now)
            .unwrap();
        // Move only the synthetic packet clock, avoiding slow/flaky sleeps.
        let mut queued = receiver.data.data.try_recv().unwrap();
        queued.admission.audio.as_mut().unwrap().captured_at = now - Duration::from_secs(1);
        sender
            .data
            .try_send(queued)
            .unwrap_or_else(|_| panic!("test queue unexpectedly full"));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(sender.audio_stats().snapshot().stale_frames, 1);
        assert_eq!(sender.audio_stats().snapshot().queued_ms, 0.0);
        assert!(!sender.is_closed());
        assert!(sender
            .send_audio_wave(
                vec![0; 3840],
                1000,
                Duration::from_millis(20),
                Instant::now()
            )
            .unwrap());
    }

    #[test]
    fn audio_cannot_exhaust_count_or_byte_budgets_and_disconnect_next_client() {
        let (sender, mut receiver) = channel_with_limits(1, 4096);
        let (old_guard, old_sender) = sender.start_session();
        let now = Instant::now();
        assert!(!old_sender
            .send_audio_wave(vec![0; 4096], 0, Duration::from_millis(20), now)
            .unwrap());
        assert!(!old_sender.is_closed());
        assert!(old_sender
            .send_audio_wave(vec![0; 100], 20, Duration::from_millis(20), now)
            .unwrap());
        assert!(!old_sender
            .send_audio_wave(vec![0; 100], 40, Duration::from_millis(20), now)
            .unwrap());
        drop(old_guard);
        let (_new_guard, new_sender) = sender.start_session();
        receiver.data.discard_pending();
        assert_eq!(sender.audio_stats().snapshot().queued_ms, 0.0);
        assert!(old_sender
            .send_audio_wave(vec![0; 100], 60, Duration::from_millis(20), now)
            .is_err());
        assert!(new_sender
            .send_audio_wave(vec![0; 100], 80, Duration::from_millis(20), now)
            .unwrap());
        assert!(!new_sender.is_closed());
    }

    #[tokio::test]
    async fn count_overflow_fails_session_without_crowding_out_controls() {
        let (sender, mut receiver) = channel_with_limits(1, 4096);
        sender.send(wave(10)).unwrap();
        assert!(sender.send(wave(10)).is_err());
        assert_eq!(sender.failure().await, "output event queue full or closed");
        let (query, reply) = tokio::sync::oneshot::channel();
        sender.send(ServerEvent::GetLocalAddr(query)).unwrap();
        let Some(ServerEvent::GetLocalAddr(query)) = receiver.recv().await else {
            panic!("query lost");
        };
        query.send(None).unwrap();
        assert_eq!(reply.await.unwrap(), None);
        sender.send(ServerEvent::Quit("stop".into())).unwrap();
        assert!(matches!(receiver.recv().await, Some(ServerEvent::Quit(_))));
    }

    #[tokio::test]
    async fn byte_budget_failure_closes_producers_and_next_session_recovers() {
        let (sender, mut receiver) = channel_with_limits(8, 1024);
        let (old_guard, old_sender) = sender.start_session();
        old_sender.send(wave(100)).unwrap();
        assert!(old_sender.send(wave(1024)).is_err());
        old_sender.closed().await;
        assert!(old_sender.is_closed());
        drop(old_guard);
        let (_new_guard, new_sender) = sender.start_session();
        // Old retained data is discarded and returns its byte permits.
        receiver.data.discard_pending();
        assert!(old_sender.send(wave(1)).is_err());
        new_sender.send(wave(500)).unwrap();
        assert!(matches!(
            receiver.recv().await,
            Some(ServerEvent::Rdpsnd(_))
        ));
        assert!(!new_sender.is_closed());
    }

    #[tokio::test]
    async fn disconnected_queued_messages_cannot_reach_a_new_client() {
        let (sender, mut receiver) = channel_with_limits(4, 4096);
        let (guard, old_sender) = sender.start_session();
        old_sender.send(wave(10)).unwrap();
        drop(guard);
        let (_guard, new_sender) = sender.start_session();
        new_sender.send(wave(20)).unwrap();
        let Some(ServerEvent::Rdpsnd(RdpsndServerMessage::Wave(data, _))) = receiver.recv().await
        else {
            panic!("missing wave");
        };
        assert_eq!(data.len(), 20);
        assert!(old_sender.send(wave(30)).is_err());
    }

    #[tokio::test]
    async fn credentials_are_coalesced_and_delivered_when_receiver_is_waiting() {
        let (sender, mut receiver) = channel();
        for username in ["old", "new"] {
            sender
                .send(ServerEvent::SetCredentials(Credentials {
                    username: username.into(),
                    password: String::new(),
                    domain: None,
                }))
                .unwrap();
        }
        let Some(ServerEvent::SetCredentials(credentials)) = receiver.recv().await else {
            panic!("missing credentials");
        };
        assert_eq!(credentials.username, "new");
        let (event, ()) = tokio::join!(receiver.recv(), async {
            tokio::task::yield_now().await;
            sender
                .send(ServerEvent::SetCredentials(Credentials {
                    username: "latest".into(),
                    password: String::new(),
                    domain: None,
                }))
                .unwrap();
        });
        let Some(ServerEvent::SetCredentials(credentials)) = event else {
            panic!("missing changed credentials");
        };
        assert_eq!(credentials.username, "latest");
    }

    #[test]
    fn query_overflow_is_observable_and_shutdown_has_no_queue_limit() {
        let (sender, mut receiver) = channel();
        for _ in 0..CONTROL_CAPACITY {
            let (query, _) = tokio::sync::oneshot::channel();
            sender.send(ServerEvent::GetLocalAddr(query)).unwrap();
        }
        let (query, mut reply) = tokio::sync::oneshot::channel();
        assert!(sender.send(ServerEvent::GetLocalAddr(query)).is_err());
        assert!(matches!(
            reply.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
        sender.send(ServerEvent::Quit("stop".into())).unwrap();
        assert!(matches!(receiver.try_recv(), Ok(ServerEvent::Quit(_))));
    }
}

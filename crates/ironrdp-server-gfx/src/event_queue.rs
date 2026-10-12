//! Bounded producer queues scoped to one connection. Overflow fails the
//! connection instead of silently losing an audio/clipboard protocol message.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ironrdp_cliprdr::backend::ClipboardMessage;
use ironrdp_rdpsnd::server::RdpsndServerMessage;
use tokio::sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore};

use super::{Credentials, ServerEvent};

const DATA_CAPACITY: usize = 256;
const DATA_BYTES: usize = 32 * 1024 * 1024;
const CONTROL_CAPACITY: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionStatus {
    Open,
    Failed(&'static str),
    Closed,
}

struct Session {
    active: AtomicBool,
    status: watch::Sender<SessionStatus>,
}

impl Session {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            active: AtomicBool::new(true),
            status: watch::channel(SessionStatus::Open).0,
        })
    }

    fn close(&self) {
        self.active.store(false, Ordering::Release);
        self.status.send_replace(SessionStatus::Closed);
    }

    fn fail(&self, reason: &'static str) {
        if self.active.swap(false, Ordering::AcqRel) {
            self.status.send_replace(SessionStatus::Failed(reason));
            tracing::warn!(reason, "Disconnecting client after output queue exhaustion");
        }
    }
}

struct QueuedEvent {
    event: ServerEvent,
    session: Arc<Session>,
    _bytes: OwnedSemaphorePermit,
}

/// Synchronous, bounded event sender usable from native callbacks. Producers
/// receive a connection-scoped clone; a disconnected producer cannot enqueue
/// into the next client's session. Any rejected data message fails its session.
#[derive(Clone)]
pub struct ServerEventTx {
    data: mpsc::Sender<QueuedEvent>,
    addresses: mpsc::Sender<ServerEvent>,
    credentials: watch::Sender<Option<Credentials>>,
    quit: watch::Sender<bool>,
    budget: Arc<Semaphore>,
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
            current: Arc::new(Mutex::new(Session::new())),
            scope: None,
        },
        ServerEventRx {
            controls: ControlReceiver {
                addresses: addresses_rx,
                credentials: credentials_rx,
                quit: quit_rx,
            },
            data: DataReceiver { data: data_rx },
        },
    )
}

impl ServerEventTx {
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
            ServerEvent::Clipboard(ClipboardMessage::Error(_))
            | ServerEvent::Rdpsnd(RdpsndServerMessage::Error(_)) => {
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
                let charge = event_bytes(&event).and_then(|n| u32::try_from(n).ok());
                let permit =
                    charge.and_then(|n| self.budget.clone().try_acquire_many_owned(n).ok());
                let Some(permit) = permit else {
                    session.fail("output byte budget exceeded");
                    return Err(ServerEventSendError("output byte budget exceeded"));
                };
                self.data
                    .try_send(QueuedEvent {
                        event,
                        session: session.clone(),
                        _bytes: permit,
                    })
                    .map_err(|_| {
                        session.fail("output event queue full or closed");
                        ServerEventSendError("output event queue full or closed")
                    })
            }
        }
    }
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
    pub(super) async fn recv(&mut self) -> Option<ServerEvent> {
        while let Some(queued) = self.data.recv().await {
            if queued.session.active.load(Ordering::Acquire) {
                return Some(queued.event);
            }
        }
        None
    }

    fn try_recv(&mut self) -> Result<ServerEvent, mpsc::error::TryRecvError> {
        loop {
            let queued = self.data.try_recv()?;
            if queued.session.active.load(Ordering::Acquire) {
                return Ok(queued.event);
            }
        }
    }

    pub(super) fn discard_pending(&mut self) {
        while self.data.try_recv().is_ok() {}
    }
}

impl ServerEventRx {
    pub async fn recv(&mut self) -> Option<ServerEvent> {
        tokio::select! {
            biased;
            event = self.controls.recv() => match event {
                Some(event) => Some(event),
                None => self.data.recv().await,
            },
            event = self.data.recv() => event,
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

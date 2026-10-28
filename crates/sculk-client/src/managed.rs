//! Explicit one-room reconnect/resubscribe coordinator. No WRITE replay or HTTP
//! password/device-approval loop. Connectors may reuse only owner-approved, valid
//! credentials and must clean up when dropped. See docs/recovery.md.
use crate::{
    connect,
    messaging::{Event, Events, MessageId, ReceiveError},
    read_only::RoomId,
    recovery::{connect_with_retry, AttemptError, RetryPolicy},
    session::{AuthError, Session, State},
    synchronization::{self, CheckpointStore, Cursor, Page, Request, Tracker},
};
use std::{future::Future, time::Duration};
use tokio::sync::watch;

/// Conservative classification: ambiguous LOGINLIST/overall timeouts, TLS trust,
/// authentication, verification, rejection and policy errors are permanent.
/// Only pre-login transport interruptions are safely retryable by this adapter.
pub fn classify(error: connect::Error) -> AttemptError<connect::Error> {
    match error {
        connect::Error::Cancelled => AttemptError::Cancelled,
        connect::Error::DialIo(
            _,
            std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::BrokenPipe,
        ) => AttemptError::Transient(error),
        connect::Error::Transport(
            connect::Stage::Booking | connect::Stage::Checkin,
            crate::Error::Closed | crate::Error::Timeout | crate::Error::Overloaded,
        ) => AttemptError::Transient(error),
        _ => AttemptError::Permanent(error),
    }
}
/// Lifecycle failure with redacted diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A healthy session already exists; creating a second one is refused.
    #[error("a session is already active")]
    AlreadyActive,
    /// Expiry/authentication failure requires fresh explicitly approved credentials.
    #[error("session requires explicit credential or authentication intervention")]
    Intervention,
    /// Connector returned a session without active authentication.
    #[error("connector did not return an authenticated session")]
    InvalidSession,
    /// Explicit bounded connection attempt failed. No source payload formatted.
    #[error("bounded reconnection failed")]
    Connect(AttemptError<connect::Error>),
    /// Typed event failure; lag is never automatically swallowed.
    #[error(transparent)]
    Receive(#[from] ReceiveError),
    /// History/checkpoint failure; caller decides when to retry read-only work.
    #[error(transparent)]
    Sync(#[from] synchronization::Error),
}
/// Per-reconnect read-only work bound. Does not silently reset on a failed request.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    /// At most 32 pages including failed/cancelled requests.
    pub pages: u16,
    /// At most 4096 requested records, including failed requests.
    pub messages: u16,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            pages: 8,
            messages: 256,
        }
    }
}
/// Single owner prevents concurrent initialization and duplicate active sessions.
/// Existing event subscriptions are dropped before reconnection. New subscription
/// is created before room watermark/history reads; pushes buffer in the same
/// bounded driver ring and any overflow remains explicit.
pub struct Room {
    room: RoomId,
    session: Option<Session>,
    events: Option<Events>,
    tracker: Tracker,
    budget: Budget,
    pages_left: u16,
    messages_left: u16,
    generation: u64,
    closed: bool,
    blocked: bool,
}
impl Room {
    /// No network/task is started. Store contains only acknowledged checkpoints.
    pub fn new(
        room: RoomId,
        dedup_capacity: usize,
        budget: Budget,
        store: &impl CheckpointStore,
    ) -> Result<Self, Error> {
        if budget.pages == 0 || budget.pages > 32 || budget.messages == 0 || budget.messages > 4096
        {
            return Err(synchronization::Error::InvalidRequest.into());
        }
        Ok(Self {
            room,
            session: None,
            events: None,
            tracker: Tracker::new(room, dedup_capacity, store)?,
            budget,
            pages_left: budget.pages,
            messages_left: budget.messages,
            generation: 0,
            closed: false,
            blocked: false,
        })
    }
    /// Current generation, incremented only after an authenticated subscription.
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// Observe processing/dedup/gap state.
    pub fn tracker(&self) -> &Tracker {
        &self.tracker
    }
    /// Explicit processing acknowledgment interface; never automatically commits
    /// after merely receiving, querying or reconnecting.
    pub fn tracker_mut(&mut self) -> &mut Tracker {
        &mut self.tracker
    }
    /// Explicitly connect or replace a disconnected session. The &mut owner
    /// serializes attempts. A fresh connection never replays a previous request.
    /// The connector must not initiate HTTP password/device authentication here.
    pub async fn reconnect<F, Fut>(
        &mut self,
        policy: RetryPolicy,
        attempt_timeout: Duration,
        cancel: watch::Receiver<bool>,
        mut connector: F,
    ) -> Result<(), Error>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<Session, connect::Error>>,
    {
        if self.closed || self.blocked {
            return Err(Error::Intervention);
        }
        if policy.attempts > 8
            || attempt_timeout.is_zero()
            || attempt_timeout > Duration::from_secs(300)
            || policy.maximum > Duration::from_secs(30)
        {
            return Err(Error::Connect(AttemptError::InvalidPolicy));
        }
        if let Some(session) = &self.session {
            match session.state() {
                State::Authenticated => return Err(Error::AlreadyActive),
                State::Failed(AuthError::Interrupted) | State::Closed => {}
                _ => return Err(Error::Intervention),
            }
        }
        self.events = None;
        if let Some(mut old) = self.session.take() {
            let mut cleanup_cancel = cancel.clone();
            tokio::select! { biased;
                _ = crate::recovery::cancelled(&mut cleanup_cancel) => return Err(Error::Connect(AttemptError::Cancelled)),
                _ = old.disconnect() => {},
            }
        }
        self.tracker.abandon_page();
        let session = connect_with_retry(policy, cancel.clone(), || {
            let attempt = connector();
            async move {
                match tokio::time::timeout(attempt_timeout, attempt).await {
                    Ok(Ok(session)) => Ok(session),
                    Ok(Err(error)) => Err(classify(error)),
                    Err(_) => Err(AttemptError::Permanent(connect::Error::Timeout)),
                }
            }
        })
        .await;
        let mut session = match session {
            Ok(session) => session,
            Err(error) => {
                if matches!(error, AttemptError::Permanent(_)) {
                    self.blocked = true;
                }
                return Err(Error::Connect(error));
            }
        };
        if session.state() != State::Authenticated {
            self.blocked = true;
            session.disconnect().await;
            return Err(Error::InvalidSession);
        }
        let events = match session.message_events() {
            Ok(events) => events,
            Err(error) => {
                session.disconnect().await;
                return Err(error.into());
            }
        };
        self.generation = self.generation.checked_add(1).ok_or(Error::Intervention)?;
        self.pages_left = self.budget.pages;
        self.messages_left = self.budget.messages;
        self.events = Some(events);
        self.session = Some(session);
        Ok(())
    }
    /// One source-backed PING with the session's bounded request deadline. Call
    /// on an application timer; no idle polling or heartbeat task is spawned.
    /// Failure marks a suspected gap. It never retries a previous WRITE.
    pub async fn heartbeat(&mut self, mut cancel: watch::Receiver<bool>) -> Result<(), Error> {
        let packet = sculk_protocol::Packet::from_document(
            0,
            sculk_protocol::Command::new("PING").expect("constant command"),
            &bson::doc! {},
        )
        .map_err(|_| synchronization::Error::InvalidRequest)?;
        let session = self.session.as_ref().ok_or(ReceiveError::Inactive)?;
        let result = tokio::select! { biased;
            _ = crate::recovery::cancelled(&mut cancel) => Err(synchronization::Error::Cancelled),
            reply = session.request(packet) => reply.map(|_| ()).map_err(synchronization::Error::from),
        };
        if result.is_err() {
            self.tracker.suspect_gap();
        }
        Ok(result?)
    }
    /// Read the selected room's watermark through the active session. Does not
    /// enter a room, change read state, process messages or commit a checkpoint.
    pub async fn watermark(
        &self,
        cancel: watch::Receiver<bool>,
    ) -> Result<Option<MessageId>, Error> {
        Ok(self
            .session
            .as_ref()
            .ok_or(ReceiveError::Inactive)?
            .room_watermark(self.room, cancel)
            .await?)
    }
    /// Announce the current room watermark and fetch at most one recovery page.
    /// Call after reconnect; process/acknowledge before continuing. None means no
    /// newer announced log, not proof of complete historical recovery.
    pub async fn recover_page(
        &mut self,
        count: u16,
        cancel: watch::Receiver<bool>,
    ) -> Result<Option<Page>, Error> {
        match self.watermark(cancel.clone()).await? {
            Some(through) if through.get() > self.cursor().get() => {
                Ok(Some(self.synchronize(through, count, cancel).await?))
            }
            _ => Ok(None),
        }
    }
    /// One bounded recovery page from the last acknowledged cursor. The caller
    /// observes a target first, processes the page separately from live delivery,
    /// and calls Tracker::begin_page/acknowledge_history/finish_page explicitly.
    pub async fn synchronize(
        &mut self,
        through: MessageId,
        count: u16,
        cancel: watch::Receiver<bool>,
    ) -> Result<Page, Error> {
        let request = Request::new(self.room, self.tracker.cursor(), through, count)?;
        if self.pages_left == 0 || count > self.messages_left {
            return Err(synchronization::Error::Limit.into());
        }
        self.pages_left -= 1;
        self.messages_left -= count;
        self.tracker.suspect_gap();
        Ok(self
            .session
            .as_ref()
            .ok_or(ReceiveError::Inactive)?
            .synchronize(request, cancel)
            .await?)
    }
    /// Receive raw typed events without hiding duplicates, unknown commands,
    /// other rooms or lag. Tracker::observe applies the selected-room boundary.
    /// A failure marks a suspected gap; reconnect remains an explicit operation.
    pub async fn recv(&mut self, cancel: watch::Receiver<bool>) -> Result<Event, Error> {
        let result = self
            .events
            .as_mut()
            .ok_or(ReceiveError::Inactive)?
            .recv(cancel)
            .await;
        if result.is_err() {
            self.tracker.suspect_gap();
        }
        Ok(result?)
    }
    /// Permanently shut down this manager. Dropping an active request/session
    /// aborts the existing bounded driver; no background reconnect task survives.
    pub async fn shutdown(&mut self) {
        self.closed = true;
        self.events = None;
        self.tracker.abandon_page();
        if let Some(mut session) = self.session.take() {
            session.disconnect().await;
        }
    }
    /// Current committed boundary, useful for a new explicit recovery request.
    pub fn cursor(&self) -> Cursor {
        self.tracker.cursor()
    }
}

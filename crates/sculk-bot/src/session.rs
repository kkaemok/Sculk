//! Room-scoped adapters for one authenticated client session.
use crate::{
    BoxFuture, Connection, Event, EventSource, Outbound, OutboundError, OutboundReceipt,
    OutgoingMessage, RegistrationError, RoomId, SourceError,
};
use sculk_client::{
    messaging::{self, ReceiveError, SendError, TextSender},
    read_only,
    session::Session,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::Duration,
};
use tokio::{sync::watch, time::Instant};

/// A connected bot source and its verified room boundary.
pub struct SessionSource {
    session: Session,
    events: messaging::Events,
    rooms: Vec<RoomId>,
    initial: bool,
    ended: bool,
}

impl SessionSource {
    /// Take ownership of one authenticated session and verify access to the
    /// explicitly selected room with CHATINFO before allowing bot dispatch.
    /// Subscribe first so messages during room verification can report lag.
    pub async fn select(
        session: Session,
        room: RoomId,
        cancel: watch::Receiver<bool>,
    ) -> Result<Self, SelectRoomError> {
        Self::select_rooms(session, &[room], cancel).await
    }

    /// Verify up to sixteen explicitly authorized rooms on the same session.
    /// One bounded event subscription preserves arrival order within each room.
    pub async fn select_rooms(
        session: Session,
        rooms: &[RoomId],
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Self, SelectRoomError> {
        if !(1..=16).contains(&rooms.len())
            || rooms.iter().copied().collect::<HashSet<_>>().len() != rooms.len()
        {
            return Err(SelectRoomError::InvalidSelection);
        }
        let events = session.message_events()?;
        for &room in rooms {
            tokio::select! { biased;
                _ = crate::runtime::cancelled(&mut cancel) => return Err(SelectRoomError::Cancelled),
                result = session.room_info(room) => { result?; }
            }
        }
        Ok(Self {
            session,
            events,
            rooms: rooms.to_vec(),
            initial: true,
            ended: false,
        })
    }

    /// Explicitly enable historical basic WRITE for the selected room. Attempts
    /// are limited to one per interval, including rejected or ambiguous sends.
    /// The caller supplies independent authorization and profile verification.
    pub fn text_outbound(&self, interval: Duration) -> Result<SessionOutbound, RegistrationError> {
        let [room] = self.rooms.as_slice() else {
            return Err(RegistrationError::Invalid);
        };
        self.scoped_outbound(&[(*room, interval)], interval)
    }

    /// Authorize writes separately for a subset of selected rooms with an
    /// aggregate account limit and a per-room limit.
    pub fn scoped_outbound(
        &self,
        rooms: &[(RoomId, Duration)],
        account_interval: Duration,
    ) -> Result<SessionOutbound, RegistrationError> {
        if rooms.iter().any(|(room, _)| !self.rooms.contains(room)) {
            return Err(RegistrationError::Invalid);
        }
        let sender = self
            .session
            .text_sender()
            .map_err(|_| RegistrationError::Invalid)?;
        SessionOutbound::new_scoped(sender, rooms, account_interval)
    }
}

/// Why authenticated room selection failed.
#[derive(Debug, thiserror::Error)]
pub enum SelectRoomError {
    #[error("invalid authorized room selection")]
    InvalidSelection,
    #[error("room selection cancelled")]
    Cancelled,
    #[error(transparent)]
    Receive(#[from] ReceiveError),
    #[error(transparent)]
    Room(#[from] read_only::Error),
}

impl EventSource for SessionSource {
    fn recv(
        &mut self,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'_, Result<Option<Event>, SourceError>> {
        Box::pin(async move {
            if self.initial {
                self.initial = false;
                return Ok(Some(Event::Connection(Connection::State {
                    generation: 1,
                    state: self.session.state(),
                })));
            }
            if self.ended {
                return Ok(None);
            }
            loop {
                match self.events.recv(cancel.clone()).await {
                    Ok(messaging::Event::Message(message))
                        if !self.rooms.contains(&message.room) =>
                    {
                        continue
                    }
                    Ok(event)
                        if event
                            .membership()
                            .ok()
                            .flatten()
                            .is_some_and(|change| !self.rooms.contains(&change.room)) =>
                    {
                        continue
                    }
                    Ok(event) => return Ok(Some(event.into())),
                    Err(error) => {
                        self.ended = true;
                        return Ok(match error {
                            ReceiveError::Lagged(count) => Some(Event::Lagged(count)),
                            ReceiveError::Expired | ReceiveError::Inactive => {
                                Some(Event::Connection(Connection::Lost {
                                    expired: error == ReceiveError::Expired,
                                }))
                            }
                            ReceiveError::Cancelled => None,
                        });
                    }
                }
            }
        })
    }

    fn shutdown(&mut self) -> BoxFuture<'_, Result<(), SourceError>> {
        Box::pin(async move {
            self.session
                .disconnect_checked()
                .await
                .map_err(|_| SourceError::Shutdown)
        })
    }
}

/// Rate-limited, room-scoped adapter backed by one authenticated session.
pub struct SessionOutbound {
    sender: TextSender,
    account_interval: Duration,
    last_attempt: Mutex<Rates>,
}

struct Rates {
    account: Option<Instant>,
    rooms: HashMap<RoomId, (Duration, Option<Instant>)>,
}

impl SessionOutbound {
    /// Bind a generation's text-only handle to its authorized room and rate limit.
    pub fn new(
        sender: TextSender,
        room: RoomId,
        interval: Duration,
    ) -> Result<Self, RegistrationError> {
        Self::new_scoped(sender, &[(room, interval)], interval)
    }

    fn new_scoped(
        sender: TextSender,
        rooms: &[(RoomId, Duration)],
        account_interval: Duration,
    ) -> Result<Self, RegistrationError> {
        let valid = |interval: Duration| !interval.is_zero() && interval <= Duration::from_secs(60);
        if !valid(account_interval) || !(1..=16).contains(&rooms.len()) {
            return Err(RegistrationError::Invalid);
        }
        let mut rates = HashMap::new();
        for &(room, interval) in rooms {
            if !valid(interval) || rates.insert(room, (interval, None)).is_some() {
                return Err(RegistrationError::Invalid);
            }
        }
        Ok(Self {
            sender,
            account_interval,
            last_attempt: Mutex::new(Rates {
                account: None,
                rooms: rates,
            }),
        })
    }

    fn admit(&self, room: RoomId, cancel: &watch::Receiver<bool>) -> Result<(), OutboundError> {
        if *cancel.borrow() || cancel.has_changed().is_err() {
            return Err(OutboundError::Cancelled);
        }
        let mut rates = self
            .last_attempt
            .lock()
            .map_err(|_| OutboundError::NotSent)?;
        let now = Instant::now();
        if rates
            .account
            .is_some_and(|last| now.duration_since(last) < self.account_interval)
        {
            return Err(OutboundError::NotSent);
        }
        let (interval, last) = rates.rooms.get_mut(&room).ok_or(OutboundError::NotSent)?;
        if last.is_some_and(|last| now.duration_since(last) < *interval) {
            return Err(OutboundError::NotSent);
        }
        *last = Some(now);
        rates.account = Some(now);
        Ok(())
    }
}

fn outcome(error: SendError) -> OutboundError {
    match error {
        SendError::InvalidRequest | SendError::NotSent => OutboundError::NotSent,
        SendError::Cancelled => OutboundError::Cancelled,
        SendError::Unsupported(_) => OutboundError::Unavailable,
        SendError::Rejected(status) => OutboundError::Rejected(status),
        SendError::TimeoutUnknown => OutboundError::TimeoutUnknown,
        SendError::DeliveryUnknown => OutboundError::DeliveryUnknown,
    }
}

impl Outbound for SessionOutbound {
    fn send<'a>(
        &'a self,
        room: RoomId,
        text: &'a str,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<OutboundReceipt, OutboundError>> {
        Box::pin(async move {
            self.admit(room, &cancel)?;
            self.sender
                .send_text(room, text, cancel)
                .await
                .map(OutboundReceipt::ServerAcknowledged)
                .map_err(outcome)
        })
    }

    fn send_message<'a>(
        &'a self,
        room: RoomId,
        message: &'a OutgoingMessage,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<OutboundReceipt, OutboundError>> {
        Box::pin(async move {
            self.admit(room, &cancel)?;
            self.sender
                .send_message(room, message, cancel)
                .await
                .map(OutboundReceipt::ServerAcknowledged)
                .map_err(outcome)
        })
    }
}

//! Explicit single-room adapters for an authenticated client session.
use crate::{
    BoxFuture, Connection, Event, EventSource, Outbound, OutboundError, OutboundReceipt,
    RegistrationError, RoomId, SourceError,
};
use sculk_client::{
    messaging::{self, ReceiveError, SendError, TextSender},
    read_only,
    session::Session,
};
use std::{sync::Mutex, time::Duration};
use tokio::{sync::watch, time::Instant};

/// A connected bot source and its verified room boundary.
pub struct SessionSource {
    session: Session,
    events: messaging::Events,
    room: RoomId,
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
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Self, SelectRoomError> {
        let events = session.message_events()?;
        tokio::select! { biased;
            _ = crate::runtime::cancelled(&mut cancel) => return Err(SelectRoomError::Cancelled),
            result = session.room_info(room) => { result?; }
        }
        Ok(Self {
            session,
            events,
            room,
            initial: true,
            ended: false,
        })
    }

    /// Explicitly enable historical basic WRITE for the selected room. Attempts
    /// are limited to one per interval, including rejected or ambiguous sends.
    /// The caller supplies independent authorization and profile verification.
    pub fn text_outbound(&self, interval: Duration) -> Result<SessionOutbound, RegistrationError> {
        if interval.is_zero() || interval > Duration::from_secs(60) {
            return Err(RegistrationError::Invalid);
        }
        let sender = self
            .session
            .text_sender()
            .map_err(|_| RegistrationError::Invalid)?;
        Ok(SessionOutbound {
            sender,
            room: self.room,
            interval,
            last_attempt: Mutex::new(None),
        })
    }
}

/// Why authenticated room selection failed.
#[derive(Debug, thiserror::Error)]
pub enum SelectRoomError {
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
                    Ok(messaging::Event::Message(message)) if message.room != self.room => continue,
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
    room: RoomId,
    interval: Duration,
    last_attempt: Mutex<Option<Instant>>,
}

impl Outbound for SessionOutbound {
    fn send<'a>(
        &'a self,
        room: RoomId,
        text: &'a str,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<OutboundReceipt, OutboundError>> {
        Box::pin(async move {
            if room != self.room || *cancel.borrow() || cancel.has_changed().is_err() {
                return Err(OutboundError::NotSent);
            }
            {
                let mut last = self
                    .last_attempt
                    .lock()
                    .map_err(|_| OutboundError::NotSent)?;
                let now = Instant::now();
                if last.is_some_and(|last| now.duration_since(last) < self.interval) {
                    return Err(OutboundError::NotSent);
                }
                *last = Some(now);
            }
            self.sender
                .send_text(room, text, cancel)
                .await
                .map(OutboundReceipt::ServerAcknowledged)
                .map_err(|error| match error {
                    SendError::InvalidRequest | SendError::NotSent => OutboundError::NotSent,
                    SendError::Rejected(status) => OutboundError::Rejected(status),
                    SendError::TimeoutUnknown => OutboundError::TimeoutUnknown,
                    SendError::DeliveryUnknown => OutboundError::DeliveryUnknown,
                })
        })
    }
}

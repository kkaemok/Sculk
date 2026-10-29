//! One receive future, no adapter task. Caller-authorized sessions stay in client.
use crate::{BoxFuture, Event};
use tokio::sync::{mpsc, watch};

/// Redacted source/cleanup failures; application code can retain its own evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// Receive operation failed; continuity is unknown.
    #[error("event source failed")]
    Receive,
    /// Explicit cleanup failed; clean disconnection is not verified.
    #[error("event source shutdown failed")]
    Shutdown,
}
/// Cancellation-safe, bounded intake boundary. `recv` must yield to the executor,
/// preserve unknown events and report losses. Drop must clean up owned resources.
/// A finite source returns None; the bot then drains its dispatch queue.
pub trait EventSource: Send {
    /// One next event. Dropping the future must not leak a task or a connection.
    fn recv(
        &mut self,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'_, Result<Option<Event>, SourceError>>;
    /// Graceful cleanup; the runner applies its configured shutdown deadline.
    fn shutdown(&mut self) -> BoxFuture<'_, Result<(), SourceError>>;
}
/// Local bounded synthetic/application event source; no KakaoTalk connection.
pub struct ChannelSource {
    receiver: mpsc::Receiver<Event>,
}
/// Create 1..1024 event slots. Sender backpressure is explicit and cancellation
/// remains the producer's responsibility. Dropping all senders signals drain.
pub fn channel(
    capacity: usize,
) -> Result<(mpsc::Sender<Event>, ChannelSource), crate::RegistrationError> {
    if capacity == 0 || capacity > 1024 {
        return Err(crate::RegistrationError::Invalid);
    }
    let (sender, receiver) = mpsc::channel(capacity);
    Ok((sender, ChannelSource { receiver }))
}
impl EventSource for ChannelSource {
    fn recv(
        &mut self,
        mut cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'_, Result<Option<Event>, SourceError>> {
        Box::pin(async move {
            tokio::select! { biased;
                _=crate::runtime::cancelled(&mut cancel)=>Ok(None),
                event=self.receiver.recv()=>Ok(event),
            }
        })
    }
    fn shutdown(&mut self) -> BoxFuture<'_, Result<(), SourceError>> {
        self.receiver.close();
        Box::pin(async { Ok(()) })
    }
}

/// Optional adapter over an already explicitly connected managed client. Never
/// authenticates, reconnects, acknowledges checkpoints or replays history.
#[cfg(feature = "managed-client")]
pub struct ManagedSource {
    room: sculk_client::managed::Room,
    pending: std::collections::VecDeque<Event>,
    ended: bool,
}
#[cfg(feature = "managed-client")]
impl ManagedSource {
    /// Move an owner-controlled manager into the bot. Establish it explicitly
    /// beforehand; a zero-generation manager reports inactive on its first read.
    pub fn new(room: sculk_client::managed::Room) -> Self {
        let mut pending = std::collections::VecDeque::with_capacity(2);
        if let Some(state) = room.session_state() {
            pending.push_back(Event::Connection(crate::Connection::State {
                generation: room.generation(),
                state,
            }));
        }
        pending.push_back(Event::Synchronization(crate::Synchronization {
            room: room.room_id(),
            cursor: room.cursor(),
            gap: room.tracker().gap(),
        }));
        Self {
            room,
            pending,
            ended: false,
        }
    }
}
#[cfg(feature = "managed-client")]
impl EventSource for ManagedSource {
    fn recv(
        &mut self,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'_, Result<Option<Event>, SourceError>> {
        Box::pin(async move {
            if let Some(event) = self.pending.pop_front() {
                return Ok(Some(event));
            }
            if self.ended {
                return Ok(None);
            }
            match self.room.recv(cancel).await {
                Ok(event) => Ok(Some(event.into())),
                Err(sculk_client::managed::Error::Receive(
                    sculk_client::messaging::ReceiveError::Lagged(n),
                )) => Ok(Some(Event::Lagged(n))),
                Err(sculk_client::managed::Error::Receive(
                    sculk_client::messaging::ReceiveError::Expired,
                )) => {
                    self.ended = true;
                    Ok(Some(Event::Connection(crate::Connection::Lost {
                        expired: true,
                    })))
                }
                Err(sculk_client::managed::Error::Receive(
                    sculk_client::messaging::ReceiveError::Inactive,
                )) => {
                    self.ended = true;
                    Ok(Some(Event::Connection(crate::Connection::Lost {
                        expired: false,
                    })))
                }
                Err(sculk_client::managed::Error::Receive(
                    sculk_client::messaging::ReceiveError::Cancelled,
                )) => Ok(None),
                Err(_) => Err(SourceError::Receive),
            }
        })
    }
    fn shutdown(&mut self) -> BoxFuture<'_, Result<(), SourceError>> {
        Box::pin(async move {
            self.room
                .shutdown_checked()
                .await
                .map_err(|_| SourceError::Shutdown)
        })
    }
}

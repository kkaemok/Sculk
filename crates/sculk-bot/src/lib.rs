//! Bounded bot dispatch over the existing Sculk client. No account authentication,
//! live WRITE adapter, interpreter or automatic recovery is started by this crate.
//! See `docs/bot.md` for ordering, failure and delivery semantics.
//!
//! ```
//! use sculk_bot::{Bot, Config, Command, PermissionId};
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut bot = Bot::new(Config::default())?;
//! let ping = PermissionId::new("commands.ping")?;
//! bot.permission(ping.clone(), true)?;
//! bot.register(Command::new("ping").alias("p").require(ping), |ctx| async move {
//!     // Configure an explicit Outbound adapter before running this handler.
//!     ctx.reply("Pong!").await?;
//!     Ok(())
//! })?;
//! # Ok(()) }
//! ```
mod command;
mod permissions;
mod runtime;
mod source;

use command::Invocation;
pub use command::{parse, Command, ParseError, Parsed};
pub use permissions::{Decision, MemoryPermissions, PermissionId, PermissionStore};
pub use runtime::{Bot, Config, Failure, HandlerError, RegistrationError, RunReport, StopReason};
pub use sculk_client::{
    messaging::{Event as ClientEvent, Message, MessageId, MessageType, SenderId, TextReceipt},
    read_only::RoomId,
    session::{AuthError, State as SessionState},
    synchronization::{Cursor, Gap},
};
#[cfg(feature = "managed-client")]
pub use source::ManagedSource;
pub use source::{channel, ChannelSource, EventSource, SourceError};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::sync::watch;

/// Boxed async boundary shared by Rust handlers and a future script host.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// Application/source connection observation; no invented server event schemas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Connection {
    /// Validated local session state and its explicit reconnect generation.
    State {
        generation: u64,
        state: SessionState,
    },
    /// Underlying session expired or disconnected; no automatic login follows.
    Lost { expired: bool },
}
/// Read-only snapshot of the existing tracker, not a recovered message delivery.
#[derive(Clone, Debug)]
pub struct Synchronization {
    /// Explicitly selected room.
    pub room: RoomId,
    /// Acknowledged cursor, not latest received timestamp.
    pub cursor: Cursor,
    /// Continuity evidence reported by the client tracker.
    pub gap: Gap,
}
/// Verified client event subset plus explicit local lifecycle notifications.
#[derive(Clone, Debug)]
pub enum Event {
    /// Real-time message. History must not be submitted as an ordinary command.
    Message(Message),
    /// Local connection observation.
    Connection(Connection),
    /// Explicit history/checkpoint notification, never automatically executed.
    Synchronization(Synchronization),
    /// Unknown or malformed packets preserved in the existing client envelope.
    Unknown(sculk_client::messaging::Event),
    /// Underlying receive ring overwrote this many packets; recovery is incomplete.
    Lagged(u64),
}
impl From<sculk_client::messaging::Event> for Event {
    fn from(event: sculk_client::messaging::Event) -> Self {
        match event {
            sculk_client::messaging::Event::Message(m) => Self::Message(m),
            other => Self::Unknown(other),
        }
    }
}
/// Startup subscription filter. General handlers execute before command handlers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    All,
    Message,
    Connection,
    Synchronization,
    Unknown,
    Lagged,
}
impl EventKind {
    pub(crate) fn matches(self, event: &Event) -> bool {
        self == Self::All
            || matches!(
                (self, event),
                (Self::Message, Event::Message(_))
                    | (Self::Connection, Event::Connection(_))
                    | (Self::Synchronization, Event::Synchronization(_))
                    | (Self::Unknown, Event::Unknown(_))
                    | (Self::Lagged, Event::Lagged(_))
            )
    }
}
/// Outbound evidence must distinguish an offline operation from acknowledgment.
#[derive(Debug, PartialEq, Eq)]
pub enum OutboundReceipt {
    /// Only a mock adapter accepted the text. Nothing reached KakaoTalk.
    MockAccepted,
    /// Reserved for an explicitly supplied adapter with actual server evidence.
    ServerAcknowledged(TextReceipt),
}
/// No raw payload/error source is included in default diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OutboundError {
    /// Default adapter: live text sending is intentionally unavailable.
    #[error("no outbound adapter configured; live WRITE is unverified")]
    Unavailable,
    /// Local adapter capacity/policy refusal before submission.
    #[error("outbound request not submitted")]
    NotSent,
    /// Cancellation/timeout after submission can leave delivery unknown.
    #[error("outbound delivery unknown; do not automatically resend")]
    DeliveryUnknown,
}
/// Explicit application-supplied send boundary. No built-in live implementation.
/// Adapters must not retry non-idempotent requests and must be drop-safe.
pub trait Outbound: Send + Sync {
    /// Submit once; mock acceptance is never a real server acknowledgment.
    fn send<'a>(
        &'a self,
        room: RoomId,
        text: &'a str,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<OutboundReceipt, OutboundError>>;
}
pub(crate) struct DisabledOutbound;
impl Outbound for DisabledOutbound {
    fn send<'a>(
        &'a self,
        _: RoomId,
        _: &'a str,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<OutboundReceipt, OutboundError>> {
        Box::pin(async { Err(OutboundError::Unavailable) })
    }
}
/// Shared event and application facilities. Message bytes remain in the client's
/// reference-counted packet; handlers do not copy content to obtain a Context.
#[derive(Clone)]
pub struct Context {
    pub(crate) event: Arc<Event>,
    pub(crate) invocation: Option<Arc<Invocation>>,
    pub(crate) failure: Option<Failure>,
    pub(crate) permissions: Arc<permissions::Permissions>,
    pub(crate) outbound: Arc<dyn Outbound>,
    pub(crate) cancel: watch::Receiver<bool>,
}
impl Context {
    /// Current typed event, including preserved unknown metadata.
    pub fn event(&self) -> &Event {
        &self.event
    }
    /// Validated real-time message, if present.
    pub fn message(&self) -> Option<&Message> {
        if let Event::Message(m) = self.event() {
            Some(m)
        } else {
            None
        }
    }
    /// Exact room when provided by the event.
    pub fn room(&self) -> Option<RoomId> {
        match self.event() {
            Event::Message(m) => Some(m.room),
            Event::Synchronization(s) => Some(s.room),
            _ => None,
        }
    }
    /// Source room-scoped sender, without invented administrative roles.
    pub fn sender(&self) -> Option<SenderId> {
        self.message().map(|m| m.sender)
    }
    /// Borrow message text from the original packet.
    pub fn content(&self) -> Option<&str> {
        self.message().and_then(Message::text)
    }
    /// Arguments bound to this message. Only command/error contexts have arguments.
    pub fn arguments(&self) -> impl Iterator<Item = &str> {
        self.invocation
            .iter()
            .flat_map(move |i| i.arguments(self.content().unwrap_or("")))
    }
    /// Name as entered, without prefix. Aliases resolve before command invocation.
    pub fn command_name(&self) -> Option<&str> {
        self.invocation
            .as_ref()
            .map(|i| i.name(self.content().unwrap_or("")))
    }
    /// Current failure for registered error handlers.
    pub fn failure(&self) -> Option<Failure> {
        self.failure
    }
    /// Fail closed for unregistered permissions or events without a sender.
    pub fn has_permission(&self, permission: &PermissionId) -> bool {
        self.room()
            .zip(self.sender())
            .is_some_and(|(r, s)| self.permissions.allows(permission, r, s))
    }
    /// Shared cancellation signal; a disconnected sender is also cancellation.
    pub fn cancellation(&self) -> watch::Receiver<bool> {
        self.cancel.clone()
    }
    /// Level-triggered cancellation state.
    pub fn is_cancelled(&self) -> bool {
        *self.cancel.borrow() || self.cancel.has_changed().is_err()
    }
    /// Wait without polling. The dispatcher also cancels a pending handler future.
    pub async fn cancelled(&self) {
        let mut rx = self.cancel.clone();
        runtime::cancelled(&mut rx).await;
    }
    /// Use the explicitly configured adapter once. Cancellation after submission
    /// returns unknown delivery; neither this method nor the dispatcher retries.
    pub async fn reply(&self, text: &str) -> Result<OutboundReceipt, OutboundError> {
        if self.is_cancelled() {
            return Err(OutboundError::NotSent);
        }
        let room = self.room().ok_or(OutboundError::NotSent)?;
        if text.is_empty() || text.len() > 16384 || text.contains('\0') {
            return Err(OutboundError::NotSent);
        }
        let mut cancel = self.cancel.clone();
        tokio::select! { biased;
            _=runtime::cancelled(&mut cancel)=>Err(OutboundError::DeliveryUnknown),
            result=self.outbound.send(room,text,self.cancel.clone())=>result,
        }
    }
}

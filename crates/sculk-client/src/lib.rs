//! Experimental request transport for already-secured streams and offline servers.
//!
//! The optional Android provider is experimental; current live compatibility is unverified. Never send tokens over a
//! raw TCP stream. Timed-out or interrupted writes have unknown delivery status
//! and are never replayed automatically.
use futures_util::{SinkExt, StreamExt};
use sculk_protocol::{Command, LocoCodec, Packet};
use std::{collections::HashMap, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{broadcast, mpsc, oneshot, watch},
    time::{self, Instant},
};
use tokio_util::codec::Framed;

#[cfg(feature = "android-auth")]
pub mod auth;
#[cfg(feature = "checkpoint-file")]
pub mod checkpoint;
#[cfg(feature = "protocol-connect")]
pub mod connect;
#[cfg(feature = "android-auth")]
pub mod dial;
#[cfg(feature = "session-recovery")]
pub mod managed;
#[cfg(feature = "messaging")]
pub mod messaging;
#[cfg(feature = "read-only")]
pub mod read_only;
#[cfg(feature = "protocol-connect")]
pub mod records;
pub mod recovery;
#[cfg(feature = "secure-session")]
pub mod session;
#[cfg(feature = "synchronization")]
pub mod synchronization;
#[cfg(feature = "secure-session")]
pub mod tls;

/// Transport state; a connected stream does not imply an authenticated session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    /// A stream is attached.
    Connected,
    /// Shutdown was requested or all handles were dropped.
    Closed,
    /// Peer, protocol or bounded-write failure terminated the stream.
    Lost,
}
/// Bounded resource and timing settings.
#[derive(Clone, Debug)]
pub struct Config {
    /// Pending and queued requests are each bounded by this limit.
    pub requests: usize,
    /// Requested broadcast slots (Tokio rounds up to a power of two). Slow
    /// receivers receive explicit lag errors. Payloads can approach 1 MiB each;
    /// reduce this on small hosts and handle gaps explicitly.
    pub events: usize,
    /// Total request deadline, including admission, writing and response.
    pub request_timeout: Duration,
    /// Maximum time the driver may spend writing a frame.
    pub write_timeout: Duration,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            requests: 32,
            events: 64,
            request_timeout: Duration::from_secs(15),
            write_timeout: Duration::from_secs(5),
        }
    }
}
/// Request failure. No variant implies that a timed-out write is safe to repeat.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    /// Invalid zero capacities or durations.
    #[error("invalid transport configuration")]
    Config,
    /// The connection has ended.
    #[error("connection closed; delivery may be unknown")]
    Closed,
    /// No response before the total deadline. Delivery is unknown.
    #[error("request deadline elapsed; delivery unknown")]
    Timeout,
    /// Bounded admission failed; this request was not written.
    #[error("request capacity exhausted; not sent")]
    Overloaded,
    /// The correlation ID matched, but the response command did not.
    #[error("response command mismatch")]
    Mismatch,
    /// A peer returned nonzero status.
    #[error("request rejected with status {0}")]
    Rejected(u16),
    /// This API cannot establish a verified KakaoTalk login.
    #[error("current KakaoTalk authentication is unverified; see docs/compatibility.md")]
    AuthenticationUnavailable,
}
/// Explicit close outcome, without transport error strings or payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ShutdownError {
    /// Driver ended before it could acknowledge this close request.
    #[error("connection already ended before checked shutdown")]
    Closed,
    /// Closing exceeded the configured bounded write deadline.
    #[error("connection shutdown timed out")]
    Timeout,
    /// The stream refused or failed AsyncWrite shutdown.
    #[error("connection shutdown transport failed")]
    Transport,
    /// An abort interrupted graceful shutdown.
    #[error("connection shutdown cancelled")]
    Cancelled,
}
/// Unmatched packets, including broadcasts and late responses. No event semantics
/// are inferred from a database feedType or unverified LOCO command name.
#[derive(Clone, Debug)]
pub enum Event {
    /// Raw validated packet; may contain sensitive application data.
    UnmatchedPacket(Packet),
}
struct Request {
    packet: Packet,
    deadline: Instant,
    reply: oneshot::Sender<Result<Packet, Error>>,
}
enum Control {
    Request(Request),
    Shutdown(oneshot::Sender<Result<(), ShutdownError>>),
}
struct Pending {
    command: Command,
    deadline: Instant,
    reply: oneshot::Sender<Result<Packet, Error>>,
}

/// Cloneable low-level client. One bounded task owns the stream. Dropping the last
/// handle closes the connection; dropping a request future does not undo a write.
#[derive(Clone)]
pub struct Client {
    tx: mpsc::Sender<Control>,
    events: broadcast::Sender<Event>,
    state: watch::Receiver<ConnectionState>,
    timeout: Duration,
    #[allow(dead_code)] // Sender lifetime also signals final Client drop.
    stop: watch::Sender<bool>,
}
impl Client {
    /// Attach an already-secured, authenticated stream, or an explicitly local
    /// mock stream. This function makes no authentication or encryption claims.
    pub fn from_stream<S>(stream: S, config: Config) -> Result<Self, Error>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        if config.requests == 0
            || config.events == 0
            || config.request_timeout.is_zero()
            || config.write_timeout.is_zero()
            || config.request_timeout > Duration::from_secs(86400)
            || config.write_timeout > Duration::from_secs(86400)
            || config.requests > 65536
            || config.events > 65536
        {
            return Err(Error::Config);
        }
        let (tx, rx) = mpsc::channel(config.requests);
        let (events, _) = broadcast::channel(config.events);
        let (state_tx, state) = watch::channel(ConnectionState::Connected);
        let (stop, stopped) = watch::channel(false);
        tokio::spawn(drive(
            stream,
            rx,
            events.clone(),
            state_tx,
            config.clone(),
            stopped,
        ));
        Ok(Self {
            tx,
            events,
            state,
            timeout: config.request_timeout,
            stop,
        })
    }
    /// Current transport state; separately observe changes with `connection_state`.
    pub fn state(&self) -> ConnectionState {
        self.state.borrow().clone()
    }
    /// Reliable latest-state channel, separate from optional packet diagnostics.
    pub fn connection_state(&self) -> watch::Receiver<ConnectionState> {
        self.state.clone()
    }
    /// Subscribe from now; `RecvError::Lagged` explicitly reports lost packets.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }
    /// Send once and correlate the response by both ID and command. The packet ID
    /// is assigned internally and is never reused on a stream. No retries occur.
    pub async fn request(&self, packet: Packet) -> Result<Packet, Error> {
        let deadline = Instant::now() + self.timeout;
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(Control::Request(Request {
                packet,
                deadline,
                reply,
            }))
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => Error::Overloaded,
                mpsc::error::TrySendError::Closed(_) => Error::Closed,
            })?;
        time::timeout_at(deadline, rx)
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(|_| Error::Closed)?
    }
    /// Close the stream, resolve outstanding requests, and wait for driver exit.
    pub async fn disconnect(&self) {
        let _ = self.disconnect_checked().await;
    }
    /// Report whether the driver completed AsyncWrite shutdown. This establishes
    /// local graceful-close completion, not receipt/processing by the remote peer.
    pub async fn disconnect_checked(&self) -> Result<(), ShutdownError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Control::Shutdown(tx))
            .await
            .map_err(|_| ShutdownError::Closed)?;
        rx.await.map_err(|_| ShutdownError::Closed)?
    }
    // Synchronous cancellation for session guards, including dropped futures.
    #[cfg(feature = "secure-session")]
    pub(crate) fn abort(&self) {
        self.stop.send_replace(true);
    }
    /// Explicitly refuse unverified authentication. No network call is made.
    pub async fn authenticate(&self) -> Result<(), Error> {
        Err(Error::AuthenticationUnavailable)
    }
}
async fn drive<S>(
    stream: S,
    mut rx: mpsc::Receiver<Control>,
    events: broadcast::Sender<Event>,
    state: watch::Sender<ConnectionState>,
    config: Config,
    mut stopped: watch::Receiver<bool>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut framed = Framed::new(stream, LocoCodec::default());
    let mut pending: HashMap<u32, Pending> = HashMap::new();
    let mut next_id = 1u32;
    let mut shutdown = None;
    let end = loop {
        let deadline = pending
            .values()
            .map(|p| p.deadline)
            .min()
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(86400));
        tokio::select! {
            _ = recovery::cancelled(&mut stopped) => break ConnectionState::Closed,
            control = rx.recv() => match control {
                None => break ConnectionState::Closed,
                Some(Control::Shutdown(ack)) => { shutdown = Some(ack); break ConnectionState::Closed; }
                Some(Control::Request(mut request)) => {
                    pending.retain(|_, p| !p.reply.is_closed());
                    let expired: Vec<_> = pending.iter().filter(|(_, p)| p.deadline <= Instant::now()).map(|(id, _)| *id).collect();
                    for id in expired { if let Some(p) = pending.remove(&id) { let _ = p.reply.send(Err(Error::Timeout)); } }
                    if request.reply.is_closed() { continue; }
                    if request.deadline <= Instant::now() { let _ = request.reply.send(Err(Error::Timeout)); continue; }
                    if pending.len() >= config.requests { let _ = request.reply.send(Err(Error::Overloaded)); continue; }
                    // Exhaustion closes the stream instead of risking ID reuse.
                    let Some(next) = next_id.checked_add(1) else { break ConnectionState::Lost; };
                    request.packet.id = next_id;
                    let id = next_id;
                    next_id = next;
                    let command = request.packet.command;
                    let write_deadline = request.deadline.min(Instant::now() + config.write_timeout);
                    if !matches!(time::timeout_at(write_deadline, async {
                        tokio::select! { biased;
                            _ = recovery::cancelled(&mut stopped) => Err(sculk_protocol::Error::Truncated),
                            result = framed.send(request.packet) => result,
                        }
                    }).await, Ok(Ok(()))) {
                        let _ = request.reply.send(Err(Error::Closed));
                        break ConnectionState::Lost;
                    }
                    pending.insert(id, Pending { command, deadline: request.deadline, reply: request.reply });
                }
            },
            packet = framed.next() => match packet {
                Some(Ok(packet)) => {
                    if let Some(p) = pending.remove(&packet.id) {
                        let result = if p.deadline <= Instant::now() { Err(Error::Timeout) }
                            else if packet.command != p.command { Err(Error::Mismatch) }
                            else if packet.status != 0 { Err(Error::Rejected(packet.status)) }
                            else { Ok(packet) };
                        let _ = p.reply.send(result);
                    } else { let _ = events.send(Event::UnmatchedPacket(packet)); }
                }
                _ => break ConnectionState::Lost,
            },
            _ = time::sleep_until(deadline) => {
                let now = Instant::now();
                let expired: Vec<_> = pending.iter().filter(|(_, p)| p.deadline <= now || p.reply.is_closed()).map(|(id, _)| *id).collect();
                for id in expired { if let Some(p) = pending.remove(&id) { let _ = p.reply.send(Err(Error::Timeout)); } }
            }
        }
    };
    // Explicit disconnect sends TLS close_notify / AsyncWrite shutdown, bounded
    // by the existing write deadline. Abort/framing/write failures still drop
    // immediately; never flush a partially failed non-idempotent write.
    let close_result = if shutdown.is_some() {
        tokio::select! { biased;
            _ = recovery::cancelled(&mut stopped) => Err(ShutdownError::Cancelled),
            result = time::timeout(config.write_timeout, framed.close()) => match result {
                Ok(Ok(())) => Ok(()), Ok(Err(_)) => Err(ShutdownError::Transport),
                Err(_) => Err(ShutdownError::Timeout),
            },
        }
    } else {
        Err(ShutdownError::Closed)
    };
    drop(framed);
    rx.close();
    for (_, p) in pending {
        let _ = p.reply.send(Err(Error::Closed));
    }
    // Discard queued requests before acknowledging shutdown.
    while let Ok(control) = rx.try_recv() {
        if let Control::Request(r) = control {
            let _ = r.reply.send(Err(Error::Closed));
        }
    }
    state.send_replace(if shutdown.is_some() && close_result.is_err() {
        ConnectionState::Lost
    } else {
        end
    });
    if let Some(ack) = shutdown {
        let _ = ack.send(close_result);
    }
}

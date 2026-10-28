//! Historical MSG/WRITE subset, corroborated by pinned node-kakao and Agent
//! Messenger sources in docs/research.md. Offline tested; live unverified.
//! Decoding happens on the consumer, never inside the network driver. Unknown
//! packets and malformed known events retain their original validated BSON.
use crate::{read_only::RoomId, session::State};
use bson::raw::{RawBsonRef, RawDocument};
use sculk_protocol::{Command, Packet};
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;

macro_rules! identifier {
    ($name:ident) => {
        /// Positive signed 64-bit wire identifier; Debug redacts the value.
        #[derive(Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(i64);
        impl $name {
            /// Preserve the exact BSON integer, rejecting nonpositive values.
            pub fn new(value: i64) -> Result<Self, InvalidMessage> {
                if value > 0 {
                    Ok(Self(value))
                } else {
                    Err(InvalidMessage)
                }
            }
            /// Explicit identity access. Never log automatically.
            pub fn get(self) -> i64 {
                self.0
            }
        }
        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(concat!(stringify!($name), "([REDACTED])"))
            }
        }
    };
}
identifier!(SenderId);
identifier!(MessageId);

/// Source-backed message types. Other numeric types are preserved, not guessed
/// from Android database feedType. No join/kick/moderation action is inferred.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageType {
    /// LOCO chatLog.type=0; feed content remains opaque.
    Feed,
    /// Plain text, type 1.
    Text,
    /// Photo, type 2.
    Photo,
    /// Video, type 3.
    Video,
    /// Audio, type 5.
    Audio,
    /// File, type 18.
    File,
    /// Reply, type 26; attachment remains opaque.
    Reply,
    /// Other raw type, including deleted-message flags.
    Unknown(i32),
}
impl From<i32> for MessageType {
    fn from(value: i32) -> Self {
        match value {
            0 => Self::Feed,
            1 => Self::Text,
            2 => Self::Photo,
            3 => Self::Video,
            5 => Self::Audio,
            18 => Self::File,
            26 => Self::Reply,
            n => Self::Unknown(n),
        }
    }
}
/// Invalid or ambiguous MSG schema. No peer content appears in diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid message event")]
pub struct InvalidMessage;

fn fields(doc: &RawDocument) -> Result<(), InvalidMessage> {
    let mut keys = std::collections::HashSet::new();
    for field in doc.iter() {
        let (key, _) = field.map_err(|_| InvalidMessage)?;
        if keys.len() >= 128 || !keys.insert(key) {
            return Err(InvalidMessage);
        }
    }
    Ok(())
}
fn value<'a>(doc: &'a RawDocument, key: &str) -> Result<Option<RawBsonRef<'a>>, InvalidMessage> {
    doc.get(key).map_err(|_| InvalidMessage)
}
fn integer(doc: &RawDocument, key: &str) -> Result<i64, InvalidMessage> {
    match value(doc, key)? {
        Some(RawBsonRef::Int32(n)) => Ok(i64::from(n)),
        Some(RawBsonRef::Int64(n)) => Ok(n),
        _ => Err(InvalidMessage),
    }
}
fn string<'a>(doc: &'a RawDocument, key: &str) -> Result<Option<&'a str>, InvalidMessage> {
    match value(doc, key)? {
        None => Ok(None),
        Some(RawBsonRef::String(s)) => Ok(Some(s)),
        _ => Err(InvalidMessage),
    }
}
fn optional_bool(doc: &RawDocument, key: &str) -> Result<Option<bool>, InvalidMessage> {
    match value(doc, key)? {
        None => Ok(None),
        Some(RawBsonRef::Boolean(b)) => Ok(Some(b)),
        _ => Err(InvalidMessage),
    }
}
fn body(packet: &Packet) -> Result<&RawDocument, InvalidMessage> {
    RawDocument::from_bytes(packet.body()).map_err(|_| InvalidMessage)
}
fn chat_log(doc: &RawDocument) -> Result<&RawDocument, InvalidMessage> {
    match value(doc, "chatLog")? {
        Some(RawBsonRef::Document(d)) => Ok(d),
        _ => Err(InvalidMessage),
    }
}

/// A zero-copy view owning one reference-counted packet. Content, attachments and
/// extra metadata are borrowed on demand. Debug never prints message content.
#[derive(Clone, Debug)]
pub struct Message {
    packet: Packet,
    /// Exact room ID, not an OpenChat link ID.
    pub room: RoomId,
    /// Room-scoped sender ID; no global account-identity assumption.
    pub sender: SenderId,
    /// Server chat-log ID, not the transport correlation or client msgId.
    pub id: MessageId,
    /// Source-backed type classification.
    pub kind: MessageType,
    /// Server sendAt in Unix seconds, not milliseconds or local receipt time.
    pub sent_at_seconds: i64,
}
impl Message {
    /// Parse only MSG. Required subset: chatId, chatLog(logId, authorId, type,
    /// sendAt); optional source fields are checked if present. Some public push
    /// handlers omit nested chatId and outer logId, so these are optional echoes
    /// and must agree when present. No missing content or timestamp is fabricated.
    pub fn from_packet(packet: Packet) -> Result<Self, InvalidMessage> {
        if packet.command.as_str() != "MSG" || packet.status != 0 {
            return Err(InvalidMessage);
        }
        let doc = body(&packet)?;
        fields(doc)?;
        if value(doc, "status")?.is_some() && integer(doc, "status")? != 0 {
            return Err(InvalidMessage);
        }
        let log = chat_log(doc)?;
        fields(log)?;
        let room = RoomId::new(integer(doc, "chatId")?).map_err(|_| InvalidMessage)?;
        let id = MessageId::new(integer(log, "logId")?)?;
        let sender = SenderId::new(integer(log, "authorId")?)?;
        let kind =
            MessageType::from(i32::try_from(integer(log, "type")?).map_err(|_| InvalidMessage)?);
        let sent_at_seconds = integer(log, "sendAt")?;
        if sent_at_seconds < 0 {
            return Err(InvalidMessage);
        }
        for (d, key, expected) in [(doc, "logId", id.get()), (log, "chatId", room.get())] {
            if value(d, key)?.is_some() && integer(d, key)? != expected {
                return Err(InvalidMessage);
            }
        }
        let text = string(log, "message")?;
        if kind == MessageType::Text && text.is_none() {
            return Err(InvalidMessage);
        }
        string(log, "attachment")?;
        string(log, "supplement")?;
        string(doc, "authorNickname")?;
        optional_bool(doc, "noSeen")?;
        optional_bool(doc, "notiRead")?;
        for key in ["msgId", "prevId", "referer"] {
            if value(log, key)?.is_some() {
                integer(log, key)?;
            }
        }
        if value(doc, "li")?.is_some() {
            integer(doc, "li")?;
        }
        Ok(Self {
            packet,
            room,
            sender,
            id,
            kind,
            sent_at_seconds,
        })
    }
    fn log(&self) -> &RawDocument {
        chat_log(body(&self.packet).expect("validated packet")).expect("validated MSG")
    }
    /// Optional message content, borrowed without materializing BSON.
    pub fn text(&self) -> Option<&str> {
        string(self.log(), "message").expect("validated MSG")
    }
    /// Raw attachment string. JSON interpretation is intentionally left to callers;
    /// unknown/invalid attachment JSON is preserved, never silently discarded.
    pub fn attachment(&self) -> Option<&str> {
        string(self.log(), "attachment").expect("validated MSG")
    }
    /// All source metadata, including unknown fields, without copying the payload.
    pub fn packet(&self) -> &Packet {
        &self.packet
    }
}
/// Typed event or preserved raw packet. Malformed MSG cannot masquerade as text.
#[derive(Clone, Debug)]
pub enum Event {
    /// Validated historical MSG subset.
    Message(Message),
    /// Unrecognized command, including late WRITE responses. Not an inferred action.
    Unknown(Packet),
    /// Known MSG with invalid/ambiguous fields; raw evidence retained.
    Malformed(Packet),
}
impl Event {
    /// Decode on the consumer, outside the network receive task.
    pub fn from_packet(packet: Packet) -> Self {
        if packet.command.as_str() != "MSG" {
            return Self::Unknown(packet);
        }
        match Message::from_packet(packet.clone()) {
            Ok(message) => Self::Message(message),
            Err(_) => Self::Malformed(packet),
        }
    }
}
/// Predictable receive outcomes, with no silent catch-up or automatic replay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReceiveError {
    /// This many packets were overwritten in the bounded ring; continuity is lost.
    #[error("message receiver lagged by {0} packets")]
    Lagged(u64),
    /// Cancellation is level-triggered, including disappearance of its sender.
    #[error("message reception cancelled")]
    Cancelled,
    /// The session is closed, unauthenticated or interrupted.
    #[error("message reception requires an active authenticated session")]
    Inactive,
    /// The session lease expired; queued packets are not delivered afterward.
    #[error("message session expired")]
    Expired,
}
/// One subscription to the existing bounded ring. No task/channel per subscriber.
/// Subscription starts now; initial LOGINLIST pushes and reconnect history are not
/// replayed. Retaining returned events is application-owned memory.
pub struct Events {
    packets: broadcast::Receiver<crate::Event>,
    state: watch::Receiver<State>,
    connection: watch::Receiver<crate::ConnectionState>,
    deadline: Instant,
}
impl Events {
    pub(crate) fn new(
        packets: broadcast::Receiver<crate::Event>,
        state: watch::Receiver<State>,
        connection: watch::Receiver<crate::ConnectionState>,
        deadline: Instant,
    ) -> Self {
        Self {
            packets,
            state,
            connection,
            deadline,
        }
    }
    /// Cancellation-safe: dropping a pending recv does not consume a packet.
    /// After Lagged, recv can resume from the oldest retained packet; the caller
    /// must decide how to reconcile the gap. No handler runs inside the driver.
    pub async fn recv(&mut self, mut cancel: watch::Receiver<bool>) -> Result<Event, ReceiveError> {
        loop {
            if self.deadline <= Instant::now() {
                return Err(ReceiveError::Expired);
            }
            match &*self.state.borrow_and_update() {
                State::Authenticated => {}
                State::Failed(crate::session::AuthError::Expired) => {
                    return Err(ReceiveError::Expired)
                }
                _ => return Err(ReceiveError::Inactive),
            }
            if *self.connection.borrow_and_update() != crate::ConnectionState::Connected {
                return Err(ReceiveError::Inactive);
            }
            tokio::select! { biased;
                _ = crate::recovery::cancelled(&mut cancel) => return Err(ReceiveError::Cancelled),
                _ = tokio::time::sleep_until(self.deadline) => return Err(ReceiveError::Expired),
                changed = self.state.changed() => { if changed.is_err() { return Err(ReceiveError::Inactive); } },
                changed = self.connection.changed() => { if changed.is_err() { return Err(ReceiveError::Inactive); } },
                result = self.packets.recv() => return match result {
                    Ok(crate::Event::UnmatchedPacket(packet)) => Ok(Event::from_packet(packet)),
                    Err(broadcast::error::RecvError::Lagged(n)) => Err(ReceiveError::Lagged(n)),
                    Err(broadcast::error::RecvError::Closed) => Err(ReceiveError::Inactive),
                },
            }
        }
    }
}

/// Offline text WRITE builder. This milestone exposes no high-level live send
/// method or CLI send flag. Encoding alone sends nothing. Low-level Session::request
/// remains caller-controlled and must not be used live without separate approval.
pub struct TextRequest {
    room: RoomId,
    text: String,
}
/// Text/receipt failures distinguish explicit rejection and ambiguous delivery.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// Local input failed before serialization; nothing sent by this builder.
    #[error("invalid text request")]
    InvalidRequest,
    /// Admission/authentication refused before writing.
    #[error("text request was not sent")]
    NotSent,
    /// Nonzero header/BSON status, preserved without speculative interpretation.
    #[error("text request rejected with status {0}")]
    Rejected(i64),
    /// Deadline exceeded after submission. Never automatically resend.
    #[error("text request timed out; delivery unknown; do not automatically resend")]
    TimeoutUnknown,
    /// Connection loss, expiry or malformed/mismatched acknowledgment after
    /// submission; absence of evidence is not evidence that nothing was sent.
    #[error("text delivery unknown; do not automatically resend")]
    DeliveryUnknown,
}
impl TextRequest {
    /// Local policy: nonempty text, at most 16 KiB UTF-8, no NUL. This is not a
    /// researched server message-length limit. No splitting or bulk sending.
    pub fn new(room: RoomId, text: String) -> Result<Self, SendError> {
        if text.is_empty() || text.len() > 16384 || text.contains('\0') {
            return Err(SendError::InvalidRequest);
        }
        Ok(Self { room, text })
    }
    /// Encode Agent Messenger's exact text profile: chatId:Int64, msg:String,
    /// type:Int32(1), noSeen:false. No synthetic client msgId or attachment.
    pub fn packet(&self) -> Result<Packet, SendError> {
        Packet::from_document(
            0,
            Command::new("WRITE").expect("constant command"),
            &bson::doc! {"chatId":self.room.get(), "msg":&self.text, "type":1, "noSeen":false},
        )
        .map_err(|_| SendError::InvalidRequest)
    }
    /// Interpret one caller-submitted request's result. Acknowledgment is not proof
    /// of recipient delivery/read. Never retries. Node's flat WRITE acknowledgment
    /// is supported; the optional chatLog variant needs further fixture evidence.
    pub fn outcome(
        &self,
        result: Result<Packet, crate::session::RequestError>,
    ) -> Result<TextReceipt, SendError> {
        use crate::{session::RequestError, Error};
        let packet = result.map_err(|e| match e {
            RequestError::Unauthenticated
            | RequestError::Transport(
                Error::Overloaded | Error::Config | Error::AuthenticationUnavailable,
            ) => SendError::NotSent,
            RequestError::Transport(Error::Rejected(n)) => SendError::Rejected(i64::from(n as i16)),
            RequestError::Transport(Error::Timeout) => SendError::TimeoutUnknown,
            _ => SendError::DeliveryUnknown,
        })?;
        let parse = || -> Result<TextReceipt, InvalidMessage> {
            if packet.command.as_str() != "WRITE" {
                return Err(InvalidMessage);
            }
            let doc = body(&packet)?;
            fields(doc)?;
            let room = RoomId::new(integer(doc, "chatId")?).map_err(|_| InvalidMessage)?;
            if room != self.room {
                return Err(InvalidMessage);
            }
            let id = MessageId::new(integer(doc, "logId")?)?;
            let sent_at_seconds = integer(doc, "sendAt")?;
            if sent_at_seconds < 0 {
                return Err(InvalidMessage);
            }
            // Additional node-kakao fields are optional across source profiles.
            for key in ["msgId", "prevId"] {
                if value(doc, key)?.is_some() {
                    integer(doc, key)?;
                }
            }
            Ok(TextReceipt {
                room,
                id,
                sent_at_seconds,
            })
        };
        if packet.command.as_str() != "WRITE" {
            return Err(SendError::DeliveryUnknown);
        }
        if packet.status != 0 {
            return Err(SendError::Rejected(i64::from(packet.status as i16)));
        }
        let doc = body(&packet).map_err(|_| SendError::DeliveryUnknown)?;
        fields(doc).map_err(|_| SendError::DeliveryUnknown)?;
        if value(doc, "status")
            .map_err(|_| SendError::DeliveryUnknown)?
            .is_some()
        {
            let status = integer(doc, "status").map_err(|_| SendError::DeliveryUnknown)?;
            if status != 0 {
                return Err(SendError::Rejected(status));
            }
        }
        parse().map_err(|_| SendError::DeliveryUnknown)
    }
}
impl std::fmt::Debug for TextRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TextRequest([REDACTED])")
    }
}
/// Server acknowledgment subset; no end-to-end receipt or exactly-once guarantee.
#[derive(Debug, PartialEq, Eq)]
pub struct TextReceipt {
    /// Requested room, validated against the response.
    pub room: RoomId,
    /// Assigned server log ID.
    pub id: MessageId,
    /// Server timestamp in Unix seconds.
    pub sent_at_seconds: i64,
}

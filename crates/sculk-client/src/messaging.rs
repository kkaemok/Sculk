//! Historical MSG/WRITE subset, corroborated by pinned node-kakao and Agent
//! Messenger sources in docs/research.md. Offline tested; live unverified.
//! Decoding happens on the consumer, never inside the network driver. Unknown
//! packets and malformed known events retain their original validated BSON.
use crate::{read_only::RoomId, session::State};
use bson::raw::{RawBsonRef, RawDocument};
use sculk_protocol::Packet;
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;

mod builder;
mod content;
mod write;
pub use builder::{MessageBuilder, OutgoingMessage};
pub use content::{Attachment, Media, Mention, MessageContent, ThreadMetadata};
pub(crate) use write::WriteMode;
pub use write::{MessageSender, Operation, SendError, TextReceipt, TextRequest, TextSender};

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
    /// Digital-item emoticon, type 6.
    Emoticon,
    /// Historical KakaoLink v1 card, type 9. Payload remains opaque.
    RichLink,
    /// Sticker, type 12.
    Sticker,
    /// File, type 18.
    File,
    /// Animated sticker, type 20.
    AnimatedSticker,
    /// Actioncon, type 22.
    Actioncon,
    /// GIF sticker, type 25.
    GifSticker,
    /// Reply, type 26; attachment remains opaque.
    Reply,
    /// Group of photos, type 27.
    MultiPhoto,
    /// Other raw type, including deleted-message flags.
    Unknown(i32),
}
impl MessageType {
    /// Exact numeric chatLog.type, including unknown flags and values.
    pub fn wire_type(self) -> i32 {
        match self {
            Self::Feed => 0,
            Self::Text => 1,
            Self::Photo => 2,
            Self::Video => 3,
            Self::Audio => 5,
            Self::Emoticon => 6,
            Self::RichLink => 9,
            Self::Sticker => 12,
            Self::File => 18,
            Self::AnimatedSticker => 20,
            Self::Actioncon => 22,
            Self::GifSticker => 25,
            Self::Reply => 26,
            Self::MultiPhoto => 27,
            Self::Unknown(value) => value,
        }
    }
}
impl From<i32> for MessageType {
    fn from(value: i32) -> Self {
        match value {
            0 => Self::Feed,
            1 => Self::Text,
            2 => Self::Photo,
            3 => Self::Video,
            5 => Self::Audio,
            6 => Self::Emoticon,
            9 => Self::RichLink,
            12 => Self::Sticker,
            18 => Self::File,
            20 => Self::AnimatedSticker,
            22 => Self::Actioncon,
            25 => Self::GifSticker,
            26 => Self::Reply,
            27 => Self::MultiPhoto,
            n => Self::Unknown(n),
        }
    }
}
/// A type-26 attachment's source-message metadata. Sender IDs are scoped
/// to the referenced conversation. Displayed text is server-provided metadata.
pub struct Quote<'a> {
    /// Source log ID.
    pub message: MessageId,
    /// Source sender ID in that context.
    pub sender: SenderId,
    /// Original numeric type, preserving unknown message types.
    pub kind: MessageType,
    /// OpenChat link, when independently supplied by the attachment.
    pub link: Option<crate::read_only::OpenLinkId>,
    /// Quoted preview received from the server.
    pub preview: std::borrow::Cow<'a, str>,
}

impl std::fmt::Debug for Quote<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Quote([REDACTED])")
    }
}

#[derive(serde::Deserialize)]
struct WireQuote<'a> {
    #[serde(rename = "src_logId")]
    message: i64,
    #[serde(rename = "src_userId")]
    sender: i64,
    #[serde(rename = "src_type")]
    kind: i32,
    #[serde(rename = "src_linkId")]
    link: Option<i64>,
    #[serde(borrow, rename = "src_message")]
    preview: std::borrow::Cow<'a, str>,
}
/// Invalid or ambiguous MSG schema. No peer content appears in diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid message event")]
pub struct InvalidMessage;

pub(crate) fn fields(doc: &RawDocument) -> Result<(), InvalidMessage> {
    let mut keys = std::collections::HashSet::new();
    for field in doc.iter() {
        let (key, _) = field.map_err(|_| InvalidMessage)?;
        if keys.len() >= 128 || !keys.insert(key) {
            return Err(InvalidMessage);
        }
    }
    Ok(())
}
pub(crate) fn value<'a>(
    doc: &'a RawDocument,
    key: &str,
) -> Result<Option<RawBsonRef<'a>>, InvalidMessage> {
    doc.get(key).map_err(|_| InvalidMessage)
}
pub(crate) fn integer(doc: &RawDocument, key: &str) -> Result<i64, InvalidMessage> {
    match value(doc, key)? {
        Some(RawBsonRef::Int32(n)) => Ok(i64::from(n)),
        Some(RawBsonRef::Int64(n)) => Ok(n),
        _ => Err(InvalidMessage),
    }
}
pub(crate) fn string<'a>(
    doc: &'a RawDocument,
    key: &str,
) -> Result<Option<&'a str>, InvalidMessage> {
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
pub(crate) fn body(packet: &Packet) -> Result<&RawDocument, InvalidMessage> {
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
    log_range: std::ops::Range<usize>,
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
    /// Interpret the sender within this message's room, for local room policy.
    pub fn room_member(&self) -> Option<crate::read_only::RoomMemberId> {
        crate::read_only::RoomMemberId::new(self.sender.get()).ok()
    }

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
        let start = log.as_bytes().as_ptr() as usize - packet.body().as_ptr() as usize;
        let log_range = start..start + log.as_bytes().len();
        Ok(Self {
            packet,
            log_range,
            room,
            sender,
            id,
            kind,
            sent_at_seconds,
        })
    }
    fn log(&self) -> &RawDocument {
        RawDocument::from_bytes(&self.packet.body()[self.log_range.clone()])
            .expect("validated log range")
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
    /// OpenChat link, when explicitly carried by the source MSG envelope.
    pub fn open_link(&self) -> Option<crate::read_only::OpenLinkId> {
        if self.packet.command.as_str() != "MSG" {
            return None;
        }
        let link = integer(body(&self.packet).ok()?, "li").ok()?;
        crate::read_only::OpenLinkId::new(link).ok()
    }
    /// Decode the historical quoted-reply attachment subset on demand. The
    /// original attachment is retained even when its schema is invalid.
    pub fn quote(&self) -> Result<Option<Quote<'_>>, InvalidMessage> {
        if self.kind != MessageType::Reply {
            return Ok(None);
        }
        let attachment = self
            .attachment()
            .filter(|attachment| attachment.len() <= 32768)
            .ok_or(InvalidMessage)?;
        let quote: WireQuote<'_> = serde_json::from_str(attachment).map_err(|_| InvalidMessage)?;
        let link = quote
            .link
            .map(crate::read_only::OpenLinkId::new)
            .transpose()
            .map_err(|_| InvalidMessage)?;
        Ok(Some(Quote {
            message: MessageId::new(quote.message)?,
            sender: SenderId::new(quote.sender)?,
            kind: MessageType::from(quote.kind),
            link,
            preview: quote.preview,
        }))
    }

    /// Raw source prevId, when supplied. Zero means the first log in node-kakao's
    /// schema; numeric IDs themselves are not assumed contiguous.
    pub fn previous_id(&self) -> Option<i64> {
        value(self.log(), "prevId")
            .expect("validated log")
            .map(|_| integer(self.log(), "prevId").expect("validated log"))
    }
    #[cfg(feature = "synchronization")]
    pub(crate) fn from_history(packet: Packet, log: &RawDocument) -> Result<Self, InvalidMessage> {
        fields(log)?;
        let room = RoomId::new(integer(log, "chatId")?).map_err(|_| InvalidMessage)?;
        let id = MessageId::new(integer(log, "logId")?)?;
        let sender = SenderId::new(integer(log, "authorId")?)?;
        let kind =
            MessageType::from(i32::try_from(integer(log, "type")?).map_err(|_| InvalidMessage)?);
        let sent_at_seconds = integer(log, "sendAt")?;
        if sent_at_seconds < 0 {
            return Err(InvalidMessage);
        }
        if kind == MessageType::Text && string(log, "message")?.is_none() {
            return Err(InvalidMessage);
        }
        for key in ["message", "attachment", "supplement"] {
            string(log, key)?;
        }
        for key in ["msgId", "prevId", "referer"] {
            if value(log, key)?.is_some() {
                integer(log, key)?;
            }
        }
        let start = (log.as_bytes().as_ptr() as usize)
            .checked_sub(packet.body().as_ptr() as usize)
            .ok_or(InvalidMessage)?;
        let end = start
            .checked_add(log.as_bytes().len())
            .filter(|end| *end <= packet.body().len())
            .ok_or(InvalidMessage)?;
        let log_range = start..end;
        Ok(Self {
            packet,
            log_range,
            room,
            sender,
            id,
            kind,
            sent_at_seconds,
        })
    }
    #[cfg(feature = "synchronization")]
    pub(crate) fn same_log(&self, other: &Self) -> bool {
        self.log().as_bytes() == other.log().as_bytes()
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

/// What the source membership command observed. A departure carries no cause.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MembershipKind {
    Added,
    Departed,
}

/// Exact room-scoped membership observation. Raw metadata remains on the event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MembershipChange {
    pub room: RoomId,
    pub member: crate::read_only::RoomMemberId,
    pub kind: MembershipKind,
}

/// Historical log-change pushes documented by OpenKakao. Codes are source
/// observations and do not establish current Android-side behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogChange {
    Deleted {
        room: RoomId,
        message: MessageId,
    },
    Reaction {
        room: RoomId,
        message: MessageId,
        member: crate::read_only::RoomMemberId,
        action: i32,
    },
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

    /// Validate NEWMEM or DELMEM on demand. The raw envelope remains available
    /// even when the packet is ambiguous or a known field does not validate.
    pub fn membership(&self) -> Result<Option<MembershipChange>, InvalidMessage> {
        let Self::Unknown(packet) = self else {
            return Ok(None);
        };
        let kind = match packet.command.as_str() {
            "NEWMEM" => MembershipKind::Added,
            "DELMEM" => MembershipKind::Departed,
            _ => return Ok(None),
        };
        let doc = body(packet)?;
        let log = chat_log(doc)?;
        fields(doc)?;
        fields(log)?;
        if packet.status != 0 || (value(doc, "status")?.is_some() && integer(doc, "status")? != 0) {
            return Err(InvalidMessage);
        }
        let room = RoomId::new(integer(doc, "chatId")?).map_err(|_| InvalidMessage)?;
        if value(log, "chatId")?.is_some() && integer(log, "chatId")? != room.get() {
            return Err(InvalidMessage);
        }
        let member = crate::read_only::RoomMemberId::new(integer(log, "authorId")?)
            .map_err(|_| InvalidMessage)?;
        Ok(Some(MembershipChange { room, member, kind }))
    }

    /// Inspect the historical SYNCDLMSG or SYNCACTION fields without
    /// advancing room history or substituting a message dispatch event.
    pub fn historical_log_change(&self) -> Result<Option<LogChange>, InvalidMessage> {
        let Self::Unknown(packet) = self else {
            return Ok(None);
        };
        let command = packet.command.as_str();
        if !matches!(command, "SYNCDLMSG" | "SYNCACTION") {
            return Ok(None);
        }
        let doc = body(packet)?;
        fields(doc)?;
        if packet.status != 0 || (value(doc, "status")?.is_some() && integer(doc, "status")? != 0) {
            return Err(InvalidMessage);
        }
        let room = RoomId::new(integer(doc, "chatId")?).map_err(|_| InvalidMessage)?;
        let message = MessageId::new(integer(doc, "logId")?)?;
        let change = if command == "SYNCACTION" {
            let member = crate::read_only::RoomMemberId::new(integer(doc, "userId")?)
                .map_err(|_| InvalidMessage)?;
            let action = i32::try_from(integer(doc, "type")?).map_err(|_| InvalidMessage)?;
            LogChange::Reaction {
                room,
                message,
                member,
                action,
            }
        } else {
            LogChange::Deleted { room, message }
        };
        Ok(Some(change))
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

use super::{
    body, fields, integer, value, InvalidMessage, MessageId, MessageType, OutgoingMessage,
};
use crate::{read_only::RoomId, session::State};
use sculk_protocol::{Command, Packet};
use tokio::{sync::watch, time::Instant};

/// Encoder for the historical basic text WRITE request.
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
    /// Cancellation observed before admission.
    #[error("outbound request cancelled before submission")]
    Cancelled,
    /// The profile has no supported submission path for this operation.
    #[error("unsupported outbound operation: {0:?}")]
    Unsupported(Operation),
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

/// Non-idempotent messaging operation. Capability checks do not submit a packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Text,
    Mention,
    Quote,
    Upload,
    Delete,
    Edit,
    React,
    Typing,
    Forward,
    Thread,
}
impl TextRequest {
    /// Local policy: nonempty text, at most 16 KiB UTF-8, no NUL. This is not a
    /// researched server message-length limit. No splitting or bulk sending.
    pub fn new(room: RoomId, text: String) -> Result<Self, SendError> {
        validate_text(&text)?;
        Ok(Self { room, text })
    }
    /// Encode Agent Messenger's historical basic text profile: chatId:Int64, msg:String,
    /// type:Int32(1), noSeen:false. No synthetic client msgId or attachment.
    pub fn packet(&self) -> Result<Packet, SendError> {
        write_packet(self.room, &self.text, MessageType::Text, None, None)
    }
    /// Interpret one caller-submitted request's result. Acknowledgment is not proof
    /// of recipient delivery/read. Never retries. Node's flat WRITE acknowledgment
    /// is supported; the optional chatLog variant needs further fixture evidence.
    pub fn outcome(
        &self,
        result: Result<Packet, crate::session::RequestError>,
    ) -> Result<TextReceipt, SendError> {
        write_outcome(self.room, None, result)
    }
}

fn write_outcome(
    expected_room: RoomId,
    client_message_id: Option<i64>,
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
    let parse = |doc| -> Result<TextReceipt, InvalidMessage> {
        let room = RoomId::new(integer(doc, "chatId")?).map_err(|_| InvalidMessage)?;
        if room != expected_room {
            return Err(InvalidMessage);
        }
        let id = MessageId::new(integer(doc, "logId")?)?;
        let sent_at_seconds = integer(doc, "sendAt")?;
        if sent_at_seconds < 0 {
            return Err(InvalidMessage);
        }
        // Additional node-kakao fields are optional across source profiles.
        if value(doc, "msgId")?.is_some() {
            let received = integer(doc, "msgId")?;
            if client_message_id.is_some_and(|expected| expected != received) {
                return Err(InvalidMessage);
            }
        }
        if value(doc, "prevId")?.is_some() {
            integer(doc, "prevId")?;
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
    parse(doc).map_err(|_| SendError::DeliveryUnknown)
}
impl std::fmt::Debug for TextRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TextRequest([REDACTED])")
    }
}
/// Handle to one authenticated session. The owning session controls
/// shutdown and expiration even when a handle is retained by an application.
pub struct MessageSender {
    client: crate::Client,
    state: watch::Receiver<State>,
    deadline: Instant,
    mode: WriteMode,
}

/// Compatibility name for the original text-sending handle.
pub type TextSender = MessageSender;

#[derive(Clone)]
pub(crate) enum WriteMode {
    Legacy,
    #[cfg(feature = "protocol-connect")]
    Tablet {
        suffix: i64,
        sequence: std::sync::Arc<std::sync::atomic::AtomicI64>,
    },
}

impl WriteMode {
    #[cfg(feature = "protocol-connect")]
    pub(crate) fn tablet(
        device: &str,
        sequence: std::sync::Arc<std::sync::atomic::AtomicI64>,
    ) -> Self {
        let hash = device.encode_utf16().fold(0_i32, |acc, unit| {
            acc.wrapping_mul(31).wrapping_add(i32::from(unit))
        });
        Self::Tablet {
            suffix: i64::from(hash.rem_euclid(100)),
            sequence,
        }
    }

    fn next_message_id(&self) -> Result<Option<i64>, SendError> {
        match self {
            Self::Legacy => Ok(None),
            #[cfg(feature = "protocol-connect")]
            Self::Tablet { suffix, sequence } => {
                let wall = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| SendError::InvalidRequest)?
                    .as_millis();
                Ok(Some(next_tablet_id(sequence, *suffix, wall)))
            }
        }
    }
}

#[cfg(feature = "protocol-connect")]
fn next_tablet_id(sequence: &std::sync::atomic::AtomicI64, suffix: i64, wall_millis: u128) -> i64 {
    use std::sync::atomic::Ordering;
    const WINDOW: u128 = 0x7fff_ff9b;
    const BUCKETS: i64 = (i32::MAX as i64 - 99) / 100 + 1;
    let base = ((wall_millis % WINDOW) / 100) as i64;
    let mut previous = sequence.load(Ordering::Relaxed);
    loop {
        let mut next = base.max(previous + 1);
        let mut id = (next % BUCKETS) * 100 + suffix;
        if id == 0 {
            next += 1;
            id = (next % BUCKETS) * 100 + suffix;
        }
        match sequence.compare_exchange_weak(previous, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return id,
            Err(current) => previous = current,
        }
    }
}

fn write_packet(
    room: RoomId,
    text: &str,
    kind: MessageType,
    extra: Option<&str>,
    client_message_id: Option<i64>,
) -> Result<Packet, SendError> {
    let mut doc =
        bson::doc! {"chatId":room.get(), "msg":text, "type":kind.wire_type(), "noSeen":false};
    if let Some(id) = client_message_id {
        doc.insert("msgId", id);
        doc.insert("scope", 1_i32);
        doc.insert("silence", false);
    }
    if let Some(extra) = extra.or(client_message_id.map(|_| "{}")) {
        doc.insert("extra", extra);
    }
    Packet::from_document(0, Command::new("WRITE").expect("constant command"), &doc)
        .map_err(|_| SendError::InvalidRequest)
}

impl MessageSender {
    /// Check availability before issuing a mutation, without touching the network.
    pub fn require(&self, operation: Operation) -> Result<(), SendError> {
        match operation {
            Operation::Text | Operation::Mention | Operation::Quote => Ok(()),
            other => Err(SendError::Unsupported(other)),
        }
    }

    /// Explicit media boundary. A validated plan still requires an independently
    /// verified transport, upload receipt and message submission contract.
    #[cfg(all(feature = "media", unix))]
    pub async fn send_attachments(
        &self,
        _room: RoomId,
        _plan: crate::media::Plan,
        cancel: watch::Receiver<bool>,
    ) -> Result<TextReceipt, SendError> {
        if *cancel.borrow() || cancel.has_changed().is_err() {
            return Err(SendError::Cancelled);
        }
        self.require(Operation::Upload)?;
        Err(SendError::Unsupported(Operation::Upload))
    }
    pub(crate) fn new(
        client: crate::Client,
        state: watch::Receiver<State>,
        deadline: Instant,
        mode: WriteMode,
    ) -> Self {
        Self {
            client,
            state,
            deadline,
            mode,
        }
    }

    /// Submit once, correlate the reply and require a room-bound acknowledgment.
    /// Cancelling after admission conservatively returns unknown delivery.
    pub async fn send_text(
        &self,
        room: RoomId,
        text: &str,
        cancel: watch::Receiver<bool>,
    ) -> Result<TextReceipt, SendError> {
        validate_text(text)?;
        self.send(room, text, MessageType::Text, None, cancel).await
    }

    /// Submit a composed message once using this session's profile and the same
    /// receipt and cancellation semantics as plain text.
    pub async fn send_message(
        &self,
        room: RoomId,
        message: &OutgoingMessage,
        cancel: watch::Receiver<bool>,
    ) -> Result<TextReceipt, SendError> {
        let (text, kind, extra) = message.parts(room)?;
        self.send(room, text, kind, extra, cancel).await
    }

    async fn send(
        &self,
        room: RoomId,
        text: &str,
        kind: MessageType,
        extra: Option<&str>,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<TextReceipt, SendError> {
        if *cancel.borrow() || cancel.has_changed().is_err() {
            return Err(SendError::Cancelled);
        }
        if *self.state.borrow() != State::Authenticated
            || self.deadline <= Instant::now()
            || self.client.state() != crate::ConnectionState::Connected
        {
            return Err(SendError::NotSent);
        }
        let client_message_id = self.mode.next_message_id()?;
        let packet = write_packet(room, text, kind, extra, client_message_id)?;
        if *cancel.borrow() || cancel.has_changed().is_err() {
            return Err(SendError::Cancelled);
        }
        let response = self.client.submit(packet).map_err(|_| SendError::NotSent)?;
        tokio::select! { biased;
            _ = crate::recovery::cancelled(&mut cancel) => Err(SendError::DeliveryUnknown),
            result = tokio::time::timeout_at(self.deadline, response) => {
                write_outcome(room, client_message_id,
                    result.unwrap_or(Err(crate::Error::Timeout))
                        .map_err(crate::session::RequestError::Transport),
                )
            }
        }
    }
}

pub(super) fn validate_text(text: &str) -> Result<(), SendError> {
    if text.is_empty() || text.len() > 16384 || text.contains('\0') {
        return Err(SendError::InvalidRequest);
    }
    Ok(())
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

#[cfg(all(test, feature = "protocol-connect"))]
mod tests {
    use super::*;
    use std::{collections::HashSet, sync::atomic::AtomicI64};

    #[test]
    fn tablet_ids_stay_positive_across_a_clock_fold_and_skip_zero() {
        let sequence = AtomicI64::new(-1);
        let before = 0x7fff_ff9a;
        let mut ids = HashSet::from([next_tablet_id(&sequence, 0, before)]);
        for wall in [before + 1, before - 1000, 0] {
            let id = next_tablet_id(&sequence, 0, wall);
            assert!((1..=i64::from(i32::MAX)).contains(&id));
            assert!(ids.insert(id));
        }
        assert_eq!(next_tablet_id(&AtomicI64::new(-1), 0, 0), 100);
    }
}

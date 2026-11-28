use super::{write::validate_text, Message, MessageType, SendError};
use crate::read_only::{OpenLinkId, RoomId, RoomMemberId};
use serde::{ser::SerializeStruct, Serialize};

/// Compose text, explicit room-member mentions and a same-room quotation.
/// Local bounds keep allocation small before a request is admitted.
#[derive(Default)]
pub struct MessageBuilder {
    text: String,
    mentions: Vec<MentionTag>,
    at_count: usize,
    quote: Option<Source>,
    invalid: bool,
}

struct MentionTag {
    member: RoomMemberId,
    display: String,
    at: Vec<usize>,
}

impl Serialize for MentionTag {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut fields = serializer.serialize_struct("MentionTag", 3)?;
        fields.serialize_field("at", &self.at)?;
        fields.serialize_field("len", &self.display.chars().count())?;
        fields.serialize_field("user_id", &self.member.get())?;
        fields.end()
    }
}

struct Source {
    room: RoomId,
    log: i64,
    user: i64,
    kind: i32,
    preview: String,
    link: Option<OpenLinkId>,
}

/// Validated, composed content. Submission still requires an authenticated
/// sender, a room and the caller's send authorization.
pub struct OutgoingMessage {
    text: String,
    kind: MessageType,
    extra: Option<String>,
    source_room: Option<RoomId>,
}

impl std::fmt::Debug for OutgoingMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OutgoingMessage([REDACTED])")
    }
}

impl OutgoingMessage {
    pub(super) fn parts(
        &self,
        room: RoomId,
    ) -> Result<(&str, MessageType, Option<&str>), SendError> {
        if self.source_room.is_some_and(|source| source != room) {
            return Err(SendError::InvalidRequest);
        }
        Ok((&self.text, self.kind, self.extra.as_deref()))
    }
}

impl MessageBuilder {
    /// Begin with empty content.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append literal text. Literal '@' characters also count toward positions.
    pub fn text(mut self, part: &str) -> Self {
        if part.len() > 16384 - self.text.len() || part.contains('\0') {
            self.invalid = true;
        } else {
            self.at_count += part.bytes().filter(|byte| *byte == b'@').count();
            self.text.push_str(part);
        }
        self
    }

    /// Append an explicit tag for a member of the destination room. A member
    /// must have a consistent display name. Only BMP names have known units.
    pub fn mention(mut self, member: RoomMemberId, display: &str) -> Self {
        if self.invalid
            || display.is_empty()
            || display.len() > 256
            || display.len() + 1 > 16384 - self.text.len()
            || display
                .chars()
                .any(|c| c == '@' || c.is_control() || u32::from(c) > 0xffff)
        {
            self.invalid = true;
            return self;
        }
        self.text.push('@');
        self.text.push_str(display);
        self.at_count += 1;
        if let Some(tag) = self.mentions.iter_mut().find(|tag| tag.member == member) {
            if tag.display != display || tag.at.len() >= 64 {
                self.invalid = true;
            } else {
                tag.at.push(self.at_count);
            }
        } else if self.mentions.len() >= 64 {
            self.invalid = true;
        } else {
            self.mentions.push(MentionTag {
                member,
                display: display.to_owned(),
                at: vec![self.at_count],
            });
        }
        self
    }

    /// Quote a received message in its original room. Supply the verified
    /// OpenChat link from room metadata when the source is in an OpenChat.
    pub fn reply_to(mut self, message: &Message, link: Option<OpenLinkId>) -> Self {
        if self.quote.is_some() || message.text().is_some_and(|text| text.len() > 16384) {
            self.invalid = true;
            return self;
        }
        self.quote = Some(Source {
            room: message.room,
            log: message.id.get(),
            user: message.sender.get(),
            kind: message.kind.wire_type(),
            preview: message.text().unwrap_or_default().to_owned(),
            link,
        });
        self
    }

    /// Finish and validate local size and metadata bounds.
    pub fn build(self) -> Result<OutgoingMessage, SendError> {
        if self.invalid {
            return Err(SendError::InvalidRequest);
        }
        validate_text(&self.text)?;
        let source_room = self.quote.as_ref().map(|source| source.room);
        let (kind, extra) = if let Some(source) = self.quote {
            let mut extra = serde_json::json!({
                "attach_only": false, "attach_type": source.kind,
                "src_logId": source.log, "src_userId": source.user, "src_type": source.kind,
                "src_message": source.preview, "src_mentions": [], "mentions": self.mentions,
            });
            if let Some(link) = source.link {
                extra["src_linkId"] = link.get().into();
            }
            (MessageType::Reply, Some(extra.to_string()))
        } else if self.mentions.is_empty() {
            (MessageType::Text, None)
        } else {
            (
                MessageType::Text,
                Some(serde_json::json!({"mentions": self.mentions}).to_string()),
            )
        };
        if extra.as_ref().is_some_and(|extra| extra.len() > 32768) {
            return Err(SendError::InvalidRequest);
        }
        Ok(OutgoingMessage {
            text: self.text,
            kind,
            extra,
            source_room,
        })
    }
}

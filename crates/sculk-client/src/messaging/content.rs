use super::{integer, string, Message, MessageId, MessageType, Quote, SenderId};
use crate::read_only::{OpenLinkId, RoomMemberId};
use serde_json::{Map, Value};
use std::borrow::Cow;

/// A room-scoped mention decoded from attachment metadata.
#[derive(Debug, PartialEq, Eq)]
pub struct Mention {
    /// Member referenced within the message's room.
    pub member: RoomMemberId,
    /// One-based ordinals of '@' in the message.
    pub positions: Vec<u32>,
    /// Wire length of the tagged display name.
    pub display_length: u32,
}

/// Optional thread metadata as supplied by the server.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ThreadMetadata {
    /// Raw scope. Its meaning may vary by profile.
    pub scope: Option<i32>,
    /// Referenced thread log.
    pub thread: Option<MessageId>,
    /// Previous log when the source supplied a nonzero ID.
    pub previous: Option<MessageId>,
}

/// Descriptive media metadata. Keys and URLs are opaque and can be sensitive.
/// Decoding never accesses a URL or opens a file.
pub struct Media {
    /// Source media key.
    pub key: Option<String>,
    /// Source URL, when present.
    pub url: Option<String>,
    /// Reported file name.
    pub file_name: Option<String>,
    /// Reported byte size.
    pub bytes: Option<u64>,
    /// Pixel width when present.
    pub width: Option<u32>,
    /// Pixel height when present.
    pub height: Option<u32>,
    /// Reported duration; units are profile and media dependent.
    pub duration: Option<u64>,
}
impl std::fmt::Debug for Media {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Media([REDACTED])")
    }
}

/// Recognized attachment subset. Classification of unknown metadata is deferred.
pub enum Attachment<'a> {
    /// Quoted log and its source metadata.
    Reply(Quote<'a>),
    /// Photo, video, audio or file. See the containing message's type.
    Media(Media),
    /// Ordered photo group.
    Photos(Vec<Media>),
    /// Emoticon or sticker with optional source item path.
    Sticker {
        /// Provider item path, treated as opaque.
        item_path: Option<String>,
    },
    /// Historical KakaoLink card; fields are preserved in the original metadata.
    RichLink,
    /// Untyped or invalid attachment metadata.
    Opaque,
}

/// On-demand view over one message. Original metadata remains available for
/// partially decoded and future wire formats.
pub struct MessageContent<'a> {
    /// Exact decoded kind.
    pub kind: MessageType,
    /// Borrowed display text.
    pub text: Option<&'a str>,
    /// Valid mention entries, if present.
    pub mentions: Vec<Mention>,
    /// Structured metadata available for the recognized type.
    pub attachment: Option<Attachment<'a>>,
    /// Original, unmodified attachment string.
    pub raw_attachment: Option<&'a str>,
    /// Original, unmodified supplement string.
    pub raw_supplement: Option<&'a str>,
    /// Optional fields from the chat log and supplement.
    pub thread: ThreadMetadata,
}

const METADATA_LIMIT: usize = 32768;

fn object(raw: Option<&str>) -> Option<Map<String, Value>> {
    serde_json::from_str(raw.filter(|raw| raw.len() <= METADATA_LIMIT)?).ok()
}

fn id(value: &Value) -> Option<i64> {
    match value {
        Value::String(digits)
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) =>
        {
            digits.parse::<i64>().ok()
        }
        _ => value.as_i64(),
    }
    .filter(|id| *id > 0)
}

fn mentions(map: &Map<String, Value>) -> Vec<Mention> {
    let Some(entries) = map.get("mentions").and_then(Value::as_array) else {
        return Vec::new();
    };
    if entries.len() > 64 {
        return Vec::new();
    }
    entries
        .iter()
        .filter_map(|entry| {
            let member = RoomMemberId::new(id(entry.get("user_id")?)?).ok()?;
            let display_length = small(entry.get("len"))?;
            let ordinals = entry.get("at")?;
            let positions = if let Some(values) = ordinals.as_array() {
                if values.is_empty() || values.len() > 64 {
                    return None;
                }
                values.iter().map(ordinal).collect::<Option<Vec<_>>>()?
            } else {
                vec![ordinal(ordinals)?]
            };
            if display_length == 0 || !positions.windows(2).all(|pair| pair[0] < pair[1]) {
                return None;
            }
            Some(Mention {
                member,
                positions,
                display_length,
            })
        })
        .collect()
}

fn small(value: Option<&Value>) -> Option<u32> {
    u32::try_from(value?.as_u64()?).ok()
}

fn ordinal(value: &Value) -> Option<u32> {
    small(Some(value)).filter(|position| *position != 0)
}

fn text(value: Option<&Value>) -> Option<String> {
    value?
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn single(map: &Map<String, Value>, kind: MessageType) -> Media {
    Media {
        key: text(map.get(if kind == MessageType::Video {
            "tk"
        } else {
            "k"
        })),
        url: text(map.get("url")),
        file_name: text(map.get("name")),
        bytes: map
            .get("s")
            .or_else(|| map.get("size"))
            .and_then(Value::as_u64),
        width: small(map.get("w")),
        height: small(map.get("h")),
        duration: map.get("d").and_then(Value::as_u64),
    }
}

fn photos(map: &Map<String, Value>) -> Option<Vec<Media>> {
    let keys = map.get("kl")?.as_array()?;
    if keys.is_empty() || keys.len() > 64 {
        return None;
    }
    for key in ["wl", "hl", "sl", "imageUrls"] {
        if map
            .get(key)
            .is_some_and(|value| value.as_array().is_none_or(|a| a.len() != keys.len()))
        {
            return None;
        }
    }
    let at = |field, index| map.get(field)?.as_array()?.get(index);
    keys.iter()
        .enumerate()
        .map(|(i, key)| {
            let key = text(Some(key))?;
            Some(Media {
                key: Some(key),
                url: text(at("imageUrls", i)),
                file_name: None,
                bytes: at("sl", i).and_then(Value::as_u64),
                width: small(at("wl", i)),
                height: small(at("hl", i)),
                duration: None,
            })
        })
        .collect()
}

fn quote(map: &Map<String, Value>) -> Option<Quote<'static>> {
    Some(Quote {
        message: MessageId::new(id(map.get("src_logId")?)?).ok()?,
        sender: SenderId::new(id(map.get("src_userId")?)?).ok()?,
        kind: MessageType::from(i32::try_from(map.get("src_type")?.as_i64()?).ok()?),
        link: match map.get("src_linkId") {
            Some(value) => Some(OpenLinkId::new(id(value)?).ok()?),
            None => None,
        },
        preview: Cow::Owned(map.get("src_message")?.as_str()?.to_owned()),
    })
}

fn attachment(kind: MessageType, map: &Map<String, Value>) -> Attachment<'static> {
    match kind {
        MessageType::Reply => quote(map).map_or(Attachment::Opaque, Attachment::Reply),
        MessageType::Photo | MessageType::Video | MessageType::Audio | MessageType::File => {
            Attachment::Media(single(map, kind))
        }
        MessageType::MultiPhoto => photos(map).map_or(Attachment::Opaque, Attachment::Photos),
        MessageType::Emoticon
        | MessageType::Sticker
        | MessageType::AnimatedSticker
        | MessageType::Actioncon
        | MessageType::GifSticker => Attachment::Sticker {
            item_path: text(map.get("path")).or_else(|| text(map.get("emoticonItemPath"))),
        },
        MessageType::RichLink => Attachment::RichLink,
        _ => Attachment::Opaque,
    }
}

impl Message {
    /// Interpret optional metadata on demand. Raw bytes remain available from
    /// the packet, and invalid optional fields do not hide the received message.
    pub fn content(&self) -> MessageContent<'_> {
        let log = self.log();
        let raw_attachment = self.attachment();
        let raw_supplement = string(log, "supplement").ok().flatten();
        let map = object(raw_attachment);
        let supplement = object(raw_supplement);
        let scope = integer(log, "scope")
            .ok()
            .or_else(|| supplement.as_ref()?.get("scope")?.as_i64());
        let thread = integer(log, "threadId")
            .ok()
            .filter(|id| *id > 0)
            .or_else(|| id(supplement.as_ref()?.get("threadId")?));
        MessageContent {
            kind: self.kind,
            text: self.text(),
            mentions: map.as_ref().map_or_else(Vec::new, mentions),
            attachment: raw_attachment.map(|_| {
                map.as_ref()
                    .map_or(Attachment::Opaque, |map| attachment(self.kind, map))
            }),
            raw_attachment,
            raw_supplement,
            thread: ThreadMetadata {
                scope: scope.and_then(|scope| i32::try_from(scope).ok()),
                thread: thread.and_then(|id| MessageId::new(id).ok()),
                previous: self.previous_id().and_then(|id| MessageId::new(id).ok()),
            },
        }
    }
}

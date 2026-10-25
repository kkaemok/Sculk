//! Experimental source-backed read-only chat queries. No live compatibility claim.
//! One page/request at a time; no implicit pagination, history loading or read marks.
use crate::session::Session;
use bson::{doc, Bson, Document};
use sculk_protocol::{Command, Packet};

/// Read-only query failure. No response payload or personal data in errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid room ID or cursor.
    #[error("invalid read-only request")]
    InvalidRequest,
    /// Response omitted, duplicated or mistyped required fields, or exceeded bounds.
    #[error("invalid read-only response")]
    InvalidResponse,
    /// Peer returned nonzero BSON status.
    #[error("read-only request rejected with status {0}")]
    Rejected(i64),
    /// Authentication/transport/timeout failure; never automatically retried.
    #[error(transparent)]
    Session(#[from] crate::session::RequestError),
}
/// Positive room-scoped identifier. This is not a user/account/OpenChat profile ID.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct RoomId(i64);
impl std::fmt::Debug for RoomId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoomId([REDACTED])")
    }
}
impl RoomId {
    /// Preserve the exact signed 64-bit wire value, rejecting nonpositive IDs.
    pub fn new(id: i64) -> Result<Self, Error> {
        if id <= 0 {
            Err(Error::InvalidRequest)
        } else {
            Ok(Self(id))
        }
    }
    /// Explicit access; avoid including IDs in diagnostic reports.
    pub fn get(self) -> i64 {
        self.0
    }
}
/// Server pagination tokens. Opaque last_token_id may be signed; do not infer TTL.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    /// Server-returned lastTokenId, exact BSON integer.
    pub last_token_id: i64,
    /// Server-returned lastChatId, zero only for the initial cursor.
    pub last_chat_id: i64,
}
/// Minimal room list item. No last-message content is retained.
pub struct RoomSummary {
    /// Room identity from `c`.
    pub id: RoomId,
    /// Raw optional type string from `t`; unknown values are preserved.
    pub kind: Option<String>,
    /// Optional nonnegative active-member count from `a`.
    pub active_members: Option<u32>,
}
/// One bounded LCHATLIST page. Does not claim a complete room list when eof=false.
pub struct ChatPage {
    /// At most 4096 returned summaries, additionally bounded by packet size.
    pub rooms: Vec<RoomSummary>,
    /// Explicit server end-of-list flag.
    pub eof: bool,
    /// Required, progressing server cursor when more pages exist.
    pub next: Option<Cursor>,
}
/// Raw room metadata entry. Do not log personal content automatically.
pub struct RoomMeta {
    /// Raw server metadata kind, not interpreted beyond source-backed fields.
    pub kind: i32,
    /// Server-provided content (possibly structured JSON), not fabricated title text.
    pub content: String,
}
/// Minimal CHATINFO response tied to the requested room.
pub struct RoomInfo {
    /// Echoed chatId must match the request.
    pub id: RoomId,
    /// Raw optional room type.
    pub kind: Option<String>,
    /// Optional count returned by the server.
    pub active_members: Option<u32>,
    /// None means the server omitted chatMetas; no synthetic metadata is supplied.
    pub metadata: Option<Vec<RoomMeta>>,
}
/// A member in the requested room. Not asserted to be a stable global identity.
pub struct Member {
    /// Room-scoped userId; never equated with accountId or profileId.
    pub user_id: i64,
    /// Optional server nickname; no invented fallback name.
    pub nickname: Option<String>,
}
/// GETMEM response. No complete membership or replay guarantee across reconnects.
pub struct Members {
    /// Room requested, not a globally inferred identity scope.
    pub room: RoomId,
    /// At most 4096 entries, bounded again by packet length.
    pub members: Vec<Member>,
    /// Opaque server membership token, not an expiration time.
    pub token: i64,
}
fn number(v: &Bson) -> Result<i64, Error> {
    match v {
        Bson::Int32(n) => Ok(i64::from(*n)),
        Bson::Int64(n) => Ok(*n),
        _ => Err(Error::InvalidResponse),
    }
}
fn required_number(doc: &Document, key: &str) -> Result<i64, Error> {
    number(doc.get(key).ok_or(Error::InvalidResponse)?)
}
fn text(doc: &Document, key: &str, max: usize) -> Result<Option<String>, Error> {
    match doc.get(key) {
        None => Ok(None),
        Some(Bson::String(s)) if s.len() <= max => Ok(Some(s.clone())),
        _ => Err(Error::InvalidResponse),
    }
}
fn count(doc: &Document, key: &str) -> Result<Option<u32>, Error> {
    doc.get(key)
        .map(|v| number(v).and_then(|n| u32::try_from(n).map_err(|_| Error::InvalidResponse)))
        .transpose()
}
fn values<'a>(doc: &'a Document, key: &str) -> Result<&'a Vec<Bson>, Error> {
    let array = doc.get_array(key).map_err(|_| Error::InvalidResponse)?;
    if array.len() > 4096 {
        Err(Error::InvalidResponse)
    } else {
        Ok(array)
    }
}
fn positive(doc: &Document, key: &str) -> Result<RoomId, Error> {
    RoomId::new(required_number(doc, key)?).map_err(|_| Error::InvalidResponse)
}
fn request(name: &str, body: &Document) -> Result<Packet, Error> {
    Packet::from_document(0, Command::new(name).expect("constant command"), body)
        .map_err(|_| Error::InvalidRequest)
}
fn parse(packet: Packet, command: &str) -> Result<Document, Error> {
    if packet.command.as_str() != command {
        return Err(Error::InvalidResponse);
    }
    if packet.status != 0 {
        return Err(Error::Rejected(i64::from(packet.status as i16)));
    }
    // Reject duplicate keys in returned containers before owned BSON collapses them.
    fn unique(doc: &bson::raw::RawDocument) -> Result<(), Error> {
        let mut keys = std::collections::HashSet::new();
        for field in doc.iter() {
            let (k, v) = field.map_err(|_| Error::InvalidResponse)?;
            if !keys.insert(k) {
                return Err(Error::InvalidResponse);
            }
            match v {
                bson::raw::RawBsonRef::Document(d) => unique(d)?,
                bson::raw::RawBsonRef::Array(a) => {
                    for v in a {
                        if let bson::raw::RawBsonRef::Document(d) =
                            v.map_err(|_| Error::InvalidResponse)?
                        {
                            unique(d)?;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    unique(bson::raw::RawDocument::from_bytes(packet.body()).map_err(|_| Error::InvalidResponse)?)?;
    let doc = packet.document().map_err(|_| Error::InvalidResponse)?;
    if let Some(status) = doc.get("status") {
        let n = number(status)?;
        if n != 0 {
            return Err(Error::Rejected(n));
        }
    }
    Ok(doc)
}
impl Session {
    /// Request one source-profile room page. No automatic repeat, cursor guessing,
    /// read acknowledgment, or last-message retention. Requires an active session.
    pub async fn chat_rooms(&self, cursor: Cursor) -> Result<ChatPage, Error> {
        if cursor.last_chat_id < 0 {
            return Err(Error::InvalidRequest);
        }
        let packet = request(
            "LCHATLIST",
            &doc! {"chatIds":[],"maxIds":[],"lastTokenId":cursor.last_token_id,"lastChatId":cursor.last_chat_id},
        )?;
        let body = parse(self.request(packet).await?, "LCHATLIST")?;
        let rooms = values(&body, "chatDatas")?
            .iter()
            .map(|v| {
                let d = v.as_document().ok_or(Error::InvalidResponse)?;
                Ok(RoomSummary {
                    id: positive(d, "c")?,
                    kind: text(d, "t", 128)?,
                    active_members: count(d, "a")?,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let eof = body.get_bool("eof").map_err(|_| Error::InvalidResponse)?;
        let next = if eof {
            None
        } else {
            let next = Cursor {
                last_token_id: required_number(&body, "lastTokenId")?,
                last_chat_id: required_number(&body, "lastChatId")?,
            };
            if next.last_chat_id < 0 || next == cursor {
                return Err(Error::InvalidResponse);
            }
            Some(next)
        };
        Ok(ChatPage { rooms, eof, next })
    }
    /// Fetch the documented CHATINFO subset. The returned chatId must match.
    pub async fn room_info(&self, room: RoomId) -> Result<RoomInfo, Error> {
        let body = parse(
            self.request(request("CHATINFO", &doc! {"chatId":room.0})?)
                .await?,
            "CHATINFO",
        )?;
        let data = body
            .get_document("chatInfo")
            .map_err(|_| Error::InvalidResponse)?;
        let id = positive(data, "chatId")?;
        if id != room {
            return Err(Error::InvalidResponse);
        }
        let metadata = if data.contains_key("chatMetas") {
            let array = values(data, "chatMetas")?;
            if array.len() > 128 {
                return Err(Error::InvalidResponse);
            }
            Some(
                array
                    .iter()
                    .map(|v| {
                        let d = v.as_document().ok_or(Error::InvalidResponse)?;
                        Ok(RoomMeta {
                            kind: i32::try_from(required_number(d, "type")?)
                                .map_err(|_| Error::InvalidResponse)?,
                            content: text(d, "content", 32768)?.ok_or(Error::InvalidResponse)?,
                        })
                    })
                    .collect::<Result<_, Error>>()?,
            )
        } else {
            None
        };
        Ok(RoomInfo {
            id,
            kind: text(data, "type", 128)?,
            active_members: count(data, "activeMembersCount")?,
            metadata,
        })
    }
    /// Fetch one GETMEM snapshot without deriving global account/profile identity.
    pub async fn room_members(&self, room: RoomId) -> Result<Members, Error> {
        let body = parse(
            self.request(request("GETMEM", &doc! {"chatId":room.0})?)
                .await?,
            "GETMEM",
        )?;
        if let Some(id) = body.get("chatId") {
            if number(id)? != room.0 {
                return Err(Error::InvalidResponse);
            }
        }
        let members = values(&body, "members")?
            .iter()
            .map(|v| {
                let d = v.as_document().ok_or(Error::InvalidResponse)?;
                Ok(Member {
                    user_id: positive(d, "userId")?.0,
                    nickname: text(d, "nickName", 1024)?,
                })
            })
            .collect::<Result<_, Error>>()?;
        Ok(Members {
            room,
            members,
            token: required_number(&body, "token")?,
        })
    }
}

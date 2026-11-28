//! Experimental source-backed read-only chat queries. No live compatibility claim.
//! One page/request at a time; no implicit pagination, history loading or read marks.
use crate::session::Session;
use bson::{doc, Bson, Document};
use sculk_protocol::{Command, Packet};
use std::collections::HashSet;
use tokio::sync::watch;

/// Read-only query failure. No response payload or personal data in errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid room ID or cursor.
    #[error("invalid read-only request")]
    InvalidRequest,
    /// Response omitted, duplicated or mistyped required fields, or exceeded bounds.
    #[error("invalid read-only response")]
    InvalidResponse,
    /// The server did not establish a stable or complete result within bounds.
    #[error("read-only snapshot changed or is incomplete")]
    IncompleteSnapshot,
    /// Read-only operation cancelled.
    #[error("read-only operation cancelled")]
    Cancelled,
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
/// Positive OpenChat link identifier. Rooms and links have separate ID spaces.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct OpenLinkId(i64);
impl std::fmt::Debug for OpenLinkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpenLinkId([REDACTED])")
    }
}
impl OpenLinkId {
    /// Construct an exact positive OpenChat link identifier.
    pub fn new(value: i64) -> Result<Self, Error> {
        if value <= 0 {
            Err(Error::InvalidRequest)
        } else {
            Ok(Self(value))
        }
    }
    /// Read the exact identifier.
    pub fn get(self) -> i64 {
        self.0
    }
}

/// Read-only metadata for one requested OpenChat link.
pub struct OpenLinkInfo {
    /// Exact ID returned by the server and checked against the request.
    pub id: OpenLinkId,
    /// User-provided link name.
    pub name: String,
    /// Raw link type if the server supplied it.
    pub kind: Option<i32>,
}
/// Server pagination tokens. Opaque last_token_id may be signed; do not infer TTL.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
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
/// Why a bounded room listing stopped before explicit EOF.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListingLimit {
    PageLimit,
    ResultLimit,
    InvalidProgress,
    Cancelled,
}

/// Room summaries observed by a bounded traversal.
pub struct RoomListing {
    pub rooms: Vec<RoomSummary>,
    pub status: Result<(), ListingLimit>,
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
/// Source-backed fields from one room-bound CHATINFO read.
#[cfg(feature = "messaging")]
pub struct RoomDetails {
    pub info: RoomInfo,
    pub name: Option<String>,
    pub kind: Option<RoomKind>,
    pub link: Option<OpenLinkId>,
    pub watermark: Option<RoomLog>,
    pub last_seen: Option<RoomLog>,
    pub unread: Option<u32>,
}

#[cfg(feature = "messaging")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RoomKind {
    Named(String),
    Numeric(i32),
}

#[cfg(feature = "messaging")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoomLog {
    Empty,
    Id(crate::messaging::MessageId),
}
/// A member in the requested room. Not asserted to be a stable global identity.
pub struct Member {
    /// Room-scoped userId; never equated with accountId or profileId.
    pub user_id: i64,
    /// Optional server nickname; no invented fallback name.
    pub nickname: Option<String>,
}
impl Member {
    /// The exact member ID for use within the containing room.
    pub fn id(&self) -> Result<RoomMemberId, Error> {
        RoomMemberId::new(self.user_id)
    }
}

/// An exact ID scoped to a room. Its numeric value conveys no account identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct RoomMemberId(i64);

impl RoomMemberId {
    /// Construct from a positive wire integer from the room.
    pub fn new(id: i64) -> Result<Self, Error> {
        if id > 0 {
            Ok(Self(id))
        } else {
            Err(Error::InvalidRequest)
        }
    }

    /// Explicit wire value. Treat as private.
    pub fn get(self) -> i64 {
        self.0
    }
}
impl std::fmt::Debug for RoomMemberId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoomMemberId([REDACTED])")
    }
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

/// Completeness established by two bounded, read-only observations.
pub enum MemberSnapshot {
    Complete(Members),
    Partial {
        members: Members,
        expected_count: Option<u32>,
        reason: MemberSnapshotLimit,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemberSnapshotLimit {
    MissingCount,
    CountMismatch,
    MembershipChanged,
    DuplicateMember,
}

/// A mentionable member from a scoped membership observation.
pub struct MentionTarget<'a> {
    room: RoomId,
    member: RoomMemberId,
    display: &'a str,
}

impl MentionTarget<'_> {
    pub fn room(&self) -> RoomId {
        self.room
    }
    pub fn member(&self) -> RoomMemberId {
        self.member
    }
    pub fn display(&self) -> &str {
        self.display
    }
}

impl Members {
    /// Resolve one exact ID and display name within this observation.
    pub fn mention_target(&self, target: RoomMemberId) -> Result<MentionTarget<'_>, Error> {
        let mut matches = self
            .members
            .iter()
            .filter(|member| member.user_id == target.get());
        let display = matches
            .next()
            .and_then(|member| member.nickname.as_deref())
            .filter(|name| !name.is_empty())
            .ok_or(Error::InvalidRequest)?;
        if matches.next().is_some() {
            return Err(Error::InvalidResponse);
        }
        Ok(MentionTarget {
            room: self.room,
            member: target,
            display,
        })
    }
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
        let page = self.chat_page(cursor).await?;
        if page.next == Some(cursor) {
            return Err(Error::InvalidResponse);
        }
        Ok(page)
    }

    async fn chat_page(&self, cursor: Cursor) -> Result<ChatPage, Error> {
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
            if next.last_chat_id < 0 {
                return Err(Error::InvalidResponse);
            }
            Some(next)
        };
        Ok(ChatPage { rooms, eof, next })
    }

    /// List rooms to explicit EOF within caller-provided bounds. Keeps the first
    /// occurrence of each exact ID, preserving server order across pages.
    /// Use the one-page API to consume partial results.
    pub async fn chat_rooms_bounded(
        &self,
        max_pages: usize,
        max_rooms: usize,
    ) -> Result<Vec<RoomSummary>, Error> {
        let (_stop, cancel) = watch::channel(false);
        let result = self.list_rooms(max_pages, max_rooms, cancel).await?;
        result.status.map_err(|_| Error::IncompleteSnapshot)?;
        Ok(result.rooms)
    }

    /// Traverse exact cursors with bounds and keep partial observations.
    /// Use `chat_rooms` to consume pages one at a time.
    pub async fn list_rooms(
        &self,
        max_pages: usize,
        max_rooms: usize,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<RoomListing, Error> {
        if !(1..=100).contains(&max_pages) || !(1..=4096).contains(&max_rooms) {
            return Err(Error::InvalidRequest);
        }
        let mut cursor = Cursor::default();
        let mut seen_cursors = HashSet::from([cursor]);
        let mut seen_rooms = HashSet::new();
        let mut rooms = Vec::new();
        let partial = |rooms, reason| {
            Ok(RoomListing {
                rooms,
                status: Err(reason),
            })
        };
        for _ in 0..max_pages {
            let page = tokio::select! { biased;
                _ = crate::recovery::cancelled(&mut cancel) =>
                    return partial(rooms, ListingLimit::Cancelled),
                page = self.chat_page(cursor) => page?,
            };
            if page.rooms.is_empty() && !page.eof {
                return partial(rooms, ListingLimit::InvalidProgress);
            }
            for room in page.rooms {
                if seen_rooms.insert(room.id) {
                    if rooms.len() == max_rooms {
                        return partial(rooms, ListingLimit::ResultLimit);
                    }
                    rooms.push(room);
                }
            }
            match page.next {
                None => {
                    return Ok(RoomListing {
                        rooms,
                        status: Ok(()),
                    })
                }
                Some(next) if seen_cursors.insert(next) => cursor = next,
                _ => return partial(rooms, ListingLimit::InvalidProgress),
            }
        }
        partial(rooms, ListingLimit::PageLimit)
    }

    /// Fetch the documented CHATINFO subset. The returned chatId must match.
    pub async fn room_info(&self, room: RoomId) -> Result<RoomInfo, Error> {
        let data = self.room_document(room).await?;
        Self::room_info_from(room, &data)
    }

    fn room_info_from(room: RoomId, data: &Document) -> Result<RoomInfo, Error> {
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
            id: room,
            kind: text(data, "type", 128)?,
            active_members: count(data, "activeMembersCount")?,
            metadata,
        })
    }

    /// Read the explicit title, link, watermark and available read-state
    /// metadata. A read-state query never sends a read receipt.
    #[cfg(feature = "messaging")]
    pub async fn room_details(&self, room: RoomId) -> Result<RoomDetails, Error> {
        let mut data = self.room_document(room).await?;
        let kind = match data.get("type") {
            None => None,
            Some(Bson::String(_)) => text(&data, "type", 128)?.map(RoomKind::Named),
            Some(value) => Some(RoomKind::Numeric(
                i32::try_from(number(value)?).map_err(|_| Error::InvalidResponse)?,
            )),
        };
        if matches!(kind, Some(RoomKind::Numeric(_))) {
            data.remove("type");
        }
        let info = Self::room_info_from(room, &data)?;
        let name = info
            .metadata
            .as_deref()
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.kind == 3 && !entry.content.is_empty())
            })
            .map(|entry| entry.content.clone());
        let link = room_link(&data)?;
        let watermark = room_log(&data, "lastLogId")?;
        let last_seen = room_log(&data, "lastSeenLogId")?;
        let unread = match data.get("invalidNewMessageCount") {
            None | Some(Bson::Boolean(false)) => count(&data, "newMessageCount")?,
            Some(Bson::Boolean(true)) => None,
            _ => return Err(Error::InvalidResponse),
        };
        Ok(RoomDetails {
            info,
            name,
            kind,
            link,
            watermark,
            last_seen,
            unread,
        })
    }
    async fn room_document(&self, room: RoomId) -> Result<Document, Error> {
        let mut body = parse(
            self.request(request("CHATINFO", &doc! {"chatId": room.get()})?)
                .await?,
            "CHATINFO",
        )?;
        match body.remove("chatInfo") {
            Some(Bson::Document(data)) if positive(&data, "chatId")? == room => Ok(data),
            _ => Err(Error::InvalidResponse),
        }
    }

    /// Inspect CHATINFO for a source-supported link association. None means
    /// the peer omitted the association or supplied its zero sentinel.
    pub async fn room_open_link(&self, room: RoomId) -> Result<Option<OpenLinkId>, Error> {
        let data = self.room_document(room).await?;
        room_link(&data)
    }

    /// Inspect a known OpenChat link using INFOLINK. An empty response list
    /// leaves metadata unavailable. This request does not join or grant room access.
    pub async fn open_link_info(&self, link: OpenLinkId) -> Result<Option<OpenLinkInfo>, Error> {
        let packet = request("INFOLINK", &doc! {"lis": [link.get()]})?;
        let body = parse(self.request(packet).await?, "INFOLINK")?;
        let Some(_) = body.get("ols") else {
            return Ok(None);
        };
        match values(&body, "ols")?.as_slice() {
            [] => Ok(None),
            [Bson::Document(info)] if required_number(info, "li")? == link.get() => {
                let kind = info
                    .get("lt")
                    .map(|value| i32::try_from(number(value)?).map_err(|_| Error::InvalidResponse))
                    .transpose()?;
                Ok(Some(OpenLinkInfo {
                    id: link,
                    name: text(info, "ln", 4096)?.ok_or(Error::InvalidResponse)?,
                    kind,
                }))
            }
            _ => Err(Error::InvalidResponse),
        }
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

    /// Read CHATINFO and GETMEM twice. Return the second GETMEM snapshot only
    /// when both observations agree on their member IDs and active counts.
    /// No room entry, read acknowledgment or enrollment is performed.
    pub async fn room_members_verified(&self, room: RoomId) -> Result<Members, Error> {
        match self.observe_members(room).await? {
            MemberSnapshot::Complete(members) => Ok(members),
            MemberSnapshot::Partial { .. } => Err(Error::IncompleteSnapshot),
        }
    }

    /// Verify a bounded snapshot without room entry, with explicit partial results.
    pub async fn room_member_snapshot(
        &self,
        room: RoomId,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<MemberSnapshot, Error> {
        tokio::select! { biased;
            _ = crate::recovery::cancelled(&mut cancel) => Err(Error::Cancelled),
            snapshot = self.observe_members(room) => snapshot,
        }
    }

    async fn observe_members(&self, room: RoomId) -> Result<MemberSnapshot, Error> {
        let (first_count, first_display) = self.room_member_view(room).await?;
        let mut first_members = self.room_members(room).await?;
        let (second_count, second_display) = self.room_member_view(room).await?;
        let mut second_members = self.room_members(room).await?;
        let (first_ids, first_duplicate) = dedup_members(&mut first_members);
        let (second_ids, second_duplicate) = dedup_members(&mut second_members);
        let reason = if first_duplicate || second_duplicate {
            MemberSnapshotLimit::DuplicateMember
        } else if first_count != second_count
            || first_display != second_display
            || first_ids != second_ids
        {
            MemberSnapshotLimit::MembershipChanged
        } else if let Some(count) = second_count {
            if second_ids.len() == count as usize && second_display.is_subset(&second_ids) {
                return Ok(MemberSnapshot::Complete(second_members));
            }
            MemberSnapshotLimit::CountMismatch
        } else {
            MemberSnapshotLimit::MissingCount
        };
        Ok(MemberSnapshot::Partial {
            members: second_members,
            expected_count: second_count,
            reason,
        })
    }

    async fn room_member_view(&self, room: RoomId) -> Result<(Option<u32>, HashSet<i64>), Error> {
        let info = self.room_document(room).await?;
        let count = count(&info, "activeMembersCount")?;
        let display = if info.contains_key("displayMembers") {
            let ids = values(&info, "displayMembers")?
                .iter()
                .map(|value| {
                    let member = value.as_document().ok_or(Error::InvalidResponse)?;
                    required_number(member, "userId")
                })
                .collect::<Result<Vec<_>, _>>()?;
            unique_members(ids.into_iter())?
        } else {
            HashSet::new()
        };
        Ok((count, display))
    }
}

fn dedup_members(members: &mut Members) -> (HashSet<i64>, bool) {
    let mut seen = HashSet::new();
    let total = members.members.len();
    members.members.retain(|member| seen.insert(member.user_id));
    (seen, members.members.len() != total)
}

fn room_link(data: &Document) -> Result<Option<OpenLinkId>, Error> {
    match data.get("li").map(number).transpose()? {
        None | Some(0) => Ok(None),
        Some(value) => OpenLinkId::new(value)
            .map(Some)
            .map_err(|_| Error::InvalidResponse),
    }
}

#[cfg(feature = "messaging")]
fn room_log(data: &Document, name: &str) -> Result<Option<RoomLog>, Error> {
    match data.get(name).map(number).transpose()? {
        None => Ok(None),
        Some(0) => Ok(Some(RoomLog::Empty)),
        Some(id) => crate::messaging::MessageId::new(id)
            .map(|id| Some(RoomLog::Id(id)))
            .map_err(|_| Error::InvalidResponse),
    }
}

fn unique_members(ids: impl Iterator<Item = i64>) -> Result<HashSet<i64>, Error> {
    let mut seen = HashSet::new();
    for id in ids {
        if id <= 0 || !seen.insert(id) {
            return Err(Error::InvalidResponse);
        }
    }
    Ok(seen)
}

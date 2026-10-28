//! Bounded historical SYNCMSG profile and explicit processing checkpoints.
//! Source evidence/limitations: docs/research.md. No live-compatibility claim.
use crate::{
    messaging::{self, Message, MessageId},
    read_only::RoomId,
    session::Session,
};
use bson::raw::RawBsonRef;
use sculk_protocol::{Command, Packet};
use std::collections::{HashSet, VecDeque};
use tokio::sync::watch;

/// Synchronization failure. Never includes payload, room or account information.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid local count/range/budget.
    #[error("invalid synchronization request")]
    InvalidRequest,
    /// Missing, duplicated, mistyped, conflicting, foreign-room or out-of-range data.
    #[error("invalid synchronization response")]
    InvalidResponse,
    /// Required documented schema such as isOK/chatLogs was absent.
    #[error("unsupported synchronization response schema")]
    UnsupportedSchema,
    /// Server ignored a local bound, or the operation exhausted its budget.
    #[error("synchronization budget exceeded")]
    Limit,
    /// Incomplete page provided no forward progress. No automatic repeat.
    #[error("synchronization cursor did not progress")]
    NoProgress,
    /// Explicit peer rejection, preserved without inferred meaning.
    #[error("synchronization rejected with status {0}")]
    Rejected(i64),
    /// Cancellation or disappearance of its sender. Checkpoint unchanged.
    #[error("synchronization cancelled")]
    Cancelled,
    /// Authentication/transport error. No implicit replay or WRITE retry.
    #[error(transparent)]
    Session(#[from] crate::session::RequestError),
    /// Invalid processing boundary, wrong room, concurrent page, or pending gap.
    #[error("invalid synchronization processing boundary")]
    Boundary,
    /// Store failed; in-memory cursor is not advanced. Side effects may repeat.
    #[error("checkpoint storage failed")]
    Storage,
}
impl From<messaging::InvalidMessage> for Error {
    fn from(_: messaging::InvalidMessage) -> Self {
        Self::InvalidResponse
    }
}
/// Exact forward cursor. Zero means initial query, not a fabricated message ID.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor(i64);
impl Cursor {
    /// Reject negative values, preserving signed Int64 precision.
    pub fn new(value: i64) -> Result<Self, Error> {
        if value < 0 {
            Err(Error::InvalidRequest)
        } else {
            Ok(Self(value))
        }
    }
    /// Explicit access; diagnostic formatting redacts this value.
    pub fn get(self) -> i64 {
        self.0
    }
}
impl std::fmt::Debug for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cursor([REDACTED])")
    }
}
/// One finite SYNCMSG request, with a caller-observed upper watermark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    room: RoomId,
    from: Cursor,
    through: MessageId,
    count: u16,
}
impl Request {
    /// Local per-page policy: 1..100 logs, from < through. IDs need not be contiguous.
    pub fn new(room: RoomId, from: Cursor, through: MessageId, count: u16) -> Result<Self, Error> {
        if count == 0 || count > 100 || from.get() >= through.get() {
            return Err(Error::InvalidRequest);
        }
        Ok(Self {
            room,
            from,
            through,
            count,
        })
    }
    /// Agent Messenger's exact command shape. Encoding sends nothing.
    pub fn packet(self) -> Result<Packet, Error> {
        Packet::from_document(0,Command::new("SYNCMSG").expect("constant command"),
            &bson::doc!{"chatId":self.room.get(),"cur":self.from.get(),"cnt":i32::from(self.count),"max":self.through.get()})
            .map_err(|_|Error::InvalidRequest)
    }
}
/// A distinct synchronization delivery: original response bytes plus ordered
/// zero-copy message views. Never masquerades as ordinary realtime MSG.
#[derive(Debug)]
pub struct Page {
    request: Request,
    packet: Packet,
    messages: Vec<Message>,
    complete: bool,
    minimum: Option<MessageId>,
    duplicate_records: usize,
}
impl Page {
    /// Validate a single researched response. Reordering/duplicate removal is only
    /// inside this room-bound page. Conflicting same-ID records fail closed.
    pub fn parse(request: Request, packet: Packet) -> Result<Self, Error> {
        use messaging::{body, fields, integer, value};
        if packet.command.as_str() != "SYNCMSG" {
            return Err(Error::UnsupportedSchema);
        }
        if packet.status != 0 {
            return Err(Error::Rejected(i64::from(packet.status as i16)));
        }
        let doc = body(&packet)?;
        fields(doc)?;
        if value(doc, "status")?.is_some() {
            let code = integer(doc, "status")?;
            if code != 0 {
                return Err(Error::Rejected(code));
            }
        }
        let complete = match value(doc, "isOK")? {
            Some(RawBsonRef::Boolean(b)) => b,
            _ => return Err(Error::UnsupportedSchema),
        };
        let minimum = if value(doc, "minLogId")?.is_some() {
            Some(MessageId::new(integer(doc, "minLogId")?)?)
        } else {
            None
        };
        // lastTokenId/jsi/li are opaque, never clocks or fabricated continuations.
        for key in ["lastTokenId", "jsi", "li"] {
            if value(doc, key)?.is_some() {
                integer(doc, key)?;
            }
        }
        let array = match value(doc, "chatLogs")? {
            Some(RawBsonRef::Array(a)) => Some(a),
            None if complete => None,
            None => return Err(Error::UnsupportedSchema),
            _ => return Err(Error::InvalidResponse),
        };
        let mut messages = Vec::new();
        if let Some(array) = array {
            for (index, item) in array.into_iter().enumerate() {
                if index >= usize::from(request.count) {
                    return Err(Error::Limit);
                }
                let log = match item.map_err(|_| Error::InvalidResponse)? {
                    RawBsonRef::Document(log) => log,
                    _ => return Err(Error::InvalidResponse),
                };
                let message = Message::from_history(packet.clone(), log)?;
                if message.room != request.room || message.id.get() > request.through.get() {
                    return Err(Error::InvalidResponse);
                }
                // References filter inclusive echoes and older records; they are
                // not delivered again and cannot drive pagination backward.
                if message.id.get() > request.from.get() {
                    messages.push(message)
                }
            }
        }
        messages.sort_unstable_by_key(|m| m.id.get());
        for pair in messages.windows(2) {
            if pair[0].id == pair[1].id && !pair[0].same_log(&pair[1]) {
                return Err(Error::InvalidResponse);
            }
        }
        let before = messages.len();
        messages.dedup_by_key(|m| m.id);
        let duplicate_records = before - messages.len();
        if !complete && messages.is_empty() {
            return Err(Error::NoProgress);
        }
        Ok(Self {
            request,
            packet,
            messages,
            complete,
            minimum,
            duplicate_records,
        })
    }
    /// Ascending log IDs within one verified room, preserving unknown message types.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
    /// Server isOK flag for the requested window, not global completeness.
    pub fn server_complete(&self) -> bool {
        self.complete
    }
    /// Server-declared minimum available log, if present.
    pub fn minimum_available(&self) -> Option<MessageId> {
        self.minimum
    }
    /// Number of identical same-ID records removed inside this page.
    pub fn duplicate_records(&self) -> usize {
        self.duplicate_records
    }
    /// Original source response, including unknown fields. Sensitive; do not log.
    pub fn packet(&self) -> &Packet {
        &self.packet
    }
    /// Proposed next request cursor. It is NOT a committed processing checkpoint.
    pub fn next_cursor(&self) -> Cursor {
        Cursor(
            self.messages
                .last()
                .map_or(self.request.from.get(), |m| m.id.get()),
        )
    }
}
impl Session {
    /// One bounded, cancellable history request. No pagination or retry occurs.
    pub async fn synchronize(
        &self,
        request: Request,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Page, Error> {
        tokio::select! {biased;
            _=crate::recovery::cancelled(&mut cancel)=>Err(Error::Cancelled),
            packet=self.request(request.packet()?)=>Page::parse(request,packet?),
        }
    }
    /// Read CHATINFO.lastLogId without entering a room or changing read marks.
    /// Missing schema fails explicitly; zero means no message watermark.
    pub async fn room_watermark(
        &self,
        room: RoomId,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Option<MessageId>, Error> {
        let request = Packet::from_document(
            0,
            Command::new("CHATINFO").expect("constant command"),
            &bson::doc! {"chatId":room.get()},
        )
        .map_err(|_| Error::InvalidRequest)?;
        let packet = tokio::select! {biased;_=crate::recovery::cancelled(&mut cancel)=>return Err(Error::Cancelled),p=self.request(request)=>p?};
        if packet.status != 0 {
            return Err(Error::Rejected(i64::from(packet.status as i16)));
        }
        let doc = messaging::body(&packet)?;
        messaging::fields(doc)?;
        if packet.command.as_str() != "CHATINFO" {
            return Err(Error::UnsupportedSchema);
        }
        if messaging::value(doc, "status")?.is_some() {
            let code = messaging::integer(doc, "status")?;
            if code != 0 {
                return Err(Error::Rejected(code));
            }
        }
        let data = match messaging::value(doc, "chatInfo")? {
            Some(RawBsonRef::Document(d)) => d,
            _ => return Err(Error::UnsupportedSchema),
        };
        messaging::fields(data)?;
        if messaging::integer(data, "chatId")? != room.get() {
            return Err(Error::InvalidResponse);
        }
        match messaging::integer(data, "lastLogId").map_err(|_| Error::UnsupportedSchema)? {
            0 => Ok(None),
            n => Ok(Some(MessageId::new(n)?)),
        }
    }
}
/// Narrow durable boundary. Implementations must atomically compare-and-swap
/// one room's cursor. No message body, token, timestamp or account is persisted.
/// This synchronous interface belongs at startup/a blocking worker for disk I/O.
pub trait CheckpointStore {
    /// Current acknowledged boundary for this room, or zero initially.
    fn load(&self, room: RoomId) -> Result<Cursor, StoreError>;
    /// Atomically replace expected with next; refuse stale/backward commits.
    fn commit(&mut self, room: RoomId, expected: Cursor, next: Cursor) -> Result<(), StoreError>;
}
/// Redacted storage failure; no sensitive path or identifiers in diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("checkpoint storage failed or conflicted")]
pub struct StoreError;
/// Lightweight single-room in-memory store. Not durable across process restart.
#[derive(Debug)]
pub struct MemoryStore {
    room: RoomId,
    cursor: Cursor,
}
impl MemoryStore {
    /// Explicit room and starting checkpoint, supplied by the owner/application.
    pub fn new(room: RoomId, cursor: Cursor) -> Self {
        Self { room, cursor }
    }
}
impl CheckpointStore for MemoryStore {
    fn load(&self, room: RoomId) -> Result<Cursor, StoreError> {
        if room == self.room {
            Ok(self.cursor)
        } else {
            Err(StoreError)
        }
    }
    fn commit(&mut self, room: RoomId, expected: Cursor, next: Cursor) -> Result<(), StoreError> {
        if room != self.room || self.cursor != expected || next.get() < expected.get() {
            return Err(StoreError);
        }
        self.cursor = next;
        Ok(())
    }
}
/// Evidence about continuity. IDs alone never confirm that messages are missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gap {
    /// No unresolved gap is known; not a guarantee of global history completeness.
    None,
    /// Startup/reconnect, lag, missing predecessor, or reordered delivery.
    Suspected,
    /// Server returned an unprocessed historical log in a previously suspect range.
    ConfirmedUnprocessed,
    /// Server reports a minimum newer than the requested starting cursor.
    UnavailableHistory,
}
/// Bounded dedup result. Older IDs outside the retained window are not falsely
/// declared duplicates; application idempotency may be necessary after restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observation {
    /// Exact recently processed ID, or the stored checkpoint itself.
    Duplicate,
    /// New forward log; predecessor may still require recovery.
    New,
    /// Older/out-of-order ID outside the dedup window; not silently discarded.
    OlderUntracked,
}
struct ActivePage {
    packet: Packet,
    request: Request,
    next: usize,
    len: usize,
    complete: bool,
    limited: bool,
    epoch: u64,
}
/// One room's serial processing boundary and bounded recent-ID set. No task, timer,
/// content cache or assumed contiguous-ID arithmetic. Use one owner per room.
pub struct Tracker {
    room: RoomId,
    cursor: Cursor,
    capacity: usize,
    seen: HashSet<MessageId>,
    order: VecDeque<MessageId>,
    gap: Gap,
    epoch: u64,
    active: Option<ActivePage>,
}
impl Tracker {
    /// Load only a previously acknowledged checkpoint. Starts suspect until a
    /// server-backed window is processed; process restart is not lossless evidence.
    pub fn new(room: RoomId, capacity: usize, store: &impl CheckpointStore) -> Result<Self, Error> {
        if capacity == 0 || capacity > 65536 {
            return Err(Error::InvalidRequest);
        }
        Ok(Self {
            room,
            cursor: store.load(room).map_err(|_| Error::Storage)?,
            capacity,
            seen: HashSet::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            gap: Gap::Suspected,
            epoch: 0,
            active: None,
        })
    }
    /// Last committed processing boundary, never the newest merely received log.
    pub fn cursor(&self) -> Cursor {
        self.cursor
    }
    /// Current continuity evidence.
    pub fn gap(&self) -> Gap {
        self.gap
    }
    /// Mark explicit lag/reconnect/disconnection. An in-progress page cannot clear
    /// a newer gap. No silent packet drop or claim of completed recovery follows.
    pub fn suspect_gap(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.gap != Gap::UnavailableHistory {
            self.gap = Gap::Suspected
        }
    }
    /// Classify without marking a message processed. Processing failure must not
    /// populate the dedup set or advance the cursor.
    pub fn observe(&self, message: &Message) -> Result<Observation, Error> {
        if message.room != self.room {
            return Err(Error::Boundary);
        }
        if message.id.get() == self.cursor.get() || self.seen.contains(&message.id) {
            Ok(Observation::Duplicate)
        } else if message.id.get() < self.cursor.get() {
            Ok(Observation::OlderUntracked)
        } else {
            Ok(Observation::New)
        }
    }
    fn remember(&mut self, id: MessageId) {
        if self.seen.contains(&id) {
            return;
        }
        if self.order.len() == self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        self.seen.insert(id);
        self.order.push_back(id);
    }
    /// Call only AFTER application processing succeeds. Live gaps freeze the safe
    /// checkpoint, while remembering processed IDs for subsequent history dedup.
    /// Side effects and this commit are not an exactly-once transaction.
    pub fn acknowledge_live(
        &mut self,
        message: &Message,
        store: &mut impl CheckpointStore,
    ) -> Result<(), Error> {
        let observation = self.observe(message)?;
        if observation == Observation::Duplicate {
            return Ok(());
        }
        if self.active.is_none()
            && self.gap == Gap::None
            && message.id.get() > self.cursor.get()
            && message.previous_id() == Some(self.cursor.get())
        {
            let next = Cursor(message.id.get());
            store
                .commit(self.room, self.cursor, next)
                .map_err(|_| Error::Storage)?;
            self.cursor = next;
        } else {
            self.suspect_gap()
        }
        self.remember(message.id);
        Ok(())
    }
    /// Begin processing one parsed server page from exactly the committed cursor.
    pub fn begin_page(&mut self, page: &Page) -> Result<(), Error> {
        if self.active.is_some()
            || page.request.room != self.room
            || page.request.from != self.cursor
        {
            return Err(Error::Boundary);
        }
        let limited = page
            .minimum
            .is_some_and(|id| id.get() > page.request.from.get());
        if limited {
            self.gap = Gap::UnavailableHistory
        } else if self.gap != Gap::None && page.messages.iter().any(|m| !self.seen.contains(&m.id))
        {
            self.gap = Gap::ConfirmedUnprocessed
        }
        self.active = Some(ActivePage {
            packet: page.packet.clone(),
            request: page.request,
            next: 0,
            len: page.messages.len(),
            complete: page.complete,
            limited,
            epoch: self.epoch,
        });
        Ok(())
    }
    /// After processing (or recognizing a duplicate), commit each history entry
    /// strictly in page order. A storage failure leaves this entry unacknowledged.
    pub fn acknowledge_history(
        &mut self,
        page: &Page,
        index: usize,
        store: &mut impl CheckpointStore,
    ) -> Result<(), Error> {
        let active = self.active.as_ref().ok_or(Error::Boundary)?;
        if active.packet != page.packet || active.request != page.request || active.next != index {
            return Err(Error::Boundary);
        }
        let message = page.messages.get(index).ok_or(Error::Boundary)?;
        let next = Cursor(message.id.get());
        if next.get() <= self.cursor.get() {
            return Err(Error::Boundary);
        }
        store
            .commit(self.room, self.cursor, next)
            .map_err(|_| Error::Storage)?;
        self.cursor = next;
        self.remember(message.id);
        self.active.as_mut().expect("active page").next += 1;
        Ok(())
    }
    /// Complete only after all returned entries were acknowledged. Returns the
    /// actual gap state: a server flag never clears truncation or a newer lag.
    pub fn finish_page(&mut self) -> Result<Gap, Error> {
        let active = self.active.as_ref().ok_or(Error::Boundary)?;
        if active.next != active.len {
            return Err(Error::Boundary);
        }
        if active.complete
            && !active.limited
            && active.epoch == self.epoch
            && self.gap != Gap::UnavailableHistory
        {
            self.gap = if self.cursor.get() == active.request.through.get() {
                Gap::None
            } else {
                Gap::Suspected
            };
        }
        self.active = None;
        Ok(self.gap)
    }
    /// Cancel a partial page/reconnect from the last committed entry, preserving
    /// failed/unprocessed entries for a new explicit read-only request.
    pub fn abandon_page(&mut self) {
        self.active = None;
        self.suspect_gap()
    }
}

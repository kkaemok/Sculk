#![cfg(feature = "synchronization")]
use bson::{doc, Document};
use sculk_client::{
    messaging::{Message, MessageId, MessageType},
    read_only::RoomId,
    synchronization::{
        CheckpointStore, Cursor, Error, Gap, MemoryStore, Observation, Page, Request, StoreError,
        Tracker,
    },
};
use sculk_protocol::{Command, Packet};
const ID: i64 = 9_007_199_254_740_993;
fn room() -> RoomId {
    RoomId::new(ID).unwrap()
}
fn cursor(n: i64) -> Cursor {
    Cursor::new(n).unwrap()
}
fn request(from: i64, count: u16) -> Request {
    Request::new(
        room(),
        cursor(from),
        MessageId::new(ID + 100).unwrap(),
        count,
    )
    .unwrap()
}
fn packet(d: Document) -> Packet {
    Packet::from_document(0, Command::new("SYNCMSG").unwrap(), &d).unwrap()
}
fn log(id: i64, prev: i64) -> Document {
    doc! {"chatId":ID,"logId":id,"authorId":ID+1000,"type":1,"message":"synthetic history","attachment":"{}","sendAt":1700000000,"prevId":prev}
}
fn live(id: i64, prev: i64) -> Message {
    Message::from_packet(
        Packet::from_document(
            0,
            Command::new("MSG").unwrap(),
            &doc! {"chatId":ID,"chatLog":log(id,prev)},
        )
        .unwrap(),
    )
    .unwrap()
}
fn page(from: i64, logs: Vec<Document>, complete: bool) -> Page {
    let through = logs
        .iter()
        .map(|log| log.get_i64("logId").unwrap())
        .max()
        .unwrap_or(ID + 100);
    let request =
        Request::new(room(), cursor(from), MessageId::new(through).unwrap(), 100).unwrap();
    Page::parse(request, packet(doc! {"isOK":complete,"chatLogs":logs})).unwrap()
}
#[test]
fn source_request_and_synthetic_reply_preserve_large_ids_and_unknown_types() {
    let req = request(ID, 2);
    let bytes = include_bytes!("../../../tests/fixtures/agent-syncmsg-request.bin");
    assert_eq!(req.packet().unwrap().body(), &bytes[22..]);
    assert_eq!(&bytes[6..13], b"SYNCMSG");
    let bytes = include_bytes!("../../../tests/fixtures/node-syncmsg-synthetic.bin");
    let reply = Packet::from_bytes(
        1,
        0,
        Command::new("SYNCMSG").unwrap(),
        bytes[22..].to_vec().into(),
    )
    .unwrap();
    let result = Page::parse(req, reply.clone()).unwrap();
    assert!(result.server_complete());
    assert_eq!(result.messages().len(), 2);
    assert_eq!(result.messages()[0].id.get(), ID + 1);
    assert_eq!(result.messages()[1].kind, MessageType::Unknown(999));
    assert_eq!(
        result.messages()[0].sent_at_seconds,
        result.messages()[1].sent_at_seconds
    );
    assert_eq!(result.messages()[0].text(), Some("Synthetic history"));
    assert_eq!(
        result.messages()[0].packet().body().as_ptr(),
        reply.body().as_ptr()
    );
    assert_eq!(result.next_cursor().get(), ID + 2);
}
#[test]
fn bounded_pages_sort_room_logs_remove_only_identical_duplicates_and_do_not_invent_completion() {
    let first = log(ID + 10, ID);
    let second = log(ID + 80, ID + 10);
    let p = page(
        ID,
        vec![second.clone(), first.clone(), first.clone()],
        false,
    );
    assert_eq!(
        p.messages().iter().map(|m| m.id.get()).collect::<Vec<_>>(),
        [ID + 10, ID + 80]
    );
    assert_eq!(p.duplicate_records(), 1);
    assert!(!p.server_complete());
    let mut conflict = first.clone();
    conflict.insert("message", "conflicting content");
    assert!(matches!(
        Page::parse(
            request(ID, 3),
            packet(doc! {"isOK":true,"chatLogs":[first,conflict]})
        ),
        Err(Error::InvalidResponse)
    ));
    assert!(matches!(
        Page::parse(
            request(ID, 1),
            packet(doc! {"isOK":true,"chatLogs":[second.clone(),second]})
        ),
        Err(Error::Limit)
    ));
    assert!(matches!(
        Page::parse(request(ID, 1), packet(doc! {"isOK":false,"chatLogs":[]})),
        Err(Error::NoProgress)
    ));
    // Numeric differences do not imply missing logs. No +/- 1 cursor arithmetic.
    let p = page(ID, vec![log(ID + 80, ID)], true);
    assert_eq!(p.messages()[0].id.get(), ID + 80);
}
#[test]
fn unsupported_corrupt_rejected_and_foreign_room_responses_fail_closed() {
    for d in [
        doc! {},
        doc! {"eof":true,"chatLogs":[]},
        doc! {"isOK":0,"chatLogs":[]},
    ] {
        assert!(matches!(
            Page::parse(request(ID, 2), packet(d)),
            Err(Error::UnsupportedSchema)
        ));
    }
    for mut l in [log(ID + 1, ID), log(ID + 1, ID), log(ID + 101, ID)] {
        if l.get_i64("logId").unwrap() <= ID + 100 {
            l.insert("chatId", ID + 1);
        }
        assert!(matches!(
            Page::parse(request(ID, 2), packet(doc! {"isOK":true,"chatLogs":[l]})),
            Err(Error::InvalidResponse)
        ));
    }
    let mut l = log(ID + 1, ID);
    l.insert("authorId", ID as f64);
    assert!(matches!(
        Page::parse(request(ID, 2), packet(doc! {"isOK":true,"chatLogs":[l]})),
        Err(Error::InvalidResponse)
    ));
    let mut raw = bson::raw::RawDocumentBuf::new();
    raw.append("isOK", true);
    raw.append("isOK", false);
    let p = Packet::from_bytes(
        0,
        0,
        Command::new("SYNCMSG").unwrap(),
        raw.into_bytes().into(),
    )
    .unwrap();
    assert!(matches!(
        Page::parse(request(ID, 2), p),
        Err(Error::InvalidResponse)
    ));
    assert!(matches!(
        Page::parse(request(ID, 2), packet(doc! {"status":-203})),
        Err(Error::Rejected(-203))
    ));
    assert!(Request::new(
        room(),
        cursor(ID + 100),
        MessageId::new(ID + 100).unwrap(),
        2
    )
    .is_err());
    assert!(Request::new(room(), cursor(ID), MessageId::new(ID + 100).unwrap(), 101).is_err());
}
#[test]
fn checkpoints_follow_successful_processing_and_gap_evidence_not_receipt_or_timestamps() {
    let mut store = MemoryStore::new(room(), cursor(ID));
    let mut tracker = Tracker::new(room(), 4, &store).unwrap();
    let p = page(ID, vec![log(ID + 10, ID), log(ID + 30, ID + 10)], true);
    assert_eq!(tracker.cursor(), cursor(ID));
    tracker.begin_page(&p).unwrap();
    assert_eq!(tracker.gap(), Gap::ConfirmedUnprocessed);
    assert!(tracker.acknowledge_history(&p, 1, &mut store).is_err());
    assert!(tracker.finish_page().is_err());
    assert_eq!(store.load(room()).unwrap(), cursor(ID));
    for (i, m) in p.messages().iter().enumerate() {
        assert_eq!(tracker.observe(m).unwrap(), Observation::New);
        tracker.acknowledge_history(&p, i, &mut store).unwrap();
    }
    assert_eq!(tracker.finish_page().unwrap(), Gap::None);
    let m = live(ID + 80, ID + 30);
    tracker.acknowledge_live(&m, &mut store).unwrap();
    assert_eq!(tracker.cursor(), cursor(ID + 80));
    assert_eq!(tracker.observe(&m).unwrap(), Observation::Duplicate);
    let out_of_order = live(ID + 50, ID + 30);
    assert_eq!(
        tracker.observe(&out_of_order).unwrap(),
        Observation::OlderUntracked
    );
    tracker.acknowledge_live(&out_of_order, &mut store).unwrap();
    assert_eq!(tracker.cursor(), cursor(ID + 80));
    assert_eq!(tracker.gap(), Gap::Suspected);
    let future = live(ID + 100, ID + 90);
    tracker.acknowledge_live(&future, &mut store).unwrap();
    assert_eq!(tracker.cursor(), cursor(ID + 80));
    assert_eq!(tracker.observe(&future).unwrap(), Observation::Duplicate);
    // Server history confirms the previously unprocessed log; observed future log
    // is deduplicated but must still be acknowledged to advance the safe cursor.
    let p = page(
        ID + 80,
        vec![log(ID + 90, ID + 80), log(ID + 100, ID + 90)],
        true,
    );
    tracker.begin_page(&p).unwrap();
    assert_eq!(tracker.gap(), Gap::ConfirmedUnprocessed);
    assert_eq!(
        tracker.observe(&p.messages()[1]).unwrap(),
        Observation::Duplicate
    );
    tracker.acknowledge_history(&p, 0, &mut store).unwrap();
    tracker.acknowledge_history(&p, 1, &mut store).unwrap();
    assert_eq!(tracker.finish_page().unwrap(), Gap::None);
}
#[test]
fn truncation_and_lag_during_sync_cannot_be_reported_as_recovered() {
    let mut store = MemoryStore::new(room(), cursor(ID));
    let mut tracker = Tracker::new(room(), 2, &store).unwrap();
    let p = page(ID, vec![log(ID + 10, ID)], true);
    tracker.begin_page(&p).unwrap();
    tracker.suspect_gap();
    tracker.acknowledge_history(&p, 0, &mut store).unwrap();
    assert_eq!(tracker.finish_page().unwrap(), Gap::Suspected);
    let p = Page::parse(
        request(ID + 10, 2),
        packet(doc! {"isOK":true,"minLogId":ID+20,"chatLogs":[log(ID+30,ID+20)]}),
    )
    .unwrap();
    tracker.begin_page(&p).unwrap();
    tracker.acknowledge_history(&p, 0, &mut store).unwrap();
    assert_eq!(tracker.finish_page().unwrap(), Gap::UnavailableHistory);
}
#[test]
fn failed_storage_does_not_advance_boundary_and_bounded_dedup_never_labels_evicted_ids_duplicates()
{
    struct Failing(MemoryStore, bool);
    impl CheckpointStore for Failing {
        fn load(&self, r: RoomId) -> Result<Cursor, StoreError> {
            self.0.load(r)
        }
        fn commit(&mut self, r: RoomId, e: Cursor, n: Cursor) -> Result<(), StoreError> {
            if self.1 {
                Err(StoreError)
            } else {
                self.0.commit(r, e, n)
            }
        }
    }
    let mut store = Failing(MemoryStore::new(room(), cursor(ID)), true);
    let mut tracker = Tracker::new(room(), 1, &store).unwrap();
    let p = page(ID, vec![log(ID + 10, ID), log(ID + 20, ID + 10)], true);
    tracker.begin_page(&p).unwrap();
    assert!(matches!(
        tracker.acknowledge_history(&p, 0, &mut store),
        Err(Error::Storage)
    ));
    assert_eq!(tracker.cursor(), cursor(ID));
    assert_eq!(tracker.observe(&p.messages()[0]).unwrap(), Observation::New);
    store.1 = false;
    tracker.acknowledge_history(&p, 0, &mut store).unwrap();
    tracker.abandon_page();
    assert_eq!(store.load(room()).unwrap(), cursor(ID + 10));
    let resumed = page(ID + 10, vec![log(ID + 20, ID + 10)], true);
    tracker.begin_page(&resumed).unwrap();
    tracker
        .acknowledge_history(&resumed, 0, &mut store)
        .unwrap();
    tracker.finish_page().unwrap();
    assert_eq!(
        tracker.observe(&p.messages()[0]).unwrap(),
        Observation::OlderUntracked
    );
    let restarted = Tracker::new(room(), 1, &store).unwrap();
    assert_eq!(restarted.gap(), Gap::Suspected);
    assert_eq!(
        restarted.observe(&resumed.messages()[0]).unwrap(),
        Observation::Duplicate
    );
}

#[test]
fn server_completion_without_requested_anchor_does_not_clear_a_suspected_gap() {
    let mut store = MemoryStore::new(room(), cursor(ID));
    let mut tracker = Tracker::new(room(), 4, &store).unwrap();
    let p = Page::parse(
        request(ID, 2),
        packet(doc! {"isOK":true,"chatLogs":[log(ID+1,ID)]}),
    )
    .unwrap();
    tracker.begin_page(&p).unwrap();
    tracker.acknowledge_history(&p, 0, &mut store).unwrap();
    assert_eq!(tracker.finish_page().unwrap(), Gap::Suspected);
}

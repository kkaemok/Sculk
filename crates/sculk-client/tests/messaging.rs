#![cfg(feature = "messaging")]
mod support;
use bson::doc;
use futures_util::{SinkExt, StreamExt};
use sculk_client::{
    messaging::{Event, Message, MessageType, ReceiveError, SendError, TextRequest},
    read_only::RoomId,
    session::{AccessToken, Grant, RequestError, Session},
    Config, Error,
};
use sculk_protocol::{Command, LocoCodec, Packet};
use std::time::Duration;
use tokio::{io::DuplexStream, sync::watch, time::Instant};
use tokio_rustls::server::TlsStream;
use tokio_util::codec::Framed;
const ID: i64 = 9_007_199_254_740_993;
fn packet(command: &str, doc: bson::Document) -> Packet {
    Packet::from_document(0, Command::new(command).unwrap(), &doc).unwrap()
}
// Synthetic fields following node-kakao MsgRes/ChatlogStruct. Not a live capture.
fn message() -> Packet {
    packet(
        "MSG",
        doc! {"chatId":ID,"logId":ID+1,"noSeen":true,"authorNickname":"private nickname",
        "chatLog":{"chatId":ID,"logId":ID+1,"authorId":ID+2,"type":1,"message":"private text",
        "sendAt":1_700_000_000,"attachment":"{bad json preserved}","msgId":3,"prevId":0_i64,
        "supplement":"opaque", "future":{"key":1}}, "future":"retained"},
    )
}
#[test]
fn incoming_schema_preserves_exact_ids_content_attachments_and_unknown_metadata() {
    let packet = message();
    let m = Message::from_packet(packet.clone()).unwrap();
    assert_eq!(m.room.get(), ID);
    assert_eq!(m.id.get(), ID + 1);
    assert_eq!(m.sender.get(), ID + 2);
    assert_eq!(m.sent_at_seconds, 1_700_000_000);
    assert_eq!(m.kind, MessageType::Text);
    assert_eq!(m.text(), Some("private text"));
    assert_eq!(m.attachment(), Some("{bad json preserved}"));
    assert_eq!(m.packet(), &packet);
    assert_eq!(m.packet().body().as_ptr(), packet.body().as_ptr());
    let debug = format!("{m:?}");
    for secret in ["private", "bad json", "9007199254740993"] {
        assert!(!debug.contains(secret));
    }
    for (wire, expected) in [
        (0, MessageType::Feed),
        (2, MessageType::Photo),
        (3, MessageType::Video),
        (5, MessageType::Audio),
        (18, MessageType::File),
        (26, MessageType::Reply),
        (16385, MessageType::Unknown(16385)),
        (999, MessageType::Unknown(999)),
    ] {
        let mut d = packet.document().unwrap();
        d.get_document_mut("chatLog").unwrap().insert("type", wire);
        assert_eq!(
            Message::from_packet(crate::packet("MSG", d)).unwrap().kind,
            expected
        );
    }
}
#[test]
fn malformed_known_and_unknown_commands_preserve_raw_evidence() {
    let original = message();
    for (key, bad) in [
        ("logId", bson::Bson::Double(ID as f64)),
        ("authorId", 0_i64.into()),
        ("sendAt", (-1).into()),
        ("message", bson::Bson::Null),
        ("attachment", bson::Bson::Boolean(false)),
        ("chatId", (ID + 1).into()),
        ("type", i64::MAX.into()),
    ] {
        let mut d = original.document().unwrap();
        d.get_document_mut("chatLog").unwrap().insert(key, bad);
        let p = packet("MSG", d);
        match Event::from_packet(p.clone()) {
            Event::Malformed(raw) => assert_eq!(raw, p),
            _ => panic!("invalid MSG accepted"),
        }
    }
    let mut d = original.document().unwrap();
    d.insert("logId", ID + 4);
    assert!(Message::from_packet(packet("MSG", d)).is_err());
    let mut d = original.document().unwrap();
    d.get_document_mut("chatLog").unwrap().remove("sendAt");
    assert!(Message::from_packet(packet("MSG", d)).is_err());
    // Duplicates survive framing but must not be collapsed by owned BSON decoding.
    let mut raw = bson::raw::RawDocumentBuf::new();
    raw.append("chatId", ID);
    raw.append("chatId", ID + 1);
    raw.append(
        "chatLog",
        bson::raw::RawDocumentBuf::from_document(
            original
                .document()
                .unwrap()
                .get_document("chatLog")
                .unwrap(),
        )
        .unwrap(),
    );
    let p =
        Packet::from_bytes(0, 0, Command::new("MSG").unwrap(), raw.into_bytes().into()).unwrap();
    assert!(matches!(Event::from_packet(p), Event::Malformed(_)));
    let p = packet("KICKOUT", doc! {"feedType":6});
    assert!(matches!(Event::from_packet(p.clone()),Event::Unknown(raw) if raw==p));
    let mut p = original;
    p.status = 1;
    assert!(matches!(Event::from_packet(p), Event::Malformed(_)));
}
async fn setup(
    capacity: usize,
    lease: Duration,
) -> (Session, Framed<TlsStream<DuplexStream>, LocoCodec>) {
    let (settings, acceptor) = support::tls_pair();
    let (a, b) = tokio::io::duplex(32768);
    let (_tx, cancel) = watch::channel(false);
    let (session, peer) = tokio::join!(
        Session::establish(
            a,
            "localhost",
            &settings,
            Duration::from_secs(1),
            Config {
                events: capacity,
                request_timeout: Duration::from_millis(50),
                ..Config::default()
            },
            cancel.clone()
        ),
        acceptor.accept(b)
    );
    let mut session = session.unwrap();
    assert!(matches!(
        session.message_events(),
        Err(ReceiveError::Inactive)
    ));
    session
        .authenticate_with(
            AccessToken::new(
                "synthetic".into(),
                42,
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap(),
            Duration::from_secs(1),
            cancel,
            |_, _| async move {
                Ok(Grant {
                    user_id: 42,
                    valid_for: lease,
                })
            },
        )
        .await
        .unwrap();
    (session, Framed::new(peer.unwrap(), LocoCodec::default()))
}
async fn flush(
    session: &Session,
    peer: &mut Framed<TlsStream<DuplexStream>, LocoCodec>,
    packets: Vec<Packet>,
) {
    let (result, ()) = tokio::join!(session.request(packet("PING", doc! {})), async {
        for p in packets {
            peer.send(p).await.unwrap();
        }
        let p = peer.next().await.unwrap().unwrap();
        assert_eq!(p.command.as_str(), "PING");
        peer.send(p).await.unwrap();
    });
    result.unwrap();
}
#[tokio::test]
async fn slow_handler_does_not_block_requests_lag_is_explicit_and_cancel_does_not_consume() {
    let (mut session, mut peer) = setup(2, Duration::from_secs(30)).await;
    let mut events = session.message_events().unwrap();
    let (tx, cancel) = watch::channel(false);
    flush(&session, &mut peer, vec![message(); 10]).await;
    assert_eq!(
        events.recv(cancel.clone()).await.unwrap_err(),
        ReceiveError::Lagged(8)
    );
    tx.send_replace(true);
    assert_eq!(
        events.recv(cancel.clone()).await.unwrap_err(),
        ReceiveError::Cancelled
    );
    tx.send_replace(false);
    assert!(matches!(
        events.recv(cancel.clone()).await.unwrap(),
        Event::Message(_)
    ));
    assert!(matches!(
        events.recv(cancel.clone()).await.unwrap(),
        Event::Message(_)
    ));
    // Dropping an in-progress receive does not advance its cursor.
    assert!(
        tokio::time::timeout(Duration::from_millis(1), events.recv(cancel.clone()))
            .await
            .is_err()
    );
    flush(
        &session,
        &mut peer,
        vec![packet("FUTURE", doc! {"unknown":1})],
    )
    .await;
    assert!(matches!(
        events.recv(cancel.clone()).await.unwrap(),
        Event::Unknown(_)
    ));
    drop(tx);
    assert_eq!(
        events.recv(cancel).await.unwrap_err(),
        ReceiveError::Cancelled
    );
    session.disconnect().await;
    let (_tx, cancel) = watch::channel(false);
    assert_eq!(
        events.recv(cancel).await.unwrap_err(),
        ReceiveError::Inactive
    );
}
#[tokio::test(start_paused = true)]
async fn lease_expiry_prevents_draining_queued_content() {
    let (mut session, mut peer) = setup(4, Duration::from_secs(1)).await;
    let mut events = session.message_events().unwrap();
    let (_tx, cancel) = watch::channel(false);
    flush(&session, &mut peer, vec![message()]).await;
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(
        events.recv(cancel).await.unwrap_err(),
        ReceiveError::Expired
    );
    session.disconnect().await;
}
#[tokio::test]
async fn peer_loss_wakes_receiver_with_no_queued_event() {
    let (mut session, peer) = setup(4, Duration::from_secs(30)).await;
    let mut events = session.message_events().unwrap();
    let (_tx, cancel) = watch::channel(false);
    drop(peer);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), events.recv(cancel))
            .await
            .unwrap()
            .unwrap_err(),
        ReceiveError::Inactive
    );
    session.disconnect().await;
}
#[test]
fn text_builder_and_outcomes_do_not_confuse_timeout_with_rejection_or_not_sent() {
    let req = TextRequest::new(RoomId::new(ID).unwrap(), "private text".into()).unwrap();
    let doc = req.packet().unwrap().document().unwrap();
    assert_eq!(
        doc,
        doc! {"chatId":ID,"msg":"private text","type":1,"noSeen":false}
    );
    assert!(matches!(doc.get("chatId"), Some(bson::Bson::Int64(_))));
    assert!(!format!("{req:?}").contains("private"));
    assert!(TextRequest::new(RoomId::new(ID).unwrap(), String::new()).is_err());
    assert!(TextRequest::new(RoomId::new(ID).unwrap(), "a".repeat(16385)).is_err());
    assert!(TextRequest::new(RoomId::new(ID).unwrap(), "\0".into()).is_err());
    for (input, expected) in [
        (Error::Timeout, SendError::TimeoutUnknown),
        (Error::Closed, SendError::DeliveryUnknown),
        (Error::Mismatch, SendError::DeliveryUnknown),
        (Error::Overloaded, SendError::NotSent),
        (Error::Rejected(65535), SendError::Rejected(-1)),
    ] {
        assert_eq!(
            req.outcome(Err(RequestError::Transport(input)))
                .unwrap_err(),
            expected
        );
    }
    assert_eq!(
        req.outcome(Err(RequestError::Expired)).unwrap_err(),
        SendError::DeliveryUnknown
    );
    assert_eq!(
        req.outcome(Err(RequestError::Unauthenticated)).unwrap_err(),
        SendError::NotSent
    );
    assert_eq!(
        req.outcome(Ok(packet("WRITE", doc! {"status":-203})))
            .unwrap_err(),
        SendError::Rejected(-203)
    );
    for d in [
        doc! {},
        doc! {"chatId":ID+1,"logId":ID+2,"sendAt":1},
        doc! {"chatId":ID,"logId":ID+2,"sendAt":-1},
    ] {
        assert_eq!(
            req.outcome(Ok(packet("WRITE", d))).unwrap_err(),
            SendError::DeliveryUnknown
        );
    }
    let receipt = req
        .outcome(Ok(packet(
            "WRITE",
            doc! {"chatId":ID,"logId":ID+3,"sendAt":1_700_000_000,"msgId":1,"prevId":0_i64},
        )))
        .unwrap();
    assert_eq!(receipt.id.get(), ID + 3);
}
#[tokio::test]
async fn offline_write_roundtrip_and_timeout_send_exactly_once() {
    let (mut session, mut peer) = setup(4, Duration::from_secs(30)).await;
    let req = TextRequest::new(RoomId::new(ID).unwrap(), "synthetic local text".into()).unwrap();
    let (result, ()) = tokio::join!(session.request(req.packet().unwrap()), async {
        let request = peer.next().await.unwrap().unwrap();
        assert_eq!(request.command.as_str(), "WRITE");
        assert_eq!(
            request.document().unwrap(),
            req.packet().unwrap().document().unwrap()
        );
        let mut reply = packet("WRITE", doc! {"chatId":ID,"logId":ID+1,"sendAt":1});
        reply.id = request.id;
        peer.send(reply).await.unwrap();
    });
    assert_eq!(req.outcome(result).unwrap().id.get(), ID + 1);
    let (result, ()) = tokio::join!(session.request(req.packet().unwrap()), async {
        let request = peer.next().await.unwrap().unwrap();
        assert_eq!(request.command.as_str(), "WRITE");
        // Leave reply ambiguous. There must be no retry on the transport.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), peer.next())
                .await
                .is_err()
        );
    });
    assert_eq!(req.outcome(result).unwrap_err(), SendError::TimeoutUnknown);
    session.disconnect().await;
}

#[test]
fn text_request_matches_actual_reference_method_bytes() {
    let request = TextRequest::new(RoomId::new(ID).unwrap(), "Synthetic text".into()).unwrap();
    let encoded = include_bytes!("../../../tests/fixtures/agent-write-request.bin");
    let packet = request.packet().unwrap();
    assert_eq!(&encoded[6..11], b"WRITE");
    assert_eq!(
        u32::from_le_bytes(encoded[18..22].try_into().unwrap()) as usize,
        packet.body().len()
    );
    assert_eq!(&encoded[22..], packet.body());
}

#[test]
fn synthetic_msg_fixture_decodes_reference_framing() {
    let bytes = include_bytes!("../../../tests/fixtures/node-msg-synthetic.bin");
    let packet = Packet::from_bytes(
        0,
        0,
        Command::new("MSG").unwrap(),
        bytes[22..].to_vec().into(),
    )
    .unwrap();
    let message = Message::from_packet(packet).unwrap();
    assert_eq!(message.room.get(), ID);
    assert_eq!(message.sender.get(), ID + 2);
    assert_eq!(message.id.get(), ID + 1);
    assert_eq!(message.text(), Some("Synthetic text"));
}

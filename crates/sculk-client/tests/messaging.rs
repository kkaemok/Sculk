#![cfg(feature = "messaging")]
mod support;
use bson::doc;
use futures_util::{SinkExt, StreamExt};
use sculk_client::{
    messaging::{
        Attachment, Event, Message, MessageBuilder, MessageType, ReceiveError, SendError,
        TextRequest,
    },
    read_only::{Member, Members, OpenLinkId, RoomId, RoomMemberId},
    session::{AccessToken, Grant, RequestError, Session},
    Config, Error,
};
use sculk_protocol::{Command, LocoCodec, Packet};
use std::time::Duration;
use tokio::{io::DuplexStream, sync::watch, time::Instant};
use tokio_rustls::server::TlsStream;
use tokio_util::codec::Framed;
const ID: i64 = 9_007_199_254_740_993;

#[test]
fn historical_reaction_keeps_exact_scoped_ids_and_the_raw_action() {
    use sculk_client::messaging::LogChange;
    let raw = packet(
        "SYNCACTION",
        doc! {"userId":ID+1,"chatId":ID,"logId":ID+2,"type":100},
    );
    let event = Event::from_packet(raw.clone());
    let Some(LogChange::Reaction {
        room,
        message,
        member,
        action,
    }) = event.historical_log_change().unwrap()
    else {
        panic!("reaction observation expected");
    };
    assert_eq!(
        (room.get(), message.get(), member.get(), action),
        (ID, ID + 2, ID + 1, 100)
    );
    assert!(matches!(event, Event::Unknown(original) if original == raw));
    assert!(
        Event::from_packet(packet("SYNCDLMSG", doc! {"chatId":ID, "logId":-1_i64}))
            .historical_log_change()
            .is_err()
    );
}

#[test]
fn member_departure_preserves_a_room_scoped_observation_and_unknown_cause() {
    use sculk_client::messaging::MembershipKind;
    let raw = packet(
        "DELMEM",
        doc! {"chatId":ID,"chatLog":{"authorId":ID+1,"future":"opaque"}},
    );
    let event = Event::from_packet(raw.clone());
    let observed = event.membership().unwrap().unwrap();
    assert_eq!(observed.kind, MembershipKind::Departed);
    assert_eq!(observed.room.get(), ID);
    assert_eq!(observed.member.get(), ID + 1);
    assert!(matches!(event, Event::Unknown(original) if original == raw));
    assert!(Event::from_packet(packet("KICKOUT", doc! {"chatId":ID}))
        .membership()
        .unwrap()
        .is_none());
    assert!(Event::from_packet(packet(
        "NEWMEM",
        doc! {"chatId":ID,"chatLog":{"authorId":ID+1,"chatId":ID+2}}
    ))
    .membership()
    .is_err());
}
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
fn metadata_keeps_large_ids_and_unknown_fields_and_checks_photo_alignment() {
    let mut body = message().document().unwrap();
    let log = body.get_document_mut("chatLog").unwrap();
    log.insert("type", 27);
    let extra = format!(
        r#"{{"kl":["first","second"],"sl":[10,20],"mentions":[{{"user_id":{ID},"at":[1,3],"len":2}},{{"user_id":9007199254740993.0,"at":[2],"len":1}}],"future":{{"x":1}}}}"#
    );
    log.insert("attachment", extra.clone());
    log.insert("supplement", format!(r#"{{"scope":3,"threadId":"{ID}"}}"#));
    let msg = Message::from_packet(packet("MSG", body.clone())).unwrap();
    let content = msg.content();
    assert_eq!(content.raw_attachment, Some(&*extra));
    assert_eq!(content.thread.thread.unwrap().get(), ID);
    assert_eq!(content.thread.scope, Some(3));
    assert_eq!(content.mentions.len(), 1);
    assert_eq!(content.mentions[0].member.get(), ID);
    assert_eq!(content.mentions[0].positions, [1, 3]);
    let Some(Attachment::Photos(photos)) = content.attachment else {
        panic!("expected photos")
    };
    assert_eq!(photos[1].bytes, Some(20));
    body.get_document_mut("chatLog")
        .unwrap()
        .insert("attachment", r#"{"kl":["first","second"],"sl":[10]}"#);
    let msg = Message::from_packet(packet("MSG", body)).unwrap();
    assert!(matches!(msg.content().attachment, Some(Attachment::Opaque)));
}

#[tokio::test]
async fn composed_quote_with_mentions_uses_one_room_bound_write() {
    let (mut session, mut peer) = setup(8, Duration::from_secs(30)).await;
    let (stop, cancel) = watch::channel(false);
    let source = Message::from_packet(message()).unwrap();
    let member = RoomMemberId::new(ID + 30).unwrap();
    let members = |room, nickname: &str| Members {
        room,
        token: 0,
        members: vec![Member {
            user_id: member.get(),
            nickname: Some(nickname.to_owned()),
        }],
    };
    let snapshot = members(source.room, "홍길동");
    let target = snapshot.mention_target(member).unwrap();
    let emoji = members(source.room, "😀");
    assert!(MessageBuilder::new()
        .mention(&emoji.mention_target(member).unwrap())
        .build()
        .is_err());
    let other = members(RoomId::new(ID + 1).unwrap(), "other");
    assert!(MessageBuilder::new()
        .mention(&other.mention_target(member).unwrap())
        .reply_to(&source, None)
        .build()
        .is_err());
    let content = MessageBuilder::new()
        .text("😀literal@ ")
        .mention(&target)
        .text(" and ")
        .mention(&target)
        .reply_to(&source, Some(OpenLinkId::new(ID + 20).unwrap()))
        .build()
        .unwrap();
    let sender = session.message_sender().unwrap();
    assert_eq!(
        sender
            .send_message(RoomId::new(ID + 99).unwrap(), &content, cancel.clone())
            .await
            .unwrap_err(),
        SendError::InvalidRequest
    );
    let (sent, ()) = tokio::join!(sender.send_message(source.room, &content, cancel), async {
        let req = peer.next().await.unwrap().unwrap();
        assert_eq!(req.command.as_str(), "WRITE");
        let doc = req.document().unwrap();
        assert_eq!(doc.get_i32("type").unwrap(), 26);
        assert_eq!(
            doc.get_str("msg").unwrap(),
            "😀literal@ @홍길동 and @홍길동"
        );
        let extra: serde_json::Value = serde_json::from_str(doc.get_str("extra").unwrap()).unwrap();
        assert_eq!(extra["src_logId"].as_i64(), Some(ID + 1));
        assert_eq!(extra["src_userId"].as_i64(), Some(ID + 2));
        assert_eq!(extra["src_linkId"].as_i64(), Some(ID + 20));
        assert_eq!(
            extra["mentions"][0],
            serde_json::json!({"at":[2,3],"len":3,"user_id":ID+30})
        );
        let mut reply = packet("WRITE", doc! {"chatId":ID,"logId":ID+80,"sendAt":3_i64});
        reply.id = req.id;
        peer.send(reply).await.unwrap();
    });
    assert_eq!(sent.unwrap().id.get(), ID + 80);
    drop(stop);
    session.disconnect().await;
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

#[test]
fn unicode_text_and_raw_attachments_roundtrip_and_invalid_utf8_is_rejected() {
    let original = message();
    let mut doc = original.document().unwrap();
    let log = doc.get_document_mut("chatLog").unwrap();
    let text = "합성 메시지 👋🏽\n日本語 / e\u{301}";
    let attachment = "{\"설명\":\"합성\"}";
    log.insert("message", text);
    log.insert("attachment", attachment);
    let packet = crate::packet("MSG", doc);
    let parsed = Message::from_packet(packet.clone()).unwrap();
    assert_eq!(parsed.text(), Some(text));
    assert_eq!(parsed.attachment(), Some(attachment));
    assert!(!format!("{parsed:?}").contains("합성"));
    let mut invalid = packet.body().to_vec();
    let position = invalid
        .windows(text.len())
        .position(|w| w == text.as_bytes())
        .unwrap();
    invalid[position] = 0xff;
    assert!(Packet::from_bytes(0, 0, Command::new("MSG").unwrap(), invalid.into()).is_err());
}

#[tokio::test]
async fn authenticated_sender_correlates_room_ack_rejection_and_cancellation_without_retry() {
    let (mut session, mut peer) = setup(4, Duration::from_secs(30)).await;
    let sender = session.text_sender().unwrap();
    let room = RoomId::new(ID).unwrap();
    let (stop, cancel) = watch::channel(true);
    assert_eq!(
        sender
            .send_text(room, "synthetic", cancel.clone())
            .await
            .unwrap_err(),
        SendError::Cancelled,
    );
    stop.send_replace(false);
    let (result, ()) = tokio::join!(session.send_text(room, "안녕", cancel.clone()), async {
        let req = peer.next().await.unwrap().unwrap();
        assert_eq!(req.command.as_str(), "WRITE");
        assert_eq!(
            req.document().unwrap(),
            doc! {"chatId":ID,"msg":"안녕","type":1,"noSeen":false}
        );
        let mut ack = packet("WRITE", doc! {"chatId":ID,"logId":ID+1,"sendAt":5_i64});
        ack.id = req.id;
        peer.send(ack).await.unwrap();
    });
    assert_eq!(result.unwrap().id.get(), ID + 1);
    let (result, ()) = tokio::join!(sender.send_text(room, "synthetic", cancel.clone()), async {
        let req = peer.next().await.unwrap().unwrap();
        let mut rejected = packet("WRITE", doc! {"status":-32});
        rejected.id = req.id;
        peer.send(rejected).await.unwrap();
    });
    assert_eq!(result.unwrap_err(), SendError::Rejected(-32));
    let (result, ()) = tokio::join!(sender.send_text(room, "cancelled", cancel.clone()), async {
        assert_eq!(
            peer.next().await.unwrap().unwrap().command.as_str(),
            "WRITE"
        );
        stop.send_replace(true);
    });
    assert_eq!(result.unwrap_err(), SendError::DeliveryUnknown);
    assert!(tokio::time::timeout(Duration::from_millis(20), peer.next())
        .await
        .is_err());
    session.disconnect_checked().await.unwrap();
    stop.send_replace(false);
    assert_eq!(
        sender.send_text(room, "closed", cancel).await.unwrap_err(),
        SendError::NotSent
    );
}

#[test]
fn quote_metadata_preserves_large_ids_and_raw_attachment_on_parse_failure() {
    let mut data = message().document().unwrap();
    let log = data.get_document_mut("chatLog").unwrap();
    log.insert("type", 26);
    let attachment = format!(
        r#"{{"src_logId":{},"src_userId":{},"src_type":1,"src_message":"escaped \n line","src_linkId":{}}}"#,
        ID + 5,
        ID + 6,
        ID + 7
    );
    log.insert("attachment", &attachment);
    let msg = Message::from_packet(packet("MSG", data.clone())).unwrap();
    let quote = msg.quote().unwrap().unwrap();
    assert_eq!(quote.message.get(), ID + 5);
    assert_eq!(quote.sender.get(), ID + 6);
    assert_eq!(quote.link.unwrap().get(), ID + 7);
    assert_eq!(quote.preview, "escaped \n line");
    assert!(!format!("{quote:?}").contains("escaped"));
    let wrong = attachment.replace(&format!("\"src_logId\":{}", ID + 5), "\"src_logId\":1.0");
    data.get_document_mut("chatLog")
        .unwrap()
        .insert("attachment", &wrong);
    let msg = Message::from_packet(packet("MSG", data)).unwrap();
    assert!(msg.quote().is_err());
    assert_eq!(msg.attachment(), Some(wrong.as_str()));
}

#[tokio::test]
async fn open_link_queries_preserve_id_domains_and_reject_mismatched_results() {
    use sculk_client::read_only::{Error as ReadError, OpenLinkId};
    let (session, mut peer) = setup(4, Duration::from_secs(30)).await;
    let room = RoomId::new(ID).unwrap();
    let (link, ()) = tokio::join!(session.room_open_link(room), async {
        let req = peer.next().await.unwrap().unwrap();
        assert_eq!(req.command.as_str(), "CHATINFO");
        assert_eq!(req.document().unwrap(), doc! {"chatId":ID});
        let mut response = packet("CHATINFO", doc! {"chatInfo":{"chatId":ID,"li":ID+1}});
        response.id = req.id;
        peer.send(response).await.unwrap();
    });
    let link = link.unwrap().unwrap();
    assert_eq!(link.get(), ID + 1);
    for returned in [ID + 1, ID] {
        let (info, ()) = tokio::join!(session.open_link_info(link), async {
            let req = peer.next().await.unwrap().unwrap();
            assert_eq!(req.command.as_str(), "INFOLINK");
            assert_eq!(req.document().unwrap(), doc! {"lis":[ID+1]});
            let mut response = packet(
                "INFOLINK",
                doc! {"ols":[{"li":returned,"ln":"synthetic","lt":2}]},
            );
            response.id = req.id;
            peer.send(response).await.unwrap();
        });
        if returned == link.get() {
            let info = info.unwrap().unwrap();
            assert_eq!(info.id, OpenLinkId::new(ID + 1).unwrap());
            assert_eq!(info.kind, Some(2));
            assert_eq!(info.name, "synthetic");
        } else {
            assert!(matches!(info, Err(ReadError::InvalidResponse)));
        }
    }
}

#![cfg(feature = "read-only")]
mod support;
use bson::doc;
use futures_util::{SinkExt, StreamExt};
use sculk_client::{
    read_only::{Cursor, Error, RoomId},
    session::{AccessToken, Grant, RequestError, Session},
    Config,
};
use sculk_protocol::{Command, LocoCodec, Packet};
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::bytes::BytesMut;
use tokio_util::codec::{Decoder, Encoder, Framed};
const ID: i64 = 9_007_199_254_740_993;
fn fixture(bytes: &[u8]) -> Packet {
    LocoCodec::default()
        .decode(&mut BytesMut::from(bytes))
        .unwrap()
        .unwrap()
}
fn packet(command: &str, body: bson::Document) -> Packet {
    Packet::from_document(1, Command::new(command).unwrap(), &body).unwrap()
}
async fn setup(replies: Vec<Packet>) -> (Session, tokio::task::JoinHandle<()>) {
    let (settings, acceptor) = support::tls_pair();
    let (a, b) = tokio::io::duplex(16384);
    let peer = tokio::spawn(async move {
        let mut framed = Framed::new(acceptor.accept(b).await.unwrap(), LocoCodec::default());
        for mut reply in replies {
            let request = framed.next().await.unwrap().unwrap();
            assert_eq!(request.command, reply.command);
            let expected = match request.command.as_str() {
                "LCHATLIST" => {
                    include_bytes!("../../../tests/fixtures/agent-lchatlist-request.bin").as_slice()
                }
                "CHATINFO" => {
                    include_bytes!("../../../tests/fixtures/agent-chatinfo-request.bin").as_slice()
                }
                "GETMEM" => {
                    include_bytes!("../../../tests/fixtures/agent-getmem-request.bin").as_slice()
                }
                _ => panic!("unexpected mutation command"),
            };
            let mut normalized = request.clone();
            normalized.id = 1;
            let mut bytes = BytesMut::new();
            LocoCodec::default().encode(normalized, &mut bytes).unwrap();
            assert_eq!(bytes.as_ref(), expected);
            reply.id = request.id;
            if framed.send(reply).await.is_err() {
                return;
            }
        }
        let _ = framed.next().await;
    });
    let (_tx, cancel) = watch::channel(false);
    let mut session = Session::establish(
        a,
        "localhost",
        &settings,
        Duration::from_secs(1),
        Config::default(),
        cancel.clone(),
    )
    .await
    .unwrap();
    // Explicit mock account provider: this test verifies read-only wire behavior only.
    session
        .authenticate_with(
            AccessToken::new(
                "synthetic".into(),
                42,
                tokio::time::Instant::now() + Duration::from_secs(60),
            )
            .unwrap(),
            Duration::from_secs(1),
            cancel,
            |_, _| async {
                Ok(Grant {
                    user_id: 42,
                    valid_for: Duration::from_secs(30),
                })
            },
        )
        .await
        .unwrap();
    (session, peer)
}
#[tokio::test]
async fn source_backed_room_page_metadata_and_members_preserve_large_ids() {
    let (mut session, peer) = setup(vec![
        fixture(include_bytes!(
            "../../../tests/fixtures/agent-lchatlist-response.bin"
        )),
        fixture(include_bytes!(
            "../../../tests/fixtures/agent-chatinfo-response.bin"
        )),
        fixture(include_bytes!(
            "../../../tests/fixtures/agent-getmem-response.bin"
        )),
    ])
    .await;
    let page = session.chat_rooms(Cursor::default()).await.unwrap();
    assert!(!page.eof);
    assert_eq!(page.rooms[0].id.get(), ID);
    assert_eq!(page.rooms[0].kind.as_deref(), Some("OM"));
    assert_eq!(page.next.unwrap().last_chat_id, ID);
    let info = session.room_info(RoomId::new(ID).unwrap()).await.unwrap();
    assert_eq!(info.id.get(), ID);
    assert_eq!(info.metadata.unwrap()[0].kind, 1);
    let members = session
        .room_members(RoomId::new(ID).unwrap())
        .await
        .unwrap();
    assert_eq!(members.members[0].user_id, ID);
    assert_eq!(
        members.members[0].nickname.as_deref(),
        Some("Synthetic member")
    );
    assert_eq!(members.token, 1);
    session.disconnect().await;
    peer.await.unwrap();
}
#[tokio::test]
async fn read_only_rejection_missing_fields_and_nonprogressing_cursor_fail() {
    for body in [
        doc! {"status":-203},
        doc! {"chatDatas":[],"eof":false},
        doc! {"chatDatas":[{"c":ID as f64}],"eof":true},
        doc! {"chatDatas":[],"eof":false,"lastTokenId":0_i64,"lastChatId":0_i64},
        doc! {"chatDatas":[],"eof":"true"},
    ] {
        let (mut session, peer) = setup(vec![packet("LCHATLIST", body)]).await;
        assert!(session.chat_rooms(Cursor::default()).await.is_err());
        session.disconnect().await;
        peer.await.unwrap();
    }
    assert!(RoomId::new(0).is_err());
}
#[tokio::test]
async fn wrong_room_and_invalid_member_data_are_rejected() {
    for reply in [
        packet("CHATINFO", doc! {"chatInfo":{"chatId":43_i64}}),
        packet(
            "GETMEM",
            doc! {"members":[{"userId":ID,"nickName":false}],"token":0},
        ),
    ] {
        let (mut session, peer) = setup(vec![reply.clone()]).await;
        let failed = if reply.command.as_str() == "CHATINFO" {
            session.room_info(RoomId::new(ID).unwrap()).await.is_err()
        } else {
            session
                .room_members(RoomId::new(ID).unwrap())
                .await
                .is_err()
        };
        assert!(failed);
        session.disconnect().await;
        peer.await.unwrap();
    }
}
#[tokio::test]
async fn closed_sessions_cannot_make_read_only_requests() {
    let (mut session, peer) = setup(vec![]).await;
    session.disconnect().await;
    assert!(matches!(
        session.chat_rooms(Cursor::default()).await,
        Err(Error::Session(RequestError::Unauthenticated))
    ));
    peer.await.unwrap();
}

#[tokio::test]
async fn duplicate_response_fields_and_oversized_arrays_fail_closed() {
    let original = packet("LCHATLIST", doc! {"chatDatas":[],"eof":true});
    let mut body = original.body().to_vec();
    let fields = body[4..body.len() - 1].to_vec();
    body.pop();
    body.extend_from_slice(&fields);
    body.push(0);
    let n = body.len() as i32;
    body[..4].copy_from_slice(&n.to_le_bytes());
    let duplicate =
        Packet::from_bytes(1, 0, Command::new("LCHATLIST").unwrap(), body.into()).unwrap();
    let oversized = packet(
        "LCHATLIST",
        doc! {"chatDatas":vec![bson::Bson::Document(doc!{"c":1_i64});4097],"eof":true},
    );
    for reply in [duplicate, oversized] {
        let (mut session, peer) = setup(vec![reply]).await;
        assert!(matches!(
            session.chat_rooms(Cursor::default()).await,
            Err(Error::InvalidResponse)
        ));
        session.disconnect().await;
        peer.await.unwrap();
    }
}

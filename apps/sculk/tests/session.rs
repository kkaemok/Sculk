//! The installed starter script through the real session adapter and a local peer.
#[path = "../../../crates/sculk-client/tests/support/mod.rs"]
mod tls_support;

use bson::doc;
use futures_util::{SinkExt, StreamExt};
use sculk_bot::{Bot, EventKind, RoomId, SessionSource};
use sculk_client::session::{AccessToken, Grant, Session};
use sculk_protocol::{Command, LocoCodec, Packet};
use sculk_script::{Compiler, Source};
use std::{sync::Arc, time::Duration};
use tokio::{sync::watch, time::Instant};
use tokio_util::codec::Framed;

#[tokio::test]
async fn starter_script_dispatches_over_an_authenticated_local_session() {
    let (settings, acceptor) = tls_support::tls_pair();
    let (client, peer) = tokio::io::duplex(8192);
    let (stop, cancel) = watch::channel(false);
    let (session, server) = tokio::join!(
        Session::establish(
            client,
            "localhost",
            &settings,
            Duration::from_secs(1),
            Default::default(),
            cancel.clone(),
        ),
        acceptor.accept(peer),
    );
    let mut session = session.unwrap();
    session
        .authenticate_with(
            AccessToken::new(
                "fixture".into(),
                42,
                Instant::now() + Duration::from_secs(20),
            )
            .unwrap(),
            Duration::from_secs(1),
            cancel.clone(),
            |_, _| async {
                Ok(Grant {
                    user_id: 42,
                    valid_for: Duration::from_secs(10),
                })
            },
        )
        .await
        .unwrap();
    let mut server = Framed::new(server.unwrap(), LocoCodec::default());
    let room_id = 9_007_199_254_740_993_i64;
    let observed = Arc::new(tokio::sync::Notify::new());
    let application = async {
        let source = SessionSource::select(session, RoomId::new(room_id).unwrap(), cancel.clone())
            .await
            .unwrap();
        let mut bot = Bot::new(Default::default()).unwrap();
        bot.outbound(Arc::new(
            source.text_outbound(Duration::from_millis(50)).unwrap(),
        ));
        let program = Compiler::default()
            .compile(&[Source {
                filename: "chat.sculk",
                text: include_str!("../../../examples/openchat-bot/scripts/chat.sculk"),
            }])
            .unwrap();
        let mut bot = program.install(bot).unwrap();
        // This marker follows the command in the same bounded FIFO worker.
        let notify = observed.clone();
        bot.on(EventKind::Unknown, move |_| {
            let notify = notify.clone();
            async move {
                notify.notify_one();
                Ok(())
            }
        })
        .unwrap();
        bot.run(source, cancel).await
    };
    let peer = async {
        let query = server.next().await.unwrap().unwrap();
        assert_eq!(query.command.as_str(), "CHATINFO");
        assert_eq!(query.document().unwrap(), doc! {"chatId":room_id});
        server
            .send(
                Packet::from_document(
                    query.id,
                    query.command,
                    &doc! {
                        "chatInfo": { "chatId": room_id }
                    },
                )
                .unwrap(),
            )
            .await
            .unwrap();
        server
            .send(
                Packet::from_document(
                    0,
                    Command::new("MSG").unwrap(),
                    &doc! {
                        "chatId": room_id,
                        "chatLog": { "logId": room_id + 1, "authorId": 11_i64, "type": 1,
                                     "sendAt": 10_i64, "message": "!greet \"Ada Lovelace\"" }
                    },
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let write = server.next().await.unwrap().unwrap();
        assert_eq!(write.command.as_str(), "WRITE");
        assert_eq!(
            write.document().unwrap(),
            doc! {
                "chatId": room_id, "msg": "Hello, Ada Lovelace!", "type": 1, "noSeen": false
            }
        );
        server
            .send(
                Packet::from_document(
                    write.id,
                    write.command,
                    &doc! {
                        "chatId": room_id, "logId": room_id + 2, "sendAt": 11_i64
                    },
                )
                .unwrap(),
            )
            .await
            .unwrap();
        server
            .send(Packet::from_document(0, Command::new("FIXTURE").unwrap(), &doc! {}).unwrap())
            .await
            .unwrap();
        observed.notified().await;
        stop.send_replace(true);
        assert!(server.next().await.is_none());
    };
    let (report, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(application, peer)
    })
    .await
    .unwrap();
    assert_eq!(report.commands, 1);
    assert_eq!(report.failures, 0);
    assert_eq!(report.shutdown, Ok(()));
}

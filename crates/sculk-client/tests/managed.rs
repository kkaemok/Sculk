#![cfg(feature = "session-recovery")]
mod support;
use bson::doc;
use futures_util::{SinkExt, StreamExt};
use sculk_client::{
    connect::{self, Stage},
    managed::{self, Budget, Room},
    messaging::Event,
    read_only::RoomId,
    recovery::{AttemptError, RetryPolicy},
    session::{AccessToken, AuthError, Grant, Session},
    synchronization::{CheckpointStore, Cursor, MemoryStore, Observation},
    Config,
};
use sculk_protocol::{Command, LocoCodec, Packet};
use std::{cell::Cell, time::Duration};
use tokio::{io::DuplexStream, sync::watch, time::Instant};
use tokio_rustls::server::TlsStream;
use tokio_util::codec::Framed;
const ID: i64 = 9_007_199_254_740_993;
type Peer = Framed<TlsStream<DuplexStream>, LocoCodec>;
fn packet(name: &str, body: bson::Document) -> Packet {
    Packet::from_document(0, Command::new(name).unwrap(), &body).unwrap()
}
fn history(id: i64) -> bson::Document {
    doc! {"chatId":ID,"logId":id,"authorId":42_i64,"sendAt":1700000000,"type":1,"message":"synthetic","prevId":ID}
}
fn policy() -> RetryPolicy {
    RetryPolicy {
        attempts: 3,
        initial: Duration::from_millis(1),
        maximum: Duration::from_millis(2),
    }
}
async fn setup(capacity: usize, lease: Duration) -> (Session, Peer) {
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
                ..Config::default()
            },
            cancel.clone()
        ),
        acceptor.accept(b)
    );
    let mut session = session.unwrap();
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
fn state(budget: Budget) -> (Room, MemoryStore) {
    let id = RoomId::new(ID).unwrap();
    let store = MemoryStore::new(id, Cursor::new(ID).unwrap());
    let room = Room::new(id, 8, budget, &store).unwrap();
    (room, store)
}
async fn reply(peer: &mut Peer, name: &str, body: bson::Document) {
    let request = peer.next().await.unwrap().unwrap();
    assert_eq!(request.command.as_str(), name);
    let mut response = packet(name, body);
    response.id = request.id;
    peer.send(response).await.unwrap();
}
#[tokio::test]
async fn subscribe_before_sync_buffers_live_duplicates_and_preserves_unknown_events() {
    let (session, mut peer) = setup(8, Duration::from_secs(30)).await;
    let mut session = Some(session);
    let (mut room, mut store) = state(Budget::default());
    let (_tx, cancel) = watch::channel(false);
    room.reconnect(policy(), Duration::from_secs(1), cancel.clone(), || {
        std::future::ready(Ok(session.take().unwrap()))
    })
    .await
    .unwrap();
    assert_eq!(room.generation(), 1);
    assert!(matches!(
        room.reconnect(policy(), Duration::from_secs(1), cancel.clone(), || async {
            panic!("duplicate active session")
        })
        .await,
        Err(managed::Error::AlreadyActive)
    ));
    let (page, ()) = tokio::join!(room.recover_page(2, cancel.clone()), async {
        reply(
            &mut peer,
            "CHATINFO",
            doc! {"chatInfo":{"chatId":ID,"lastLogId":ID+2}},
        )
        .await;
        let request = peer.next().await.unwrap().unwrap();
        assert_eq!(request.command.as_str(), "SYNCMSG");
        assert_eq!(
            request.document().unwrap(),
            doc! {"chatId":ID,"cur":ID,"cnt":2,"max":ID+2}
        );
        peer.send(packet("MSG", doc! {"chatId":ID,"chatLog":history(ID+2)}))
            .await
            .unwrap();
        let mut p = packet(
            "SYNCMSG",
            doc! {"isOK":true,"chatLogs":[history(ID+1),history(ID+2)]},
        );
        p.id = request.id;
        peer.send(p).await.unwrap();
        peer.send(packet("FUTURE", doc! {"opaque":true}))
            .await
            .unwrap();
    });
    let page = page.unwrap().unwrap();
    assert_eq!(room.cursor().get(), ID);
    room.tracker_mut().begin_page(&page).unwrap();
    for i in 0..page.messages().len() {
        room.tracker_mut()
            .acknowledge_history(&page, i, &mut store)
            .unwrap();
    }
    room.tracker_mut().finish_page().unwrap();
    let event = room.recv(cancel.clone()).await.unwrap();
    match event {
        Event::Message(m) => {
            assert_eq!(room.tracker().observe(&m).unwrap(), Observation::Duplicate)
        }
        _ => panic!("lost MSG"),
    }
    assert!(matches!(
        room.recv(cancel.clone()).await.unwrap(),
        Event::Unknown(_)
    ));
    let (ping, ()) = tokio::join!(
        room.heartbeat(cancel.clone()),
        reply(&mut peer, "PING", doc! {})
    );
    ping.unwrap();
    room.shutdown().await;
    assert!(peer.next().await.is_none());
}
#[tokio::test]
async fn reconnect_resumes_partial_processing_and_retries_only_transient_prelogin_failures() {
    let (session, mut peer) = setup(8, Duration::from_secs(30)).await;
    let mut session = Some(session);
    let (mut room, mut store) = state(Budget::default());
    let (_tx, cancel) = watch::channel(false);
    room.reconnect(policy(), Duration::from_secs(1), cancel.clone(), || {
        std::future::ready(Ok(session.take().unwrap()))
    })
    .await
    .unwrap();
    let (page, ()) = tokio::join!(
        room.synchronize(
            sculk_client::messaging::MessageId::new(ID + 2).unwrap(),
            2,
            cancel.clone()
        ),
        reply(
            &mut peer,
            "SYNCMSG",
            doc! {"isOK":false,"chatLogs":[history(ID+1),history(ID+2)]}
        )
    );
    let page = page.unwrap();
    room.tracker_mut().begin_page(&page).unwrap();
    room.tracker_mut()
        .acknowledge_history(&page, 0, &mut store)
        .unwrap();
    assert_eq!(store.load(RoomId::new(ID).unwrap()).unwrap().get(), ID + 1);
    drop(peer);
    assert!(room.recv(cancel.clone()).await.is_err());
    let (session2, mut peer2) = setup(8, Duration::from_secs(30)).await;
    let mut session2 = Some(session2);
    let calls = Cell::new(0);
    room.reconnect(policy(), Duration::from_secs(1), cancel.clone(), || {
        calls.set(calls.get() + 1);
        std::future::ready(if calls.get() == 1 {
            Err(connect::Error::DialIo(
                Stage::Booking,
                std::io::ErrorKind::ConnectionReset,
            ))
        } else {
            Ok(session2.take().unwrap())
        })
    })
    .await
    .unwrap();
    assert_eq!(calls.get(), 2);
    assert_eq!(room.generation(), 2);
    assert_eq!(room.cursor().get(), ID + 1);
    let (page, ()) = tokio::join!(
        room.synchronize(
            sculk_client::messaging::MessageId::new(ID + 2).unwrap(),
            2,
            cancel
        ),
        async {
            let request = peer2.next().await.unwrap().unwrap();
            assert_eq!(request.document().unwrap().get_i64("cur").unwrap(), ID + 1);
            let mut p = packet("SYNCMSG", doc! {"isOK":true,"chatLogs":[history(ID+2)]});
            p.id = request.id;
            peer2.send(p).await.unwrap();
        }
    );
    let p = page.unwrap();
    room.tracker_mut().begin_page(&p).unwrap();
    room.tracker_mut()
        .acknowledge_history(&p, 0, &mut store)
        .unwrap();
    room.tracker_mut().finish_page().unwrap();
    room.shutdown().await;
}
#[tokio::test]
async fn rejection_verification_policy_and_ambiguous_login_failures_never_auto_retry() {
    for error in [
        connect::Error::Auth(AuthError::Rejected(-950)),
        connect::Error::Auth(AuthError::VerificationRequired),
        connect::Error::Transport(Stage::Login, sculk_client::Error::Timeout),
        connect::Error::Timeout,
        connect::Error::DialIo(Stage::Booking, std::io::ErrorKind::PermissionDenied),
    ] {
        let (mut room, _) = state(Budget::default());
        let (_tx, cancel) = watch::channel(false);
        let calls = Cell::new(0);
        let mut error = Some(error);
        assert!(matches!(
            room.reconnect(policy(), Duration::from_secs(1), cancel.clone(), || {
                calls.set(calls.get() + 1);
                std::future::ready(Err(error.take().unwrap()))
            })
            .await,
            Err(managed::Error::Connect(AttemptError::Permanent(_)))
        ));
        assert_eq!(calls.get(), 1);
        assert!(matches!(
            room.reconnect(policy(), Duration::from_secs(1), cancel, || async {
                panic!("terminal rejection must latch")
            })
            .await,
            Err(managed::Error::Intervention)
        ));
    }
}
#[tokio::test]
async fn cancelled_attempt_and_active_sync_clean_up_without_checkpointing_or_retry() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    struct Guard(Arc<AtomicUsize>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(AtomicUsize::new(0));
    let (mut room, _) = state(Budget::default());
    let (tx, cancel) = watch::channel(false);
    let (called, wait) = tokio::sync::oneshot::channel();
    let mut called = Some(called);
    let (result, ()) = tokio::join!(
        room.reconnect(policy(), Duration::from_secs(10), cancel, || {
            let guard = Guard(dropped.clone());
            called.take().unwrap().send(()).unwrap();
            async move {
                let _guard = guard;
                std::future::pending().await
            }
        }),
        async {
            wait.await.unwrap();
            tx.send_replace(true);
        }
    );
    assert!(matches!(
        result,
        Err(managed::Error::Connect(AttemptError::Cancelled))
    ));
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    let (session, mut peer) = setup(8, Duration::from_secs(30)).await;
    let mut session = Some(session);
    let (tx, cancel) = watch::channel(false);
    room.reconnect(policy(), Duration::from_secs(1), cancel.clone(), || {
        std::future::ready(Ok(session.take().unwrap()))
    })
    .await
    .unwrap();
    let (result, ()) = tokio::join!(
        room.synchronize(
            sculk_client::messaging::MessageId::new(ID + 2).unwrap(),
            2,
            cancel
        ),
        async {
            let request = peer.next().await.unwrap().unwrap();
            assert_eq!(request.command.as_str(), "SYNCMSG");
            tx.send_replace(true);
        }
    );
    assert!(matches!(
        result,
        Err(managed::Error::Sync(
            sculk_client::synchronization::Error::Cancelled
        ))
    ));
    assert_eq!(room.cursor().get(), ID);
    tokio::time::timeout(Duration::from_secs(1), room.shutdown())
        .await
        .unwrap();
    assert!(peer.next().await.is_none());
}
#[tokio::test]
async fn bounded_budget_survives_cancelled_requests_and_explicit_lag_is_not_hidden() {
    let (session, mut peer) = setup(2, Duration::from_secs(30)).await;
    let mut session = Some(session);
    let (mut room, _) = state(Budget {
        pages: 1,
        messages: 2,
    });
    let (_tx, cancel) = watch::channel(false);
    room.reconnect(policy(), Duration::from_secs(1), cancel.clone(), || {
        std::future::ready(Ok(session.take().unwrap()))
    })
    .await
    .unwrap();
    let (page, ()) = tokio::join!(
        room.synchronize(
            sculk_client::messaging::MessageId::new(ID + 2).unwrap(),
            2,
            cancel.clone()
        ),
        async {
            for _ in 0..8 {
                peer.send(packet("MSG", doc! {"chatId":ID,"chatLog":history(ID+1)}))
                    .await
                    .unwrap();
            }
            reply(
                &mut peer,
                "SYNCMSG",
                doc! {"isOK":true,"chatLogs":[history(ID+1)]},
            )
            .await;
        }
    );
    page.unwrap();
    assert!(matches!(
        room.recv(cancel.clone()).await,
        Err(managed::Error::Receive(
            sculk_client::messaging::ReceiveError::Lagged(6)
        ))
    ));
    assert!(matches!(
        room.synchronize(
            sculk_client::messaging::MessageId::new(ID + 2).unwrap(),
            1,
            cancel
        )
        .await,
        Err(managed::Error::Sync(
            sculk_client::synchronization::Error::Limit
        ))
    ));
    room.shutdown().await;
}
#[tokio::test(start_paused = true)]
async fn expiry_requires_intervention_and_does_not_reauthenticate() {
    let (session, _peer) = setup(8, Duration::from_secs(1)).await;
    let mut session = Some(session);
    let (mut room, _) = state(Budget::default());
    let (_tx, cancel) = watch::channel(false);
    room.reconnect(policy(), Duration::from_secs(1), cancel.clone(), || {
        std::future::ready(Ok(session.take().unwrap()))
    })
    .await
    .unwrap();
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(room.recv(cancel.clone()).await.is_err());
    assert!(matches!(
        room.reconnect(policy(), Duration::from_secs(1), cancel, || async {
            panic!("expired credentials require intervention")
        })
        .await,
        Err(managed::Error::Intervention)
    ));
    room.shutdown().await;
}

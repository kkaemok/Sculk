#[path = "../../../../crates/sculk-client/tests/support/mod.rs"]
mod tls_support;

use super::*;
use bson::doc;
use futures_util::{SinkExt, StreamExt};
use sculk_bot::{EventKind, RoomId};
use sculk_client::{
    checkpoint::FileStore,
    session::{parse_loginlist_reply, AccessToken},
    synchronization::CheckpointStore,
};
use sculk_protocol::{Command as WireCommand, LocoCodec, Packet};
use sculk_script::{Compiler, Limits, Source};
use tokio::{
    io::{AsyncRead, AsyncWrite, DuplexStream},
    sync::{oneshot, Notify},
};
use tokio_rustls::server::TlsStream;
use tokio_util::codec::Framed;

const ROOM: i64 = 9_007_199_254_740_993;
type Peer = Framed<TlsStream<DuplexStream>, LocoCodec>;

fn packet(id: u32, method: &str, body: bson::Document) -> Packet {
    Packet::from_document(id, WireCommand::new(method).unwrap(), &body).unwrap()
}

async fn connect_pair(cancel: watch::Receiver<bool>, lease: Duration) -> (Session, Peer) {
    let (settings, acceptor) = tls_support::tls_pair();
    let (left, right) = tokio::io::duplex(8192);
    let (guest, server) = tokio::join!(
        Session::establish(
            left,
            "localhost",
            &settings,
            Duration::from_secs(2),
            sculk_client::Config {
                request_timeout: Duration::from_millis(250),
                ..Default::default()
            },
            cancel.clone()
        ),
        acceptor.accept(right),
    );
    let mut guest = guest.unwrap();
    let mut peer = Framed::new(server.unwrap(), LocoCodec::default());
    let (authorized, ()) = tokio::join!(
        guest.authenticate_with(
            AccessToken::new("local-fixture".into(), 42, Instant::now() + lease).unwrap(),
            Duration::from_secs(2),
            cancel,
            |client, _| async move {
                let reply = client.request(packet(0, "LOGINLIST", doc! {})).await?;
                parse_loginlist_reply(&reply, 42, lease)
            }
        ),
        async {
            let login = receive(&mut peer, "LOGINLIST").await;
            peer.send(packet(login.id, "LOGINLIST", doc! {"userId":42_i64}))
                .await
                .unwrap();
        },
    );
    authorized.unwrap();
    (guest, peer)
}

async fn receive<T: AsyncRead + AsyncWrite + Unpin>(
    peer: &mut Framed<T, LocoCodec>,
    method: &str,
) -> Packet {
    let request = peer.next().await.unwrap().unwrap();
    assert_eq!(request.command.as_str(), method);
    request
}

async fn watermark<T: AsyncRead + AsyncWrite + Unpin>(
    peer: &mut Framed<T, LocoCodec>,
    current: i64,
) {
    let query = receive(peer, "CHATINFO").await;
    assert_eq!(query.document().unwrap(), doc! {"chatId": ROOM});
    peer.send(packet(
        query.id,
        "CHATINFO",
        doc! {
            "chatInfo": {"chatId": ROOM, "lastLogId": current}
        },
    ))
    .await
    .unwrap();
}

fn log(id: i64, predecessor: i64) -> bson::Document {
    doc! {"chatId":ROOM,"logId":id,"prevId":predecessor,"authorId":2_i64,"type":1_i32,"sendAt":10_i64,"message":"!ping"}
}

async fn push<T: AsyncRead + AsyncWrite + Unpin>(
    peer: &mut Framed<T, LocoCodec>,
    id: i64,
    predecessor: i64,
) {
    peer.send(packet(
        0,
        "MSG",
        doc! {"chatId":ROOM,"chatLog":log(id, predecessor)},
    ))
    .await
    .unwrap();
}

async fn history(peer: &mut Peer, from: i64, through: i64) {
    let query = receive(peer, "SYNCMSG").await;
    assert_eq!(
        query.document().unwrap(),
        doc! {"chatId":ROOM,"cur":from,"cnt":i32::from(PAGE_SIZE),"max":through}
    );
    peer.send(packet(
        query.id,
        "SYNCMSG",
        doc! {
            "isOK":true,"chatLogs":[log(through, from)]
        },
    ))
    .await
    .unwrap();
}

async fn wait_boundary(path: &Path, id: i64) {
    let path = path.join("checkpoint");
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if std::fs::read_to_string(&path).ok().is_some_and(|text| {
                text.lines()
                    .nth(2)
                    .and_then(|value| value.parse::<i64>().ok())
                    == Some(id)
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

fn program(milliseconds: u64) -> Bot {
    Compiler::new(Limits {
        handler_time: Duration::from_millis(milliseconds),
        ..Limits::default()
    })
    .compile(&[Source {
        filename: "live.sc",
        text: "command ping:\n    trigger:\n        reply \"Pong!\"\n",
    }])
    .unwrap()
    .install(Bot::new(Default::default()).unwrap())
    .unwrap()
}

#[tokio::test]
async fn recovers_via_the_real_runner_reconciles_history_and_suppresses_duplicates_and_echoes() {
    let (stop, cancel) = watch::channel(false);
    let (first, mut lost) = connect_pair(cancel.clone(), Duration::from_secs(20)).await;
    let (peer_tx, peer_rx) = oneshot::channel();
    let mut peer_tx = Some(peer_tx);
    let connector_cancel = cancel.clone();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state");
    let id = RoomId::new(ROOM).unwrap();
    let state = State::open(id, &path, true, None).unwrap();
    let room = Room::new(id, 32, managed::Budget::default(), &state.staged).unwrap();
    let p = Progress::default();
    let ready = Arc::new(Notify::new());
    let notify = ready.clone();
    let mut bot = program(1000);
    bot.on(EventKind::Connection, move |ctx| {
        let notify = notify.clone();
        async move {
            if matches!(ctx.event(), Event::Connection(Connection::State { .. })) {
                notify.notify_one();
            }
            Ok(())
        }
    })
    .unwrap();
    let mut initial = Some(first);
    let runner = Runner::new(
        bot,
        room,
        state,
        &p,
        cancel,
        None,
        Some(Duration::from_secs(1)),
    );
    let (result, ()) = tokio::time::timeout(Duration::from_secs(6), async {
        tokio::join!(
            runner.run(|| {
                let first = initial.take();
                let sender = if first.is_none() {
                    peer_tx.take()
                } else {
                    None
                };
                let cancel = connector_cancel.clone();
                async move {
                    if let Some(session) = first {
                        return Ok(session);
                    }
                    let (session, peer) = connect_pair(cancel, Duration::from_secs(20)).await;
                    sender.expect("exactly one reconnect").send(peer).unwrap();
                    Ok(session)
                }
            }),
            async {
                watermark(&mut lost, 0).await;
                ready.notified().await;
                wait_boundary(&path, 0).await;
                drop(lost);
                let mut peer = peer_rx.await.unwrap();
                watermark(&mut peer, 101).await;
                history(&mut peer, 0, 101).await;
                push(&mut peer, 101, 0).await;
                push(&mut peer, 102, 101).await;
                let write = receive(&mut peer, "WRITE").await;
                assert_eq!(
                    write.document().unwrap(),
                    doc! {
                        "chatId":ROOM,"msg":"Pong!","type":1_i32,"noSeen":false
                    }
                );
                peer.send(packet(
                    write.id,
                    "WRITE",
                    doc! {
                        "chatId":ROOM,"logId":103_i64,"sendAt":10_i64
                    },
                ))
                .await
                .unwrap();
                push(&mut peer, 103, 102).await;
                wait_boundary(&path, 103).await;
                stop.send_replace(true);
                assert!(peer.next().await.is_none());
            }
        )
    })
    .await
    .unwrap();
    result.unwrap();
    assert!(initial.is_none() && peer_tx.is_none());
    let restored = FileStore::open(&path, id).unwrap();
    assert_eq!(restored.load(id).unwrap().get(), 103);
    assert!(p.milestones().write_acknowledged && p.milestones().shutdown);
}

#[tokio::test]
async fn disconnect_and_handler_deadline_leave_uncertain_writes_fenced_across_restart() {
    for disconnect in [true, false] {
        let (_stop, cancel) = watch::channel(false);
        let (session, mut peer) = connect_pair(cancel.clone(), Duration::from_secs(15)).await;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        let id = RoomId::new(ROOM).unwrap();
        let state = State::open(id, &path, true, None).unwrap();
        let room = Room::new(id, 32, managed::Budget::default(), &state.staged).unwrap();
        let p = Progress::default();
        let mut selected = Some(session);
        let runner = Runner::new(
            program(120),
            room,
            state,
            &p,
            cancel.clone(),
            None,
            Some(Duration::from_secs(1)),
        );
        let (result, ()) = tokio::time::timeout(Duration::from_secs(4), async {
            tokio::join!(
                runner.run(|| std::future::ready(Ok(selected
                    .take()
                    .expect("must not reconnect after WRITE")))),
                async {
                    watermark(&mut peer, 200).await;
                    push(&mut peer, 201, 200).await;
                    receive(&mut peer, "WRITE").await;
                    if !disconnect {
                        assert!(peer.next().await.is_none());
                    }
                    drop(peer);
                }
            )
        })
        .await
        .unwrap();
        assert!(matches!(
            result.unwrap_err().downcast_ref(),
            Some(RuntimeError::UnknownDelivery)
        ));
        assert!(!p.milestones().write_acknowledged);
        let state = State::open(id, &path, true, None).unwrap();
        assert_eq!(state.committed.get(), 200);
        assert_eq!(state.attempted.get(), 201);
        let room = Room::new(id, 32, managed::Budget::default(), &state.staged).unwrap();
        let (session, mut peer) = connect_pair(cancel.clone(), Duration::from_secs(15)).await;
        let mut selected = Some(session);
        let p = Progress::default();
        let runner = Runner::new(
            program(500),
            room,
            state,
            &p,
            cancel,
            None,
            Some(Duration::from_secs(1)),
        );
        let (result, ()) = tokio::time::timeout(Duration::from_secs(4), async {
            tokio::join!(
                runner.run(|| std::future::ready(Ok(selected
                    .take()
                    .expect("only one recovery attempt")))),
                async {
                    watermark(&mut peer, 200).await;
                    assert!(peer.next().await.is_none());
                }
            )
        })
        .await
        .unwrap();
        assert!(matches!(
            result.unwrap_err().downcast_ref(),
            Some(RuntimeError::UnreconciledWrite)
        ));
    }
}

#[tokio::test]
async fn credential_expiration_stops_instead_of_running_a_second_login() {
    let (_tx, cancel) = watch::channel(false);
    let (session, mut peer) = connect_pair(cancel.clone(), Duration::from_millis(300)).await;
    let id = RoomId::new(ROOM).unwrap();
    let state = State::probe(id);
    let room = Room::new(id, 32, managed::Budget::default(), &state.staged).unwrap();
    let progress = Progress::default();
    let mut selected = Some(session);
    let runner = Runner::new(program(100), room, state, &progress, cancel, None, None);
    let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(
            runner.run(|| std::future::ready(Ok(selected
                .take()
                .expect("one attempt before expiry")))),
            async {
                watermark(&mut peer, 0).await;
                let _ = peer.next().await;
            },
        )
    })
    .await
    .unwrap();
    assert!(matches!(
        result.unwrap_err().downcast_ref(),
        Some(RuntimeError::Intervention)
    ));
}

#[tokio::test]
async fn selected_command_runs_after_the_full_tls_connector_and_one_room_subscription() {
    use sculk_client::{
        connect::{BookingSchema, DeviceProfile, Endpoint, Options, Protection},
        dial::DirectTcp,
    };
    use tokio::net::TcpListener;

    let (settings, acceptor) = tls_support::tls_pair();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let endpoint = Endpoint::new("localhost", port).unwrap();
    let route = DirectTcp::new(vec![endpoint.clone()], Duration::from_secs(1)).unwrap();
    let options = Options {
        booking: endpoint,
        booking_schema: BookingSchema::AgentMessenger,
        checkin: Protection::Tls,
        chat: Protection::Tls,
        tls: settings,
        transport: Default::default(),
        stage_timeout: Duration::from_secs(1),
        total_timeout: Duration::from_secs(3),
        session_lease: Duration::from_secs(10),
    };
    let profile = DeviceProfile {
        os: "android".into(),
        app_version: "fixture".into(),
        protocol_version: "1".into(),
        mccmnc: "99999".into(),
        language: "ko".into(),
        country_iso: "KR".into(),
        use_sub: true,
    };
    let (_tx, cancel) = watch::channel(false);
    let room = RoomId::new(ROOM).unwrap();
    let state = State::probe(room);
    let manager = Room::new(room, 32, managed::Budget::default(), &state.staged).unwrap();
    let progress = Progress::default();
    let runner = Runner::new(
        program(1000),
        manager,
        state,
        &progress,
        cancel.clone(),
        Some("!ping"),
        Some(Duration::from_secs(1)),
    );
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            runner.run(|| async {
                let token = AccessToken::new(
                    "test-only".into(),
                    42,
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
                options
                    .connect(
                        &profile,
                        token,
                        "local-device",
                        cancel.clone(),
                        |stage, endpoint| {
                            progress.protocol_enter(stage);
                            route.dial(endpoint, cancel.clone())
                        },
                    )
                    .await
                    .inspect(|_| progress.completed(Stage::Loginlist))
            }),
            async {
                for (method, reply) in [
                    (
                        "GETCONF",
                        doc! {"hosts":["localhost"],"wifi":{"ports":[i32::from(port)]}},
                    ),
                    ("CHECKIN", doc! {"host":"localhost","port":i32::from(port)}),
                ] {
                    let (tcp, _) = listener.accept().await.unwrap();
                    let mut peer =
                        Framed::new(acceptor.accept(tcp).await.unwrap(), LocoCodec::default());
                    let request = receive(&mut peer, method).await;
                    peer.send(packet(request.id, method, reply)).await.unwrap();
                }
                let (tcp, _) = listener.accept().await.unwrap();
                let mut peer =
                    Framed::new(acceptor.accept(tcp).await.unwrap(), LocoCodec::default());
                let auth = receive(&mut peer, "LOGINLIST").await;
                peer.send(packet(auth.id, "LOGINLIST", doc! {"userId":42_i64}))
                    .await
                    .unwrap();
                watermark(&mut peer, 42).await;
                let mut nonmatch = log(45, 42);
                nonmatch.insert("message", "!unknown");
                peer.send(packet(0, "MSG", doc! {"chatId": ROOM, "chatLog": nonmatch}))
                    .await
                    .unwrap();
                push(&mut peer, 70, 45).await;
                let write = receive(&mut peer, "WRITE").await;
                assert_eq!(write.document().unwrap().get_str("msg").unwrap(), "Pong!");
                peer.send(packet(
                    write.id,
                    "WRITE",
                    doc! {"chatId":ROOM,"logId":90_i64,"sendAt":10_i64},
                ))
                .await
                .unwrap();
                assert!(peer.next().await.is_none());
            }
        )
    })
    .await
    .unwrap();
    result.unwrap();
    let m = progress.milestones();
    assert!(m.getconf && m.checkin && m.loginlist && m.selected_room && m.synchronized);
    assert!(m.controlled_message && m.bot_command_dispatched && m.write_acknowledged && m.shutdown);
}

#[tokio::test]
async fn cancelling_a_command_probe_cannot_report_success() {
    let (stop, cancel) = watch::channel(false);
    let (session, mut peer) = connect_pair(cancel.clone(), Duration::from_secs(15)).await;
    let id = RoomId::new(ROOM).unwrap();
    let state = State::probe(id);
    let room = Room::new(id, 32, managed::Budget::default(), &state.staged).unwrap();
    let p = Progress::default();
    let mut selected = Some(session);
    let runner = Runner::new(
        program(250),
        room,
        state,
        &p,
        cancel,
        Some("!ping"),
        Some(Duration::from_secs(1)),
    );
    let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(
            runner.run(|| std::future::ready(Ok(selected.take().unwrap()))),
            async {
                watermark(&mut peer, 42).await;
                while !p.milestones().synchronized {
                    tokio::task::yield_now().await;
                }
                stop.send_replace(true);
                assert!(peer.next().await.is_none());
            },
        )
    })
    .await
    .unwrap();
    assert!(matches!(
        result.unwrap_err().downcast_ref(),
        Some(RuntimeError::ValidationCancelled)
    ));
    assert!(!p.milestones().bot_command_dispatched && !p.milestones().write_acknowledged);
    assert!(p.milestones().shutdown);
}

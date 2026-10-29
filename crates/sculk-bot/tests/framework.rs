//! All message/connection inputs here are synthetic; no network access.
use sculk_bot::*;
use sculk_protocol::{Command as WireCommand, Packet};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{watch, Notify};

fn message(room: i64, sender: i64, id: i64, text: &str) -> Event {
    let p=Packet::from_document(0,WireCommand::new("MSG").unwrap(),&bson::doc!{"chatId":room,"chatLog":{"logId":id,"authorId":sender,"type":1,"sendAt":1700000000_i64,"message":text}}).unwrap();
    Event::Message(Message::from_packet(p).unwrap())
}
async fn run(bot: Bot, events: Vec<Event>) -> RunReport {
    let (tx, source) = channel(events.len().max(1)).unwrap();
    for event in events {
        tx.send(event).await.unwrap();
    }
    drop(tx);
    let (_tx, cancel) = watch::channel(false);
    tokio::time::timeout(Duration::from_secs(2), bot.run(source, cancel))
        .await
        .unwrap()
}
#[test]
fn bounded_parser_handles_unicode_quotes_escapes_and_exact_prefix() {
    let text = "!!PiNg one 'two words' \"한글 😀\" ab\" cd\" e\\ f '' \"a\\\"b\"";
    let p = parse(text, "!!").unwrap().unwrap();
    assert_eq!(p.name(), "PiNg");
    assert_eq!(
        p.arguments().collect::<Vec<_>>(),
        ["one", "two words", "한글 😀", "ab cd", "e f", "", "a\"b"]
    );
    assert!(parse(" !!ping", "!!").unwrap().is_none());
    assert!(parse("!ping", "!!").unwrap().is_none());
    assert!(parse("!! ", "!!").unwrap().is_none());
    assert!(matches!(parse("!p 'broken", "!"), Err(ParseError::Quoting)));
    assert!(matches!(parse("!p end\\", "!"), Err(ParseError::Quoting)));
    assert!(matches!(parse("!p.bad", "!"), Err(ParseError::Name)));
    assert!(matches!(
        parse(&format!("!p {}", "a ".repeat(65)), "!"),
        Err(ParseError::Limit)
    ));
    assert!(matches!(
        parse(&format!("!p {}", "a".repeat(16384)), "!"),
        Err(ParseError::Limit)
    ));
    assert!(!format!("{p:?}").contains("PiNg"));
    let ordinary = "!p borrowed";
    let parsed = parse(ordinary, "!").unwrap().unwrap();
    assert_eq!(
        parsed.arguments().next().unwrap().as_ptr(),
        ordinary[3..].as_ptr()
    );
}
#[tokio::test]
async fn aliases_case_policy_prefix_and_context() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let captured = calls.clone();
    let mut bot = Bot::new(Config {
        prefix: "::".into(),
        case_sensitive: false,
        ..Config::default()
    })
    .unwrap();
    bot.register(Command::new("Ping").alias("P"), move |ctx| {
        let captured = captured.clone();
        async move {
            assert!(!ctx.is_cancelled());
            assert_eq!(ctx.room().unwrap().get(), i64::MAX);
            assert_eq!(ctx.sender().unwrap().get(), i64::MAX - 1);
            captured.lock().unwrap().push((
                ctx.command_name().unwrap().to_owned(),
                ctx.arguments().map(str::to_owned).collect::<Vec<_>>(),
            ));
            Ok(())
        }
    })
    .unwrap();
    let report = run(
        bot,
        vec![
            message(i64::MAX, i64::MAX - 1, i64::MAX, "::p 'two words'"),
            message(1, 2, 2, "!ping"),
            message(i64::MAX, i64::MAX - 1, 3, "::PING"),
            message(1, 2, 4, " ::ping"),
        ],
    )
    .await;
    assert_eq!(report.commands, 2);
    assert_eq!(calls.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn case_sensitive_unknown_command_is_reported() {
    let errors = Arc::new(Mutex::new(Vec::new()));
    let capture = errors.clone();
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.command("ping", |_| async { Ok(()) }).unwrap();
    bot.on_error(move |ctx| {
        let capture = capture.clone();
        async move {
            capture.lock().unwrap().push(ctx.failure().unwrap());
            Ok(())
        }
    })
    .unwrap();
    let report = run(
        bot,
        vec![
            message(1, 2, 1, "!PING"),
            message(1, 2, 2, "!missing"),
            message(1, 2, 3, "!ping"),
        ],
    )
    .await;
    assert_eq!(report.commands, 1);
    assert_eq!(report.failures, 2);
    assert_eq!(
        *errors.lock().unwrap(),
        [Failure::UnknownCommand, Failure::UnknownCommand]
    );
}
#[test]
fn duplicate_registration_is_atomic_and_permissions_must_exist() {
    let mut bot = Bot::new(Config {
        case_sensitive: false,
        ..Config::default()
    })
    .unwrap();
    bot.register(Command::new("ping").alias("p"), |_| async { Ok(()) })
        .unwrap();
    assert_eq!(
        bot.register(Command::new("new").alias("P"), |_| async { Ok(()) }),
        Err(RegistrationError::Duplicate)
    );
    bot.command("new", |_| async { Ok(()) }).unwrap();
    assert_eq!(
        bot.register(Command::new("same").alias("SAME"), |_| async { Ok(()) }),
        Err(RegistrationError::Duplicate)
    );
    let id = PermissionId::new("bot.admin").unwrap();
    assert_eq!(
        bot.register(Command::new("admin").require(id.clone()), |_| async {
            Ok(())
        }),
        Err(RegistrationError::UnregisteredPermission)
    );
    bot.permission(id.clone(), false).unwrap();
    assert_eq!(bot.permission(id, true), Err(RegistrationError::Duplicate));
    assert_eq!(
        bot.command("bad name", |_| async { Ok(()) }),
        Err(RegistrationError::Invalid)
    );
}
#[tokio::test]
async fn permissions_default_grants_denials_and_room_scope() {
    let id = PermissionId::new("command.admin").unwrap();
    let unknown = PermissionId::new("unregistered").unwrap();
    let mut rules = MemoryPermissions::default();
    let sender = SenderId::new(2).unwrap();
    rules
        .set(
            id.clone(),
            sender,
            Some(RoomId::new(1).unwrap()),
            Decision::Grant,
        )
        .unwrap();
    rules
        .set(id.clone(), SenderId::new(3).unwrap(), None, Decision::Grant)
        .unwrap();
    rules
        .set(
            id.clone(),
            SenderId::new(3).unwrap(),
            Some(RoomId::new(1).unwrap()),
            Decision::Deny,
        )
        .unwrap();
    rules
        .set(id.clone(), SenderId::new(4).unwrap(), None, Decision::Deny)
        .unwrap();
    rules
        .set(
            id.clone(),
            SenderId::new(4).unwrap(),
            Some(RoomId::new(1).unwrap()),
            Decision::Grant,
        )
        .unwrap();
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.permission_store(Arc::new(rules));
    bot.permission(id.clone(), false).unwrap();
    bot.register(Command::new("admin").require(id.clone()), move |ctx| {
        let id = id.clone();
        let unknown = unknown.clone();
        async move {
            assert!(ctx.has_permission(&id));
            assert!(!ctx.has_permission(&unknown));
            Ok(())
        }
    })
    .unwrap();
    let report = run(
        bot,
        vec![
            message(1, 2, 1, "!admin"),
            message(2, 2, 2, "!admin"),
            message(1, 3, 3, "!admin"),
            message(2, 3, 4, "!admin"),
            message(1, 4, 5, "!admin"),
        ],
    )
    .await;
    assert_eq!(report.commands, 2);
    assert_eq!(report.failures, 3);
    assert_eq!(report.first_failure, Some(Failure::PermissionDenied));
    let default = PermissionId::new("open").unwrap();
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.permission(default.clone(), true).unwrap();
    bot.register(Command::new("open").require(default), |_| async { Ok(()) })
        .unwrap();
    assert_eq!(run(bot, vec![message(1, 9, 6, "!open")]).await.commands, 1);
}
#[tokio::test]
async fn fifo_registration_order_duplicates_and_room_separation() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut bot = Bot::new(Config::default()).unwrap();
    for label in ["first", "second"] {
        let capture = order.clone();
        bot.on(EventKind::Message, move |ctx| {
            let capture = capture.clone();
            async move {
                tokio::task::yield_now().await;
                capture.lock().unwrap().push((
                    ctx.room().unwrap().get(),
                    ctx.message().unwrap().id.get(),
                    label,
                ));
                Ok(())
            }
        })
        .unwrap();
    }
    let capture = order.clone();
    bot.command("p", move |ctx| {
        let capture = capture.clone();
        async move {
            capture.lock().unwrap().push((
                ctx.room().unwrap().get(),
                ctx.message().unwrap().id.get(),
                "command",
            ));
            Ok(())
        }
    })
    .unwrap();
    let report = run(
        bot,
        vec![
            message(1, 2, 20, "!p"),
            message(1, 2, 10, "!p"),
            message(1, 2, 20, "!p"),
            message(2, 2, 20, "!p"),
        ],
    )
    .await;
    assert_eq!(report.duplicates, 1);
    assert_eq!(report.completed, 3);
    assert_eq!(report.commands, 3);
    let expected = [(1, 20), (1, 10), (2, 20)]
        .into_iter()
        .flat_map(|(r, id)| ["first", "second", "command"].map(|l| (r, id, l)))
        .collect::<Vec<_>>();
    assert_eq!(*order.lock().unwrap(), expected);
    assert_eq!(report.shutdown, Ok(()));
}
#[tokio::test]
async fn concurrent_producers_still_serialize_handlers() {
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let captured = active.clone();
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.on(EventKind::Message, move |_| {
        let captured = captured.clone();
        async move {
            assert_eq!(captured.fetch_add(1, Ordering::SeqCst), 0);
            tokio::task::yield_now().await;
            assert_eq!(captured.fetch_sub(1, Ordering::SeqCst), 1);
            Ok(())
        }
    })
    .unwrap();
    let (tx, source) = channel(8).unwrap();
    let tx2 = tx.clone();
    let (_cancel, cancel) = watch::channel(false);
    let (a, b, report) = tokio::join!(
        async move {
            for id in 1..=4 {
                tx.send(message(1, 2, id, "ordinary")).await.unwrap();
            }
        },
        async move {
            for id in 5..=8 {
                tx2.send(message(2, 2, id, "ordinary")).await.unwrap();
            }
        },
        bot.run(source, cancel)
    );
    let _ = (a, b);
    assert_eq!(report.completed, 8);
    assert_eq!(report.stop, StopReason::Exhausted);
    assert_eq!(active.load(Ordering::SeqCst), 0);
}
struct Guard(Arc<AtomicBool>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[tokio::test]
async fn slow_handler_overflow_is_explicit_and_drops_pending_future() {
    let dropped = Arc::new(AtomicBool::new(false));
    let captured = dropped.clone();
    let entered = Arc::new(Notify::new());
    let capture = entered.clone();
    let mut bot = Bot::new(Config {
        event_capacity: 1,
        ..Config::default()
    })
    .unwrap();
    bot.command("p", move |_| {
        let guard = Guard(captured.clone());
        let capture = capture.clone();
        async move {
            let _guard = guard;
            capture.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    })
    .unwrap();
    let (tx, source) = channel(3).unwrap();
    let (_tx, cancel) = watch::channel(false);
    let (report, _) = tokio::join!(bot.run(source, cancel), async move {
        tx.send(message(1, 2, 1, "!p")).await.unwrap();
        entered.notified().await;
        tx.send(message(1, 2, 2, "!p")).await.unwrap();
        tx.send(message(1, 2, 3, "!p")).await.unwrap();
    });
    assert_eq!(report.stop, StopReason::QueueOverflow);
    assert_eq!(report.received, 3);
    assert_eq!(report.admitted, 2);
    assert_eq!(report.completed, 0);
    assert_eq!(report.commands, 1);
    assert!(dropped.load(Ordering::SeqCst));
}
#[tokio::test]
async fn cancellation_remains_effective_after_eof_and_cleans_up() {
    let entered = Arc::new(Notify::new());
    let capture = entered.clone();
    let dropped = Arc::new(AtomicBool::new(false));
    let captured = dropped.clone();
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.command("p", move |ctx| {
        let capture = capture.clone();
        let guard = Guard(captured.clone());
        async move {
            let _guard = guard;
            capture.notify_one();
            ctx.cancelled().await;
            Ok(())
        }
    })
    .unwrap();
    let (tx, source) = channel(1).unwrap();
    tx.send(message(1, 2, 1, "!p")).await.unwrap();
    drop(tx);
    let (cancel_tx, cancel) = watch::channel(false);
    let (report, _) = tokio::join!(bot.run(source, cancel), async {
        entered.notified().await;
        tokio::task::yield_now().await;
        cancel_tx.send_replace(true);
    });
    assert_eq!(report.stop, StopReason::Cancelled);
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(report.shutdown, Ok(()));
}
#[tokio::test]
async fn cancellation_before_start_and_disappearing_sender_do_not_invoke_handlers() {
    for mode in [true, false] {
        let mut bot = Bot::new(Config::default()).unwrap();
        bot.on(EventKind::All, |_| async {
            panic!("cancelled handler invoked")
        })
        .unwrap();
        let (tx, source) = channel(1).unwrap();
        tx.send(message(1, 2, 1, "!p")).await.unwrap();
        let (sender, cancel) = watch::channel(mode);
        drop(sender);
        let report = bot.run(source, cancel).await;
        assert_eq!(report.stop, StopReason::Cancelled);
        assert_eq!(report.received, 0);
    }
}
#[tokio::test]
async fn handler_and_command_failures_are_reported_once_without_replay() {
    let failures = Arc::new(Mutex::new(Vec::new()));
    let capture = failures.clone();
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.on(EventKind::Message, |_| async { Err(HandlerError::Failed) })
        .unwrap();
    bot.command("p", |_| async { Err(HandlerError::Failed) })
        .unwrap();
    bot.on_error(move |ctx| {
        let capture = capture.clone();
        async move {
            capture.lock().unwrap().push(ctx.failure().unwrap());
            Err(HandlerError::Failed)
        }
    })
    .unwrap();
    let report = run(bot, vec![message(1, 2, 1, "!p")]).await;
    assert_eq!(report.commands, 1);
    assert_eq!(report.completed, 1);
    assert_eq!(report.failures, 4);
    assert_eq!(
        *failures.lock().unwrap(),
        [
            Failure::Handler(HandlerError::Failed),
            Failure::Command(HandlerError::Failed)
        ]
    );
}
#[tokio::test]
async fn unknown_malformed_lag_and_sync_are_forwarded_without_commands() {
    let count = Arc::new(Mutex::new(Vec::new()));
    let capture = count.clone();
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.command("p", |_| async {
        panic!("notification replayed as command")
    })
    .unwrap();
    bot.on(EventKind::All, move |ctx| {
        let capture = capture.clone();
        async move {
            let label = match ctx.event() {
                Event::Unknown(sculk_client::messaging::Event::Unknown(p)) => {
                    assert_eq!(p.command.as_str(), "NEW_EVENT");
                    "unknown"
                }
                Event::Unknown(sculk_client::messaging::Event::Malformed(_)) => "malformed",
                Event::Lagged(7) => "lag",
                Event::Synchronization(_) => "sync",
                Event::Connection(_) => "connection",
                _ => panic!("wrong event"),
            };
            capture.lock().unwrap().push(label);
            Ok(())
        }
    })
    .unwrap();
    let unknown = Packet::from_document(
        0,
        WireCommand::new("NEW_EVENT").unwrap(),
        &bson::doc! {"message":"!p"},
    )
    .unwrap();
    let malformed = Packet::from_document(
        0,
        WireCommand::new("MSG").unwrap(),
        &bson::doc! {"message":"!p"},
    )
    .unwrap();
    let report = run(
        bot,
        vec![
            sculk_client::messaging::Event::from_packet(unknown).into(),
            sculk_client::messaging::Event::from_packet(malformed).into(),
            Event::Lagged(7),
            Event::Synchronization(Synchronization {
                room: RoomId::new(1).unwrap(),
                cursor: Cursor::default(),
                gap: Gap::Suspected,
            }),
            Event::Connection(Connection::Lost { expired: true }),
        ],
    )
    .await;
    assert_eq!(report.completed, 5);
    assert_eq!(report.commands, 0);
    assert_eq!(
        *count.lock().unwrap(),
        ["unknown", "malformed", "lag", "sync", "connection"]
    );
}
struct MockOutbound {
    sent: Mutex<Vec<String>>,
}
impl Outbound for MockOutbound {
    fn send<'a>(
        &'a self,
        _: RoomId,
        text: &'a str,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<OutboundReceipt, OutboundError>> {
        Box::pin(async move {
            self.sent.lock().unwrap().push(text.into());
            Ok(OutboundReceipt::MockAccepted)
        })
    }
}
#[tokio::test]
async fn replies_require_explicit_adapter_and_mock_success_is_distinct() {
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.command("p", |ctx| async move {
        ctx.reply("Pong!").await?;
        Ok(())
    })
    .unwrap();
    let report = run(bot, vec![message(1, 2, 1, "!p")]).await;
    assert_eq!(
        report.first_failure,
        Some(Failure::Command(HandlerError::Outbound(
            OutboundError::Unavailable
        )))
    );
    let outbound = Arc::new(MockOutbound {
        sent: Mutex::new(Vec::new()),
    });
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.outbound(outbound.clone());
    bot.command("p", |ctx| async move {
        assert_eq!(ctx.reply("Pong!").await?, OutboundReceipt::MockAccepted);
        Ok(())
    })
    .unwrap();
    let report = run(bot, vec![message(1, 2, 1, "!p")]).await;
    assert_eq!(report.failures, 0);
    assert_eq!(*outbound.sent.lock().unwrap(), ["Pong!"]);
}
struct BrokenSource {
    shutdown: bool,
    stall: bool,
}
impl EventSource for BrokenSource {
    fn recv(
        &mut self,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'_, Result<Option<Event>, SourceError>> {
        Box::pin(async { Err(SourceError::Receive) })
    }
    fn shutdown(&mut self) -> BoxFuture<'_, Result<(), SourceError>> {
        self.shutdown = true;
        Box::pin(async move {
            if self.stall {
                std::future::pending::<()>().await;
            }
            Err(SourceError::Shutdown)
        })
    }
}
#[tokio::test(start_paused = true)]
async fn source_errors_and_bounded_shutdown_are_not_clean_close_claims() {
    for stall in [false, true] {
        let bot = Bot::new(Config::default()).unwrap();
        let (_tx, cancel) = watch::channel(false);
        let report = bot
            .run(
                BrokenSource {
                    shutdown: false,
                    stall,
                },
                cancel,
            )
            .await;
        assert_eq!(report.stop, StopReason::Source(SourceError::Receive));
        assert_eq!(report.shutdown, Err(SourceError::Shutdown));
    }
}
#[tokio::test]
async fn oversize_events_and_invalid_limits_fail_explicitly() {
    assert!(Bot::new(Config {
        event_capacity: 0,
        ..Config::default()
    })
    .is_err());
    assert!(channel(0).is_err());
    let bot = Bot::new(Config {
        max_event_bytes: 100,
        ..Config::default()
    })
    .unwrap();
    let report = run(bot, vec![message(1, 2, 1, &"x".repeat(512))]).await;
    assert_eq!(report.stop, StopReason::EventTooLarge);
    assert_eq!(report.admitted, 0);
}
#[cfg(feature = "managed-client")]
#[tokio::test]
async fn managed_adapter_does_not_authenticate_or_reconnect_inactive_room() {
    use sculk_client::{
        managed::{Budget, Room},
        synchronization::MemoryStore,
    };
    let room = RoomId::new(1).unwrap();
    let store = MemoryStore::new(room, Cursor::default());
    let managed = Room::new(room, 8, Budget::default(), &store).unwrap();
    let bot = Bot::new(Config::default()).unwrap();
    let (_tx, cancel) = watch::channel(false);
    let report = bot.run(ManagedSource::new(managed), cancel).await;
    assert_eq!(report.completed, 2);
    assert_eq!(report.stop, StopReason::Exhausted);
    assert_eq!(report.shutdown, Ok(()));
}

#[cfg(feature = "managed-client")]
#[path = "../../sculk-client/tests/support/mod.rs"]
mod tls_support;
#[cfg(feature = "managed-client")]
#[tokio::test]
async fn managed_tls_messages_and_connection_loss_reach_bot_without_reconnect() {
    use futures_util::SinkExt;
    use sculk_client::{
        managed::{Budget, Room},
        recovery::RetryPolicy,
        session::{AccessToken, Grant, Session},
        synchronization::MemoryStore,
    };
    use tokio_util::codec::Framed;
    let (settings, acceptor) = tls_support::tls_pair();
    let (a, b) = tokio::io::duplex(8192);
    let (_tx, cancel) = watch::channel(false);
    let (session, peer) = tokio::join!(
        Session::establish(
            a,
            "localhost",
            &settings,
            Duration::from_secs(1),
            sculk_client::Config::default(),
            cancel.clone()
        ),
        acceptor.accept(b)
    );
    let mut session = session.unwrap();
    let mut peer = Framed::new(peer.unwrap(), sculk_protocol::LocoCodec::default());
    session
        .authenticate_with(
            AccessToken::new(
                "synthetic-only".into(),
                42,
                tokio::time::Instant::now() + Duration::from_secs(60),
            )
            .unwrap(),
            Duration::from_secs(1),
            cancel.clone(),
            |_, _| async {
                Ok(Grant {
                    user_id: 42,
                    valid_for: Duration::from_secs(30),
                })
            },
        )
        .await
        .unwrap();
    let room = RoomId::new(1).unwrap();
    let store = MemoryStore::new(room, Cursor::default());
    let mut managed = Room::new(room, 8, Budget::default(), &store).unwrap();
    let mut owned = Some(session);
    managed
        .reconnect(
            RetryPolicy {
                attempts: 1,
                initial: Duration::from_millis(1),
                maximum: Duration::from_millis(1),
            },
            Duration::from_secs(1),
            cancel.clone(),
            || std::future::ready(Ok(owned.take().expect("no automatic reconnect"))),
        )
        .await
        .unwrap();
    let seen = Arc::new(Notify::new());
    let capture = seen.clone();
    let states = Arc::new(Mutex::new(Vec::new()));
    let captured = states.clone();
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.command("ping", move |ctx| {
        let capture = capture.clone();
        async move {
            assert_eq!(ctx.content(), Some("!ping"));
            capture.notify_one();
            Ok(())
        }
    })
    .unwrap();
    bot.on(EventKind::Connection, move |ctx| {
        let captured = captured.clone();
        async move {
            if let Event::Connection(state) = ctx.event() {
                captured.lock().unwrap().push(state.clone());
            }
            Ok(())
        }
    })
    .unwrap();
    let (report, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(bot.run(ManagedSource::new(managed), cancel), async move {
            let Event::Message(m) = message(1, 2, i64::MAX, "!ping") else {
                unreachable!()
            };
            peer.send(m.packet().clone()).await.unwrap();
            seen.notified().await;
            drop(peer);
        })
    })
    .await
    .unwrap();
    assert_eq!(report.commands, 1);
    assert_eq!(report.stop, StopReason::Exhausted);
    assert_eq!(
        *states.lock().unwrap(),
        [
            Connection::State {
                generation: 1,
                state: SessionState::Authenticated
            },
            Connection::Lost { expired: false }
        ]
    );
    assert!(report.shutdown.is_err()); // Lost peer is not a verified graceful close.
}

struct OwnedSource {
    inner: ChannelSource,
    _guard: Guard,
}
impl EventSource for OwnedSource {
    fn recv(
        &mut self,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'_, Result<Option<Event>, SourceError>> {
        self.inner.recv(cancel)
    }
    fn shutdown(&mut self) -> BoxFuture<'_, Result<(), SourceError>> {
        self.inner.shutdown()
    }
}
#[tokio::test]
async fn dropping_active_runner_drops_source_and_handler_without_tasks() {
    let source_dropped = Arc::new(AtomicBool::new(false));
    let handler_dropped = Arc::new(AtomicBool::new(false));
    let captured = handler_dropped.clone();
    let entered = Arc::new(Notify::new());
    let capture = entered.clone();
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.command("p", move |_| {
        let capture = capture.clone();
        let guard = Guard(captured.clone());
        async move {
            let _guard = guard;
            capture.notify_one();
            std::future::pending::<()>().await;
            Ok(())
        }
    })
    .unwrap();
    let (tx, source) = channel(1).unwrap();
    tx.send(message(1, 2, 1, "!p")).await.unwrap();
    let (_tx, cancel) = watch::channel(false);
    {
        let running = bot.run(
            OwnedSource {
                inner: source,
                _guard: Guard(source_dropped.clone()),
            },
            cancel,
        );
        tokio::pin!(running);
        tokio::select! {_=&mut running=>panic!("runner should be active"),_=entered.notified()=>{}}
    }
    assert!(source_dropped.load(Ordering::SeqCst));
    assert!(handler_dropped.load(Ordering::SeqCst));
}

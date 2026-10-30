//! Synthetic fixtures only. These tests never authenticate or contact KakaoTalk.
use sculk_bot::{
    Bot, BoxFuture, Config, Event, Message, Outbound, OutboundError, OutboundReceipt, RoomId,
};
use sculk_script::{
    Category, CompiledScripts, Compiler, Limits, RuntimeDiagnostic, RuntimeError, Source,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{watch, Notify};
fn compile(text: &str) -> CompiledScripts {
    Compiler::default()
        .compile(&[Source {
            filename: "test.sculk",
            text,
        }])
        .unwrap()
}
fn event(id: i64, text: &str) -> Event {
    let p=sculk_protocol::Packet::from_document(0,sculk_protocol::Command::new("MSG").unwrap(),&bson::doc!{"chatId":i64::MAX,"chatLog":{"logId":id,"authorId":i64::MAX-1,"type":1,"sendAt":1700000000_i64,"message":text}}).unwrap();
    Event::Message(Message::from_packet(p).unwrap())
}
#[derive(Default)]
struct Mock {
    sent: Mutex<Vec<String>>,
}
impl Outbound for Mock {
    fn send<'a>(
        &'a self,
        room: RoomId,
        text: &'a str,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<OutboundReceipt, OutboundError>> {
        Box::pin(async move {
            assert_eq!(room.get(), i64::MAX);
            tokio::task::yield_now().await;
            self.sent.lock().unwrap().push(text.into());
            Ok(OutboundReceipt::MockAccepted)
        })
    }
}
async fn run(
    program: &CompiledScripts,
    inputs: &[&str],
) -> (Vec<String>, sculk_bot::RunReport, Vec<RuntimeDiagnostic>) {
    let outbound = Arc::new(Mock::default());
    let errors = Arc::new(Mutex::new(Vec::new()));
    let capture = errors.clone();
    let mut bot = Bot::new(Config {
        event_capacity: 64,
        ..Config::default()
    })
    .unwrap();
    bot.outbound(outbound.clone());
    let bot = program
        .install_with_errors(bot, Arc::new(move |d| capture.lock().unwrap().push(d)))
        .unwrap();
    let (tx, source) = sculk_bot::channel(64).unwrap();
    for (i, text) in inputs.iter().enumerate() {
        tx.send(event(i as i64 + 1, text)).await.unwrap();
    }
    drop(tx);
    let (_tx, cancel) = watch::channel(false);
    let report = tokio::time::timeout(Duration::from_secs(2), bot.run(source, cancel))
        .await
        .unwrap();
    let sent = outbound.sent.lock().unwrap().clone();
    let errors = errors.lock().unwrap().clone();
    (sent, report, errors)
}
#[tokio::test]
async fn both_extensions_compile_and_execute_identically() {
    let text = "on message:\n    if message is \"안녕\":\n        reply \"안녕하세요!\"\n";
    let a = Compiler::default()
        .compile(&[Source {
            filename: "greeting.sculk",
            text,
        }])
        .unwrap();
    let b = Compiler::default()
        .compile(&[Source {
            filename: "greeting.sc",
            text,
        }])
        .unwrap();
    assert_eq!(a.instruction_count(), b.instruction_count());
    assert_eq!(run(&a, &["안녕"]).await.0, run(&b, &["안녕"]).await.0);
    assert_eq!(run(&a, &["안녕"]).await.0, ["안녕하세요!"]);
    assert!(Compiler::default()
        .compile(&[Source {
            filename: "bad.txt",
            text
        }])
        .unwrap_err()
        .iter()
        .any(|d| d.category == Category::Loading));
}
#[tokio::test]
async fn unicode_locals_escaped_strings_comments_and_exact_integers() {
    let p = compile(
        r#"# comment
on message:
    set {_이름} to "한글 😀"
    set {_id} to 9223372036854775807
    set {_min} to -9223372036854775808
    if room is {_id} and sender is 9223372036854775806:
        reply "%{_이름}%:%room%:%sender%:%{_min}%"
    reply "line\n\"quote\"\\\% # kept" # ignored
"#,
    );
    let (out, report, _) = run(&p, &["synthetic"]).await;
    assert_eq!(report.failures, 0);
    assert_eq!(
        out,
        [
            "한글 😀:9223372036854775807:9223372036854775806:-9223372036854775808",
            "line\n\"quote\"\\% # kept"
        ]
    );
}
#[tokio::test]
async fn conditions_boolean_precedence_nested_else_stop_and_reassignment() {
    let p = compile(
        r#"on message:
    set {_x} to "initial"
    if message is "hello" or message is "hi" and true:
        set {_x} to "greeting"
        if message is not "hi":
            reply "Hello!"
        else:
            send "Hi!" to event-room
    else:
        set {_x} to "other"
    reply "%{_x}%"
    if (message is "stop"):
        stop
    reply "after"
"#,
    );
    assert_eq!(
        run(&p, &["hello", "hi", "stop"]).await.0,
        ["Hello!", "greeting", "after", "Hi!", "greeting", "after", "other"]
    );
}
#[tokio::test]
async fn required_prefix_formats_explicit_declarations_and_unicode_string_prefixes() {
    let p = compile(
        r#"command /slash:
    trigger:
        reply "slash"
command !bang:
    trigger:
        reply "bang"
command ?query:
    trigger:
        reply "query"
command .dot:
    trigger:
        reply "dot"
command >>debug:
    trigger:
        reply "debug"
command greet <name>:
    prefixes: "봇아 ", "@# "
    aliases: 인사
    trigger:
        reply "%arg-1%"
"#,
    );
    let (out, report, _) = run(
        &p,
        &[
            "/slash",
            "!bang",
            "?query",
            ".dot",
            ">>debug",
            "봇아 인사 한글",
            "@# greet \"John Smith\"",
            "/slashextra",
        ],
    )
    .await;
    assert_eq!(
        out,
        [
            "slash",
            "bang",
            "query",
            "dot",
            "debug",
            "한글",
            "John Smith"
        ]
    );
    assert_eq!(report.commands, 7);
    assert_eq!(report.failures, 1);
}
#[tokio::test]
async fn global_and_command_prefix_precedence_aliases_and_framework_default() {
    let p = compile(
        r#"options:
    prefixes: "/", "!"
command ping:
    aliases: p, 도움말
    trigger:
        reply "global"
command special:
    prefixes: ".", "?"
    trigger:
        reply "override"
command >>explicit:
    prefixes: ">>"
    trigger:
        reply "explicit"
"#,
    );
    let (out, report, _) = run(
        &p,
        &[
            "/ping",
            "!p",
            "/도움말",
            ".special",
            "?special",
            ">>explicit",
            "/special",
            "!explicit",
        ],
    )
    .await;
    assert_eq!(
        out,
        ["global", "global", "global", "override", "override", "explicit"]
    );
    assert_eq!(report.commands, 6);
    let default = compile("command p:\n    trigger:\n        reply \"default\"\n");
    assert_eq!(run(&default, &["!p"]).await.0, ["default"]);
    let custom = Compiler::default()
        .default_prefixes(vec!["?".into()])
        .compile(&[Source {
            filename: "a.sc",
            text: "command p:\n    trigger:\n        reply \"custom\"\n",
        }])
        .unwrap();
    assert_eq!(run(&custom, &["?p"]).await.0, ["custom"]);
}
#[tokio::test]
async fn longest_prefix_wins_without_falling_back_to_another_command() {
    let p=compile("command p:\n    prefixes: \">\"\n    trigger:\n        reply \"short\"\ncommand p:\n    prefixes: \">>\"\n    trigger:\n        reply \"long\"\n");
    let (out, report, _) = run(&p, &[">p", ">>p", ">>partial", ">>>p"]).await;
    assert_eq!(out, ["short", "long"]);
    assert_eq!(report.commands, 2);
    assert_eq!(report.failures, 2);
}
#[tokio::test]
async fn prefixless_and_case_policy_require_explicit_host_configuration() {
    let text="command ping:\n    prefixes: \"\"\n    aliases: 도움말\n    trigger:\n        reply \"ok\"\n";
    assert!(Compiler::default()
        .compile(&[Source {
            filename: "a.sc",
            text
        }])
        .is_err());
    let p = Compiler::new(Limits {
        allow_prefixless: true,
        ..Limits::default()
    })
    .compile(&[Source {
        filename: "a.sc",
        text,
    }])
    .unwrap();
    assert_eq!(run(&p, &["ping", "도움말"]).await.0, ["ok", "ok"]);
    let text = "command Ping:\n    trigger:\n        reply \"ok\"\n";
    let p = Compiler::default()
        .case_sensitive(false)
        .compile(&[Source {
            filename: "a.sc",
            text,
        }])
        .unwrap();
    assert!(p.install(Bot::new(Config::default()).unwrap()).is_err());
    let outbound = Arc::new(Mock::default());
    let mut bot = Bot::new(Config {
        case_sensitive: false,
        ..Config::default()
    })
    .unwrap();
    bot.outbound(outbound.clone());
    let bot = p.install(bot).unwrap();
    let (tx, source) = sculk_bot::channel(1).unwrap();
    tx.send(event(1, "!PING")).await.unwrap();
    drop(tx);
    let (_tx, cancel) = watch::channel(false);
    assert_eq!(bot.run(source, cancel).await.commands, 1);
}
#[test]
fn command_conflicts_ambiguous_aliases_and_explicit_prefix_conflicts_are_compile_errors() {
    let cases=[
        "command /p:\n    prefixes: \"!\"\n    trigger:\n        stop\n",
        "command p:\n    aliases: p\n    trigger:\n        stop\n",
        "command p:\n    aliases: x\n    trigger:\n        stop\ncommand x:\n    trigger:\n        stop\n",
        "command p <a> <a>:\n    trigger:\n        stop\n",
        "command p:\n    prefixes: \"!\", \"!\"\n    trigger:\n        stop\n",
    ];
    for text in cases {
        assert!(
            Compiler::default()
                .compile(&[Source {
                    filename: "a.sc",
                    text
                }])
                .is_err(),
            "{text}"
        );
    }
    let text = "command p:\n    trigger:\n        stop\n";
    assert!(Compiler::default()
        .compile(&[
            Source {
                filename: "a.sc",
                text
            },
            Source {
                filename: "b.sculk",
                text
            }
        ])
        .is_err());
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.command("p", |_| async { Ok(()) }).unwrap();
    assert!(compile(text).install(bot).is_err());
}
#[test]
fn type_scope_expression_and_unsupported_effect_errors_have_locations() {
    let cases = [
        "on message:\n    reply {_missing}\n",
        "on message:\n    set {_x} to true\n    set {_x} to 1\n",
        "on message:\n    if true:\n        set {_x} to \"branch\"\n    reply {_x}\n",
        "on message:\n    if message is 1:\n        stop\n",
        "on message:\n    reply 1\n",
        "on message:\n    reply \"%arg-1%\"\n",
        "command p <a>:\n    trigger:\n        reply \"%arg-2%\"\n",
        "on message:\n    reply \"%unknown%\"\n",
        "on message:\n    set {global} to 1\n",
        "on connection:\n    reply \"no room\"\n",
        "on join:\n    stop\n",
        "on message:\n    execute \"shell\"\n",
        "on message:\n    if true or 1:\n        stop\n",
    ];
    for text in cases {
        let errors = Compiler::default()
            .compile(&[Source {
                filename: "invalid.sc",
                text,
            }])
            .unwrap_err();
        assert!(!errors.is_empty());
        assert!(errors
            .iter()
            .all(|d| d.span.line >= 1 && d.span.column >= 1));
    }
}
#[test]
fn indentation_and_lexical_errors_are_rejected_without_logging_private_literals() {
    let missing = Compiler::default()
        .compile(&[Source {
            filename: "missing.sc",
            text: "command p:\n    prefixes:\n    trigger:\n        stop\n",
        }])
        .unwrap_err();
    assert_eq!(missing[0].span.line, 2);
    assert_eq!(missing[0].span.column, 5);
    assert!(Compiler::default()
        .compile(&[Source {
            filename: "comments.sc",
            text: "\t# ignored comment\n\t\non message:\n    stop\n"
        }])
        .is_ok());
    for text in [
        "on message:\n\treply \"x\"\n",
        "on message:\n  stop\n",
        "on message:\n        stop\n",
        "on message:\n    reply \"unclosed\n",
        "on message:\n    reply \"\\q\"\n",
        "on message:\n    reply \"%message\"\n",
        "on message:\n    if true:\n    stop\n",
    ] {
        assert!(Compiler::default()
            .compile(&[Source {
                filename: "a.sc",
                text
            }])
            .is_err());
    }
    let source = "on message:\n    repply \"DO_NOT_LOG_ME\"\n\non message:\n    nope\n";
    let errors = Compiler::default()
        .compile(&[Source {
            filename: "private.sc",
            text: source,
        }])
        .unwrap_err();
    assert!(errors.len() >= 2);
    assert_eq!(errors[0].span.line, 2);
    assert_eq!(errors[0].span.column, 5);
    assert!(!format!("{}", errors[0]).contains("DO_NOT_LOG_ME"));
    assert!(errors[0].with_excerpt(source).contains("DO_NOT_LOG_ME"));
}
#[tokio::test]
async fn argument_errors_are_isolated_and_bot_quoted_argument_parser_is_reused() {
    let p = compile("command greet <name>:\n    trigger:\n        reply \"Hello, %arg-1%!\"\n");
    let (out, report, errors) = run(
        &p,
        &[
            "!greet",
            "!greet \"John Smith\"",
            "!greet '한글 😀'",
            "!greet broken\\",
        ],
    )
    .await;
    assert_eq!(out, ["Hello, John Smith!", "Hello, 한글 😀!"]);
    assert_eq!(report.commands, 3);
    assert_eq!(report.failures, 2);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].error, RuntimeError::Argument);
}
#[tokio::test]
async fn local_variables_are_isolated_across_concurrent_bot_invocations() {
    let p=compile("command greet <name>:\n    trigger:\n        set {_x} to arg-1\n        reply \"wait\"\n        reply \"%{_x}%\"\n");
    let (a, b) = tokio::join!(run(&p, &["!greet Alice"]), run(&p, &["!greet Bob"]));
    assert_eq!(a.0, ["wait", "Alice"]);
    assert_eq!(b.0, ["wait", "Bob"]);
    assert_eq!(a.1.shutdown, Ok(()));
    assert_eq!(b.1.shutdown, Ok(()));
}
#[test]
fn compile_resource_budgets_and_invalid_configuration_are_enforced() {
    let cases = [
        (
            Limits {
                file_bytes: 8,
                ..Limits::default()
            },
            "on message:\n    stop\n",
        ),
        (
            Limits {
                instructions: 2,
                ..Limits::default()
            },
            "on message:\n    reply \"x\"\n",
        ),
        (
            Limits {
                depth: 2,
                ..Limits::default()
            },
            "on message:\n    if true:\n        if true:\n            stop\n",
        ),
        (
            Limits {
                locals: 1,
                ..Limits::default()
            },
            "on message:\n    set {_x} to 1\n    set {_y} to 2\n",
        ),
        (
            Limits {
                string_bytes: 2,
                ..Limits::default()
            },
            "on message:\n    reply \"long\"\n",
        ),
        (
            Limits {
                handler_time: Duration::ZERO,
                ..Limits::default()
            },
            "on message:\n    stop\n",
        ),
    ];
    for (limits, text) in cases {
        assert!(Compiler::new(limits)
            .compile(&[Source {
                filename: "a.sc",
                text
            }])
            .is_err());
    }
    let c = Compiler::new(Limits {
        scripts: 1,
        ..Limits::default()
    });
    assert!(c
        .compile(&[
            Source {
                filename: "a.sc",
                text: ""
            },
            Source {
                filename: "b.sc",
                text: ""
            }
        ])
        .is_err());
}
#[tokio::test]
async fn runtime_steps_and_interpolation_output_have_independent_limits() {
    let p = Compiler::new(Limits {
        steps: 1,
        ..Limits::default()
    })
    .compile(&[Source {
        filename: "a.sc",
        text: "on message:\n    reply \"x\"\n",
    }])
    .unwrap();
    let (out, report, errors) = run(&p, &["x"]).await;
    assert!(out.is_empty());
    assert_eq!(report.failures, 1);
    assert_eq!(errors[0].error, RuntimeError::Steps);
    let p = Compiler::new(Limits {
        interpolation_bytes: 8,
        ..Limits::default()
    })
    .compile(&[Source {
        filename: "a.sc",
        text: "command p <x>:\n    trigger:\n        reply \"xx%arg-1%\"\n",
    }])
    .unwrap();
    let (out, _, errors) = run(&p, &["!p 123456789"]).await;
    assert!(out.is_empty());
    assert_eq!(errors[0].error, RuntimeError::StringLimit);
}
struct DropGuard(Arc<AtomicBool>);
impl Drop for DropGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
struct Stall {
    entered: Arc<Notify>,
    dropped: Arc<AtomicBool>,
    calls: AtomicUsize,
}
impl Outbound for Stall {
    fn send<'a>(
        &'a self,
        _: RoomId,
        _: &'a str,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<OutboundReceipt, OutboundError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let _guard = DropGuard(self.dropped.clone());
            self.entered.notify_one();
            std::future::pending().await
        })
    }
}
#[tokio::test(start_paused = true)]
async fn handler_deadline_drops_stalled_outbound_without_retry() {
    let p = Compiler::new(Limits {
        handler_time: Duration::from_millis(5),
        ..Limits::default()
    })
    .compile(&[Source {
        filename: "a.sc",
        text: "on message:\n    reply \"x\"\n",
    }])
    .unwrap();
    let outbound = Arc::new(Stall {
        entered: Arc::new(Notify::new()),
        dropped: Arc::new(AtomicBool::new(false)),
        calls: AtomicUsize::new(0),
    });
    let errors = Arc::new(Mutex::new(Vec::new()));
    let capture = errors.clone();
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.outbound(outbound.clone());
    let bot = p
        .install_with_errors(bot, Arc::new(move |d| capture.lock().unwrap().push(d)))
        .unwrap();
    let (tx, source) = sculk_bot::channel(1).unwrap();
    tx.send(event(1, "input")).await.unwrap();
    drop(tx);
    let (_tx, cancel) = watch::channel(false);
    let report = bot.run(source, cancel).await;
    assert_eq!(report.failures, 1);
    assert_eq!(errors.lock().unwrap()[0].error, RuntimeError::Deadline);
    assert!(outbound.dropped.load(Ordering::SeqCst));
    assert_eq!(outbound.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn cancellation_interrupts_script_and_gracefully_cleans_source() {
    let p = compile("on message:\n    reply \"x\"\n");
    let outbound = Arc::new(Stall {
        entered: Arc::new(Notify::new()),
        dropped: Arc::new(AtomicBool::new(false)),
        calls: AtomicUsize::new(0),
    });
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.outbound(outbound.clone());
    let bot = p.install(bot).unwrap();
    let (tx, source) = sculk_bot::channel(1).unwrap();
    tx.send(event(1, "input")).await.unwrap();
    drop(tx);
    let (cancel_tx, cancel) = watch::channel(false);
    let (report, ()) = tokio::join!(bot.run(source, cancel), async {
        outbound.entered.notified().await;
        cancel_tx.send_replace(true);
    });
    assert_eq!(report.stop, sculk_bot::StopReason::Cancelled);
    assert_eq!(report.shutdown, Ok(()));
    assert!(outbound.dropped.load(Ordering::SeqCst));
    assert_eq!(outbound.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn commands_use_existing_host_permission_registry_before_any_script_effect() {
    let p = compile(
        "command p:\n    permissions: \"commands.p\"\n    trigger:\n        reply \"secret\"\n",
    );
    assert!(p.install(Bot::new(Config::default()).unwrap()).is_err());
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.permission(sculk_bot::PermissionId::new("commands.p").unwrap(), false)
        .unwrap();
    let bot = p.install(bot).unwrap();
    let (tx, source) = sculk_bot::channel(1).unwrap();
    tx.send(event(1, "!p")).await.unwrap();
    drop(tx);
    let (_tx, cancel) = watch::channel(false);
    let report = bot.run(source, cancel).await;
    assert_eq!(report.commands, 0);
    assert_eq!(
        report.first_failure,
        Some(sculk_bot::Failure::PermissionDenied)
    );
}
#[test]
fn bounded_file_loader_rejects_non_utf8_extensions_and_oversized_sources() {
    let root = std::env::temp_dir().join(format!("sculk-script-test-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let valid = root.join("valid.sc");
    let invalid = root.join("invalid.sculk");
    std::fs::write(&valid, "on message:\n    stop\n").unwrap();
    std::fs::write(&invalid, [0xff]).unwrap();
    assert!(Compiler::default().load_files(&[&valid]).is_ok());
    assert!(Compiler::default().load_files(&[&invalid]).is_err());
    assert!(Compiler::default()
        .load_files(&[root.join("unknown.txt")])
        .is_err());
    assert!(Compiler::new(Limits {
        file_bytes: 4,
        ..Limits::default()
    })
    .load_files(&[&valid])
    .is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn unknown_and_lifecycle_events_remain_typed_without_invented_actions() {
    let p=compile("on unknown:\n    stop\non connection:\n    set {_seen} to true\non synchronization:\n    send \"sync snapshot\" to event-room\non lag:\n    stop\n");
    let outbound = Arc::new(Mock::default());
    let observed = Arc::new(AtomicBool::new(false));
    let capture = observed.clone();
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.outbound(outbound.clone());
    bot.on(sculk_bot::EventKind::Unknown, move |ctx| {
        let capture = capture.clone();
        async move {
            if let Event::Unknown(sculk_bot::ClientEvent::Unknown(packet)) = ctx.event() {
                assert_eq!(packet.command.as_str(), "FUTURE");
                capture.store(true, Ordering::SeqCst);
            }
            Ok(())
        }
    })
    .unwrap();
    let bot = p.install(bot).unwrap();
    let (tx, source) = sculk_bot::channel(4).unwrap();
    let packet = sculk_protocol::Packet::from_document(
        0,
        sculk_protocol::Command::new("FUTURE").unwrap(),
        &bson::doc! {"opaque":true},
    )
    .unwrap();
    for event in [
        sculk_bot::ClientEvent::from_packet(packet).into(),
        Event::Connection(sculk_bot::Connection::Lost { expired: true }),
        Event::Synchronization(sculk_bot::Synchronization {
            room: RoomId::new(i64::MAX).unwrap(),
            cursor: sculk_bot::Cursor::default(),
            gap: sculk_bot::Gap::Suspected,
        }),
        Event::Lagged(4),
    ] {
        tx.send(event).await.unwrap();
    }
    drop(tx);
    let (_tx, cancel) = watch::channel(false);
    let report = bot.run(source, cancel).await;
    assert_eq!(report.failures, 0);
    assert_eq!(report.commands, 0);
    assert!(observed.load(Ordering::SeqCst));
    assert_eq!(*outbound.sent.lock().unwrap(), ["sync snapshot"]);
}
#[tokio::test]
async fn argument_content_is_data_not_reparsed_interpolation_or_effects() {
    let p = compile("command p <value>:\n    trigger:\n        reply \"Value=%arg-1%\"\n");
    assert_eq!(
        run(&p, &["!p '%room% reply \"injected\"'"]).await.0,
        ["Value=%room% reply \"injected\""]
    );
}

struct FailFirst {
    calls: AtomicUsize,
}
impl Outbound for FailFirst {
    fn send<'a>(
        &'a self,
        _: RoomId,
        _: &'a str,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<OutboundReceipt, OutboundError>> {
        Box::pin(async move {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(OutboundError::NotSent)
            } else {
                Ok(OutboundReceipt::MockAccepted)
            }
        })
    }
}
#[tokio::test]
async fn outbound_failure_is_reported_once_and_later_invocations_continue() {
    let program = compile("on message:\n    reply \"test\"\n");
    let outbound = Arc::new(FailFirst {
        calls: AtomicUsize::new(0),
    });
    let errors = Arc::new(Mutex::new(Vec::new()));
    let capture = errors.clone();
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.outbound(outbound.clone());
    let bot = program
        .install_with_errors(bot, Arc::new(move |d| capture.lock().unwrap().push(d)))
        .unwrap();
    let (tx, source) = sculk_bot::channel(2).unwrap();
    tx.send(event(1, "first")).await.unwrap();
    tx.send(event(2, "second")).await.unwrap();
    drop(tx);
    let (_tx, cancel) = watch::channel(false);
    let report = bot.run(source, cancel).await;
    assert_eq!(report.completed, 2);
    assert_eq!(report.failures, 1);
    assert_eq!(outbound.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        errors.lock().unwrap()[0].error,
        RuntimeError::Outbound(OutboundError::NotSent)
    );
    assert_eq!(report.shutdown, Ok(()));
}

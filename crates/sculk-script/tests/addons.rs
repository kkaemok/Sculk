//! Synthetic addon data and local mock effects. No credentials or network.
use sculk_bot::{
    Bot, BoxFuture, Config, Event, EventKind, Message, Outbound, OutboundError, OutboundReceipt,
    RoomId,
};
use sculk_script::{
    addon::{AddonContext, AddonError, Capability, RegistrationError, Registry, Syntax},
    Category, CompiledScripts, Compiler, Limits, RuntimeDiagnostic, RuntimeError, Source, Type,
    Value,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{watch, Notify};
fn source(text: &str) -> Source<'_> {
    Source {
        filename: "addon.sculk",
        text,
    }
}
fn event(id: i64, text: &str) -> Event {
    let p=sculk_protocol::Packet::from_document(0,sculk_protocol::Command::new("MSG").unwrap(),&bson::doc!{"chatId":i64::MAX,"chatLog":{"logId":id,"authorId":i64::MAX-1,"type":1,"sendAt":1_i64,"message":text}}).unwrap();
    Event::Message(Message::from_packet(p).unwrap())
}
#[derive(Default)]
struct Mock {
    sent: Mutex<Vec<String>>,
}
impl Outbound for Mock {
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
async fn run(
    p: &CompiledScripts,
    texts: &[&str],
) -> (Vec<String>, sculk_bot::RunReport, Vec<RuntimeDiagnostic>) {
    let mock = Arc::new(Mock::default());
    let errors = Arc::new(Mutex::new(Vec::new()));
    let capture = errors.clone();
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.outbound(mock.clone());
    let bot = p
        .install_with_errors(bot, Arc::new(move |d| capture.lock().unwrap().push(d)))
        .unwrap();
    let (tx, events) = sculk_bot::channel(16).unwrap();
    for (i, text) in texts.iter().enumerate() {
        tx.send(event(i as i64 + 1, text)).await.unwrap();
    }
    drop(tx);
    let (_tx, cancel) = watch::channel(false);
    let report = tokio::time::timeout(Duration::from_secs(2), bot.run(events, cancel))
        .await
        .unwrap();
    let sent = mock.sent.lock().unwrap().clone();
    let errors = errors.lock().unwrap().clone();
    (sent, report, errors)
}
fn registry() -> Registry {
    let mut r = Registry::new();
    r.expression(
        Syntax::new("reputation of %who%")
            .parameter("who", Type::Integer)
            .returns(Type::Integer)
            .cost(10),
        |_, args| {
            Ok(Value::Integer(
                if args[0].as_integer() == Some(i64::MAX - 1) {
                    42
                } else {
                    7
                },
            ))
        },
    )
    .unwrap();
    r.condition(
        Syntax::new("trusted score %score%")
            .parameter("score", Type::Integer)
            .cost(3),
        |_, args| Ok(args[0].as_integer().ok_or(AddonError::Type)? >= 40),
    )
    .unwrap();
    r.effect(
        Syntax::new("announce score %score%")
            .parameter("score", Type::Integer)
            .cost(2)
            .requires(Capability::Reply),
        |ctx, args| async move {
            tokio::task::yield_now().await;
            ctx.reply(&format!(
                "Score: {}",
                args[0].as_integer().ok_or(AddonError::Type)?
            ))
            .await?;
            Ok(())
        },
    )
    .unwrap();
    r
}
#[test]
fn registrations_validate_metadata_and_do_not_execute_native_code() {
    let count = Arc::new(AtomicUsize::new(0));
    let capture = count.clone();
    let mut r = Registry::new();
    let id = r
        .expression(
            Syntax::new("reputation of %who%")
                .parameter("who", Type::Integer)
                .returns(Type::Integer)
                .cost(10),
            move |_, _| {
                capture.fetch_add(1, Ordering::SeqCst);
                Ok(Value::Integer(42))
            },
        )
        .unwrap();
    let descriptor = r.descriptor(id).unwrap();
    assert_eq!(descriptor.parameters[0].ty, Type::Integer);
    assert_eq!(descriptor.cost, 10);
    assert_eq!(descriptor.returns, Some(Type::Integer));
    assert!(descriptor.capabilities.is_empty());
    assert_eq!(r.len(), 1);
    let p = Compiler::default()
        .with_registry(r)
        .compile(&[source(
            "on message:\n    reply \"%reputation of sender%\"\n",
        )])
        .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(p.instruction_count() > 1);
}
#[test]
fn duplicate_ambiguous_reserved_and_invalid_patterns_are_rejected_atomically() {
    let mut r = Registry::new();
    r.expression(
        Syntax::new("rank of %who%")
            .parameter("who", Type::Integer)
            .returns(Type::Integer)
            .cost(1),
        |_, _| Ok(Value::Integer(1)),
    )
    .unwrap();
    assert_eq!(
        r.expression(
            Syntax::new("rank of %subject%")
                .parameter("subject", Type::String)
                .returns(Type::String)
                .cost(2),
            |_, _| Ok(Value::String("x".into()))
        ),
        Err(RegistrationError::Duplicate)
    );
    assert_eq!(
        r.condition(
            Syntax::new("rank %value% %tail%")
                .parameter("value", Type::String)
                .parameter("tail", Type::String)
                .cost(1),
            |_, _| Ok(true)
        ),
        Err(RegistrationError::Ambiguous)
    );
    assert_eq!(
        r.condition(
            Syntax::new("rank of %who% later")
                .parameter("who", Type::Integer)
                .cost(1),
            |_, _| Ok(true)
        ),
        Err(RegistrationError::Ambiguous)
    );
    for pattern in ["message", "reply %x%", "label and %x%"] {
        let mut syntax = Syntax::new(pattern).cost(1);
        if pattern.contains("%x%") {
            syntax = syntax.parameter("x", Type::String);
        }
        assert_eq!(
            r.condition(syntax, |_, _| Ok(true)),
            Err(RegistrationError::Reserved)
        );
    }
    assert_eq!(
        r.condition(
            Syntax::new("%x% tail")
                .parameter("x", Type::Integer)
                .cost(1),
            |_, _| Ok(true)
        ),
        Err(RegistrationError::Pattern)
    );
    assert_eq!(
        r.condition(Syntax::new("label [optional]").cost(1), |_, _| Ok(true)),
        Err(RegistrationError::Pattern)
    );
    assert_eq!(
        r.condition(Syntax::new("-1 value").cost(1), |_, _| Ok(true)),
        Err(RegistrationError::Pattern)
    );
    assert_eq!(
        r.condition(Syntax::new("label %missing%").cost(1), |_, _| Ok(true)),
        Err(RegistrationError::Signature)
    );
    assert_eq!(
        r.condition(Syntax::new("label").cost(0), |_, _| Ok(true)),
        Err(RegistrationError::Cost)
    );
    assert_eq!(
        r.expression(Syntax::new("label").cost(1), |_, _| Ok(Value::Integer(1))),
        Err(RegistrationError::Signature)
    );
    assert_eq!(
        r.event(Syntax::new("arrival").cost(1), EventKind::All, |_| Ok(true)),
        Err(RegistrationError::Signature)
    );
    assert_eq!(r.len(), 1);
}
#[tokio::test]
async fn expressions_conditions_effects_and_event_filters_share_bot_dispatch() {
    let mut r = registry();
    r.event(
        Syntax::new("synthetic report")
            .cost(2)
            .requires(Capability::ReadMessage),
        EventKind::Message,
        |ctx| Ok(ctx.message()? == "profile"),
    )
    .unwrap();
    let text="on synthetic report:\n    set {_score} to reputation of sender\n    if trusted score {_score}:\n        announce score {_score}\n";
    let p = Compiler::default()
        .with_registry(r)
        .capabilities([Capability::Reply, Capability::ReadMessage])
        .compile(&[source(text)])
        .unwrap();
    let (out, report, errors) = run(&p, &["ignored", "profile"]).await;
    assert_eq!(out, ["Score: 42"]);
    assert_eq!(report.completed, 2);
    assert_eq!(report.commands, 0);
    assert!(errors.is_empty());
    assert_eq!(report.shutdown, Ok(()));
}
#[tokio::test]
async fn nested_custom_expressions_interpolation_and_short_circuiting_are_compiled_once() {
    let r = registry();
    let p=Compiler::default().with_registry(r).compile(&[source("on message:\n    if false and trusted score (reputation of sender):\n        stop\n    if trusted score (reputation of sender):\n        reply \"%reputation of sender%\"\n")]).unwrap();
    assert_eq!(run(&p, &["input"]).await.0, ["42"]);
}
#[test]
fn incorrect_argument_types_and_malformed_calls_have_source_diagnostics() {
    let compiler = Compiler::default().with_registry(registry());
    let errors = compiler
        .compile(&[source(
            "on message:\n    set {_score} to reputation of \"bad\"\n",
        )])
        .unwrap_err();
    let type_error = errors
        .iter()
        .find(|d| d.category == Category::Type)
        .unwrap();
    assert_eq!(type_error.span.line, 2);
    assert!(type_error.span.column > 5);
    assert!(compiler
        .compile(&[source(
            "on message:\n    if trusted score reputation of sender:\n        stop\n"
        )])
        .is_err());
    assert!(compiler
        .compile(&[source(
            "on message:\n    reply \"%reputation of (sender%\"\n"
        )])
        .is_err());
}
#[tokio::test]
async fn capability_grants_are_explicit_and_do_not_grant_undeclared_facilities() {
    let text = "on message:\n    announce score 42\n";
    let errors = Compiler::default()
        .with_registry(registry())
        .compile(&[source(text)])
        .unwrap_err();
    assert!(errors.iter().any(|d| d.category == Category::Capability));
    let mut r = Registry::new();
    r.effect(Syntax::new("attempt reply").cost(1), |ctx, _| async move {
        ctx.reply("must not send").await?;
        Ok(())
    })
    .unwrap();
    let p = Compiler::default()
        .with_registry(r)
        .capabilities([Capability::Reply])
        .compile(&[source("on message:\n    attempt reply\n")])
        .unwrap();
    let (out, report, errors) = run(&p, &["input"]).await;
    assert!(out.is_empty());
    assert_eq!(report.failures, 1);
    assert_eq!(errors[0].error, RuntimeError::Addon(AddonError::Capability));
}
#[tokio::test]
async fn identity_and_message_facilities_preserve_exact_int64_and_require_declarations() {
    let mut r = Registry::new();
    r.expression(
        Syntax::new("own identity")
            .returns(Type::Integer)
            .cost(1)
            .requires(Capability::ReadIdentity),
        |ctx, _| {
            assert_eq!(ctx.room_id()?, i64::MAX);
            Ok(Value::Integer(ctx.sender_id()?))
        },
    )
    .unwrap();
    let p = Compiler::default()
        .with_registry(r)
        .capabilities([Capability::ReadIdentity])
        .compile(&[source("on message:\n    reply \"%own identity%\"\n")])
        .unwrap();
    assert_eq!(run(&p, &["input"]).await.0, [(i64::MAX - 1).to_string()]);
    assert!(!format!("{:?}", Value::String("PRIVATE".into())).contains("PRIVATE"));
    assert!(!format!("{:?}", Value::Integer(i64::MAX)).contains("922337"));
}
#[tokio::test]
async fn runtime_result_types_errors_and_following_handlers_are_isolated() {
    let mut r = Registry::new();
    r.expression(
        Syntax::new("bad result").returns(Type::Integer).cost(1),
        |_, _| Ok(Value::Boolean(true)),
    )
    .unwrap();
    let p = Compiler::default()
        .with_registry(r)
        .compile(&[source(
            "on message:\n    reply \"%bad result%\"\non message:\n    reply \"after\"\n",
        )])
        .unwrap();
    let (out, report, errors) = run(&p, &["input"]).await;
    assert_eq!(out, ["after"]);
    assert_eq!(report.failures, 1);
    assert_eq!(errors[0].error, RuntimeError::Addon(AddonError::Type));
    let mut r = Registry::new();
    r.effect(Syntax::new("fail operation").cost(1), |_, _| async {
        Err(AddonError::Failed)
    })
    .unwrap();
    let p = Compiler::default()
        .with_registry(r)
        .compile(&[source(
            "on message:\n    fail operation\non message:\n    reply \"still active\"\n",
        )])
        .unwrap();
    assert_eq!(run(&p, &["input"]).await.0, ["still active"]);
}
#[tokio::test]
async fn declared_cost_is_charged_for_each_occurrence_before_native_execution() {
    let calls = Arc::new(AtomicUsize::new(0));
    let capture = calls.clone();
    let mut r = Registry::new();
    r.expression(
        Syntax::new("expensive value")
            .returns(Type::Integer)
            .cost(5),
        move |_, _| {
            capture.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Integer(1))
        },
    )
    .unwrap();
    let p = Compiler::new(Limits {
        steps: 10,
        ..Limits::default()
    })
    .with_registry(r)
    .compile(&[source(
        "on message:\n    set {_one} to expensive value\n    set {_two} to expensive value\n",
    )])
    .unwrap();
    let (_, report, errors) = run(&p, &["input"]).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(report.failures, 1);
    assert_eq!(errors[0].error, RuntimeError::Steps);
}
#[tokio::test]
async fn repeated_facility_operations_share_the_real_budget_not_just_declared_cost() {
    let mut r = Registry::new();
    r.effect(
        Syntax::new("repeat mock")
            .cost(1)
            .requires(Capability::Reply),
        |ctx, _| async move {
            for _ in 0..100 {
                ctx.reply("bounded").await?;
            }
            Ok(())
        },
    )
    .unwrap();
    let p = Compiler::new(Limits {
        steps: 8,
        ..Limits::default()
    })
    .with_registry(r)
    .capabilities([Capability::Reply])
    .compile(&[source("on message:\n    repeat mock\n")])
    .unwrap();
    let (out, report, errors) = run(&p, &["input"]).await;
    assert!(!out.is_empty() && out.len() < 8);
    assert_eq!(report.failures, 1);
    assert_eq!(errors[0].error, RuntimeError::Steps);
}
#[tokio::test]
async fn cooperative_native_work_and_native_string_results_have_real_limits() {
    let mut r = Registry::new();
    r.expression(
        Syntax::new("cooperative value")
            .returns(Type::Integer)
            .cost(1),
        |ctx, _| {
            let _ = ctx.charge(usize::MAX); // violation must remain latched even if native code ignores it
            Ok(Value::Integer(1))
        },
    )
    .unwrap();
    let p = Compiler::default()
        .with_registry(r)
        .compile(&[source("on message:\n    reply \"%cooperative value%\"\n")])
        .unwrap();
    assert_eq!(run(&p, &["input"]).await.2[0].error, RuntimeError::Steps);
    let mut r = Registry::new();
    r.expression(
        Syntax::new("long result").returns(Type::String).cost(1),
        |_, _| Ok(Value::String("x".repeat(1024).into())),
    )
    .unwrap();
    let p = Compiler::new(Limits {
        string_bytes: 16,
        ..Limits::default()
    })
    .with_registry(r)
    .compile(&[source("on message:\n    reply long result\n")])
    .unwrap();
    assert_eq!(
        run(&p, &["input"]).await.2[0].error,
        RuntimeError::StringLimit
    );
}
struct Guard(Arc<AtomicBool>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[tokio::test(start_paused = true)]
async fn async_deadline_drops_addon_future_and_revokes_retained_context() {
    let dropped = Arc::new(AtomicBool::new(false));
    let capture = dropped.clone();
    let retained = Arc::new(Mutex::new(None::<AddonContext>));
    let save = retained.clone();
    let mut r = Registry::new();
    r.effect(
        Syntax::new("stall effect")
            .cost(1)
            .requires(Capability::Reply),
        move |ctx, _| {
            let guard = Guard(capture.clone());
            *save.lock().unwrap() = Some(ctx);
            async move {
                let _guard = guard;
                std::future::pending().await
            }
        },
    )
    .unwrap();
    let p = Compiler::new(Limits {
        handler_time: Duration::from_millis(5),
        ..Limits::default()
    })
    .with_registry(r)
    .capabilities([Capability::Reply])
    .compile(&[source("on message:\n    stall effect\n")])
    .unwrap();
    let (_, report, errors) = run(&p, &["input"]).await;
    assert_eq!(report.failures, 1);
    assert_eq!(errors[0].error, RuntimeError::Deadline);
    assert!(dropped.load(Ordering::SeqCst));
    let ctx = retained.lock().unwrap().take().unwrap();
    assert_eq!(ctx.reply("expired").await, Err(AddonError::Closed));
}
#[tokio::test]
async fn external_cancellation_cleans_up_active_native_effect_without_tasks() {
    let entered = Arc::new(Notify::new());
    let notify = entered.clone();
    let dropped = Arc::new(AtomicBool::new(false));
    let capture = dropped.clone();
    let mut r = Registry::new();
    r.effect(Syntax::new("wait effect").cost(1), move |ctx, _| {
        let notify = notify.clone();
        let guard = Guard(capture.clone());
        async move {
            let _guard = guard;
            notify.notify_one();
            ctx.cancelled().await;
            Ok(())
        }
    })
    .unwrap();
    let p = Compiler::default()
        .with_registry(r)
        .compile(&[source("on message:\n    wait effect\n")])
        .unwrap();
    let bot = p.install(Bot::new(Config::default()).unwrap()).unwrap();
    let (tx, events) = sculk_bot::channel(1).unwrap();
    tx.send(event(1, "input")).await.unwrap();
    drop(tx);
    let (cancel_tx, cancel) = watch::channel(false);
    let (report, ()) = tokio::join!(bot.run(events, cancel), async {
        entered.notified().await;
        cancel_tx.send_replace(true);
    });
    assert_eq!(report.stop, sculk_bot::StopReason::Cancelled);
    assert_eq!(report.shutdown, Ok(()));
    assert!(dropped.load(Ordering::SeqCst));
}
#[tokio::test]
async fn successful_callbacks_also_expire_captured_contexts() {
    let retained = Arc::new(Mutex::new(None::<AddonContext>));
    let save = retained.clone();
    let mut r = Registry::new();
    r.expression(
        Syntax::new("saved value")
            .returns(Type::Integer)
            .cost(1)
            .requires(Capability::ReadMessage),
        move |ctx, _| {
            *save.lock().unwrap() = Some(ctx);
            Ok(Value::Integer(1))
        },
    )
    .unwrap();
    let p = Compiler::default()
        .with_registry(r)
        .capabilities([Capability::ReadMessage])
        .compile(&[source("on message:\n    set {_value} to saved value\n")])
        .unwrap();
    assert_eq!(run(&p, &["input"]).await.1.failures, 0);
    assert_eq!(
        retained.lock().unwrap().take().unwrap().message(),
        Err(AddonError::Closed)
    );
}
#[tokio::test]
async fn unicode_patterns_and_both_extensions_reuse_commands_aliases_locals_and_templates() {
    let mut r = Registry::new();
    r.expression(
        Syntax::new("평판 값 %대상%")
            .parameter("대상", Type::String)
            .returns(Type::String)
            .cost(1),
        |_, args| Ok(args[0].clone()),
    )
    .unwrap();
    let text="options:\n    prefixes: \"!\", \">>\"\ncommand greet <name>:\n    aliases: 인사\n    trigger:\n        set {_name} to 평판 값 arg-1\n        reply \"Hello, %{_name}%\"\n";
    for filename in ["unicode.sculk", "unicode.sc"] {
        let p = Compiler::default()
            .with_registry(r.clone())
            .compile(&[Source { filename, text }])
            .unwrap();
        assert_eq!(run(&p, &[">>인사 '한글 😀'"]).await.0, ["Hello, 한글 😀"]);
    }
}
#[test]
fn distinct_literal_branches_are_deterministic_and_limits_are_bounded() {
    let mut r = Registry::new();
    for name in ["score for %id%", "score from %id%"] {
        r.expression(
            Syntax::new(name)
                .parameter("id", Type::Integer)
                .returns(Type::Integer)
                .cost(1),
            |_, _| Ok(Value::Integer(1)),
        )
        .unwrap();
    }
    for i in 2..64 {
        r.condition(Syntax::new(format!("entry_{i}")).cost(1), |_, _| Ok(true))
            .unwrap();
    }
    assert_eq!(
        r.condition(Syntax::new("overflow").cost(1), |_, _| Ok(true)),
        Err(RegistrationError::Limit)
    );
    assert_eq!(r.len(), 64);
}

#[tokio::test]
async fn application_capabilities_are_distinct_from_facilities_and_explicitly_granted() {
    let application =
        Capability::Application(sculk_bot::PermissionId::new("synthetic.store").unwrap());
    let needed = application.clone();
    let mut r = Registry::new();
    r.condition(
        Syntax::new("application ready")
            .cost(1)
            .requires(application.clone()),
        move |ctx, _| {
            Ok(ctx.has_capability(&needed) && !ctx.has_capability(&Capability::ReadMessage))
        },
    )
    .unwrap();
    let text = "on message:\n    if application ready:\n        reply \"ready\"\n";
    assert!(Compiler::default()
        .with_registry(r.clone())
        .compile(&[source(text)])
        .is_err());
    let p = Compiler::default()
        .with_registry(r)
        .capabilities([application, Capability::ReadMessage])
        .compile(&[source(text)])
        .unwrap();
    assert_eq!(run(&p, &["input"]).await.0, ["ready"]);
}
struct PendingOutbound {
    entered: Arc<Notify>,
    dropped: Arc<AtomicBool>,
    calls: AtomicUsize,
}
impl Outbound for PendingOutbound {
    fn send<'a>(
        &'a self,
        _: RoomId,
        _: &'a str,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<OutboundReceipt, OutboundError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let _guard = Guard(self.dropped.clone());
            self.entered.notify_one();
            std::future::pending().await
        })
    }
}
#[tokio::test]
async fn cancellation_after_native_send_submission_never_replays_or_claims_delivery() {
    let outbound = Arc::new(PendingOutbound {
        entered: Arc::new(Notify::new()),
        dropped: Arc::new(AtomicBool::new(false)),
        calls: AtomicUsize::new(0),
    });
    let mut r = Registry::new();
    r.effect(
        Syntax::new("submit note")
            .cost(1)
            .requires(Capability::Reply),
        |ctx, _| async move {
            ctx.reply("test").await?;
            Ok(())
        },
    )
    .unwrap();
    let p = Compiler::default()
        .with_registry(r)
        .capabilities([Capability::Reply])
        .compile(&[source("on message:\n    submit note\n")])
        .unwrap();
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.outbound(outbound.clone());
    let bot = p.install(bot).unwrap();
    let (tx, events) = sculk_bot::channel(1).unwrap();
    tx.send(event(1, "input")).await.unwrap();
    drop(tx);
    let (cancel_tx, cancel) = watch::channel(false);
    let (report, ()) = tokio::join!(bot.run(events, cancel), async {
        outbound.entered.notified().await;
        cancel_tx.send_replace(true);
    });
    assert_eq!(report.stop, sculk_bot::StopReason::Cancelled);
    assert_eq!(outbound.calls.load(Ordering::SeqCst), 1);
    assert!(outbound.dropped.load(Ordering::SeqCst));
    assert_eq!(report.completed, 0);
}
#[tokio::test]
async fn execution_counters_include_native_adapter_acceptances() {
    let p = Compiler::default()
        .with_registry(registry())
        .capabilities([Capability::Reply])
        .compile(&[source(
            "on message:\n    announce score 42\n    reply \"builtin\"\n",
        )])
        .unwrap();
    let result = Arc::new(Mutex::new(None));
    let capture = result.clone();
    let mock = Arc::new(Mock::default());
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.outbound(mock);
    bot.on(EventKind::Message, move |ctx| {
        let program = p.clone();
        let capture = capture.clone();
        async move {
            *capture.lock().unwrap() = Some(program.execute(0, &ctx).await.unwrap());
            Ok(())
        }
    })
    .unwrap();
    let (tx, events) = sculk_bot::channel(1).unwrap();
    tx.send(event(1, "input")).await.unwrap();
    drop(tx);
    let (_tx, cancel) = watch::channel(false);
    assert_eq!(bot.run(events, cancel).await.failures, 0);
    let execution = result.lock().unwrap().take().unwrap();
    assert_eq!(execution.replies, 2);
    assert!(execution.steps >= 5);
}

#[test]
fn compiled_calls_retain_only_used_native_implementations() {
    let used = Arc::new(17_i64);
    let unused = Arc::new(99_i64);
    let used_weak = Arc::downgrade(&used);
    let unused_weak = Arc::downgrade(&unused);
    let mut r = Registry::new();
    r.expression(
        Syntax::new("used data").returns(Type::Integer).cost(1),
        move |_, _| Ok(Value::Integer(*used)),
    )
    .unwrap();
    r.expression(
        Syntax::new("unused data").returns(Type::Integer).cost(1),
        move |_, _| Ok(Value::Integer(*unused)),
    )
    .unwrap();
    let compiler = Compiler::default().with_registry(r);
    let program = compiler
        .compile(&[source("on message:\n    reply \"%used data%\"\n")])
        .unwrap();
    drop(compiler);
    assert!(unused_weak.upgrade().is_none());
    assert!(used_weak.upgrade().is_some());
    drop(program);
    assert!(used_weak.upgrade().is_none());
}

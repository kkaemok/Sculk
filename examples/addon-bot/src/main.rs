use sculk_bot::{
    Bot, BoxFuture, Config, Event, EventKind, Message, Outbound, OutboundError, OutboundReceipt,
    PermissionId, RoomId,
};
use sculk_script::{
    addon::{AddonError, Capability, Registry, Syntax},
    Compiler, Source, Type, Value,
};
use std::sync::Arc;
use tokio::sync::watch;
struct ConsoleMock;
impl Outbound for ConsoleMock {
    fn send<'a>(
        &'a self,
        _: RoomId,
        text: &'a str,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<OutboundReceipt, OutboundError>> {
        Box::pin(async move {
            println!("Bot (mock): {text}");
            Ok(OutboundReceipt::MockAccepted)
        })
    }
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("OFFLINE ONLY: statically linked trusted Rust addon; synthetic score and mock reply. No KakaoTalk connection.");
    let score_cap = Capability::Application(PermissionId::new("synthetic.reputation")?);
    let mut registry = Registry::new();
    registry.expression(
        Syntax::new("reputation of %subject%")
            .parameter("subject", Type::Integer)
            .returns(Type::Integer)
            .cost(10)
            .requires(score_cap.clone()),
        |_, args| {
            let subject = args[0].as_integer().ok_or(AddonError::Type)?;
            Ok(Value::Integer(if subject == 2 { 42 } else { 7 }))
        },
    )?;
    registry.condition(
        Syntax::new("trusted score %score%")
            .parameter("score", Type::Integer)
            .cost(3),
        |_, args| Ok(args[0].as_integer().ok_or(AddonError::Type)? >= 40),
    )?;
    registry.effect(
        Syntax::new("announce score %score%")
            .parameter("score", Type::Integer)
            .cost(2)
            .requires(Capability::Reply),
        |ctx, args| async move {
            let score = args[0].as_integer().ok_or(AddonError::Type)?;
            tokio::task::yield_now().await;
            assert_eq!(
                ctx.reply(&format!("Synthetic reputation: {score}")).await?,
                OutboundReceipt::MockAccepted
            );
            Ok(())
        },
    )?;
    registry.event(
        Syntax::new("synthetic report")
            .cost(2)
            .requires(Capability::ReadMessage),
        EventKind::Message,
        |ctx| Ok(ctx.message()? == "profile"),
    )?;
    let compiler = Compiler::default().with_registry(registry).capabilities([
        score_cap,
        Capability::Reply,
        Capability::ReadMessage,
    ]);
    let source = Source {
        filename: "reputation.sculk",
        text: include_str!("../scripts/reputation.sculk"),
    };
    let program = match compiler.compile(&[source]) {
        Ok(p) => p,
        Err(errors) => {
            for error in errors {
                eprintln!("{error}");
            }
            return Err("addon script compilation failed".into());
        }
    };
    let mut bot = Bot::new(Config::default())?;
    bot.outbound(Arc::new(ConsoleMock));
    let bot = program.install_with_errors(bot, Arc::new(|d| eprintln!("{d}")))?;
    let (tx, source) = sculk_bot::channel(1)?;
    let packet = sculk_protocol::Packet::from_document(
        0,
        sculk_protocol::Command::new("MSG")?,
        &bson::doc! {"chatId":1_i64,"chatLog":{"logId":1_i64,"authorId":2_i64,"type":1,"sendAt":1700000000_i64,"message":"profile"}},
    )?;
    println!("User (synthetic): profile");
    tx.send(Event::Message(Message::from_packet(packet)?))
        .await?;
    drop(tx);
    let (_tx, cancel) = watch::channel(false);
    let report = bot.run(source, cancel).await;
    println!(
        "Offline shutdown: {:?}; failures={}; cleanup={:?}",
        report.stop, report.failures, report.shutdown
    );
    if report.failures != 0 || report.shutdown.is_err() {
        return Err("offline addon example failed".into());
    }
    Ok(())
}

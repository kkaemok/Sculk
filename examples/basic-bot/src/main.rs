//! Explicitly synthetic offline bot: never opens a network connection.
use sculk_bot::{
    channel, Bot, BoxFuture, Command, Config, Decision, Event, EventKind, MemoryPermissions,
    Message, Outbound, OutboundError, OutboundReceipt, PermissionId, RoomId, SenderId,
};
use sculk_protocol::{Command as WireCommand, Packet};
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
    println!(
        "OFFLINE ONLY: synthetic local messages; no KakaoTalk connection or server acknowledgment."
    );
    let permission = PermissionId::new("commands.ping")?;
    let mut rules = MemoryPermissions::default();
    rules.set(
        permission.clone(),
        SenderId::new(2)?,
        Some(RoomId::new(1)?),
        Decision::Grant,
    )?;
    let mut bot = Bot::new(Config::default())?;
    bot.outbound(Arc::new(ConsoleMock));
    bot.permission_store(Arc::new(rules));
    bot.permission(permission.clone(), false)?;
    bot.register(
        Command::new("ping").alias("p").require(permission),
        |ctx| async move {
            let receipt = ctx.reply("Pong!").await?;
            assert_eq!(receipt, OutboundReceipt::MockAccepted);
            Ok(())
        },
    )?;
    bot.command("help", |ctx| async move {
        ctx.reply("Available commands: ping, help").await?;
        Ok(())
    })?;
    bot.on(EventKind::Message, |ctx| async move {
        // Content printing is explicit and limited to this synthetic example.
        println!("User (synthetic): {}", ctx.content().unwrap_or(""));
        Ok(())
    })?;
    bot.on_error(|ctx| async move {
        println!("Local dispatch failure: {:?}", ctx.failure());
        Ok(())
    })?;
    let (tx, source) = channel(8)?;
    for (id, text) in [(1_i64, "!ping"), (2, "!help")] {
        let packet = Packet::from_document(
            0,
            WireCommand::new("MSG")?,
            &bson::doc! {"chatId":1_i64,"chatLog":{"logId":id,"authorId":2_i64,"type":1,"sendAt":1700000000_i64,"message":text}},
        )?;
        tx.send(Event::Message(Message::from_packet(packet)?))
            .await?;
    }
    drop(tx); // EOF drains handlers; no background tasks survive.
    let (_cancel_tx, cancel) = watch::channel(false);
    let report = bot.run(source, cancel).await;
    println!(
        "Shutdown: {:?}; {} commands; cleanup {:?}",
        report.stop, report.commands, report.shutdown
    );
    if report.failures != 0 || report.shutdown.is_err() {
        return Err("offline example failed".into());
    }
    Ok(())
}

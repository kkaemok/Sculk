use sculk_bot::{
    Bot, BoxFuture, Config, Event, Message, Outbound, OutboundError, OutboundReceipt, RoomId,
};
use sculk_script::Compiler;
use std::{path::Path, sync::Arc};
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
        "OFFLINE ONLY: synthetic events and mock replies; no KakaoTalk connection or delivery."
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts");
    let program = match Compiler::default()
        .load_files(&[root.join("hello.sculk"), root.join("commands.sc")])
    {
        Ok(p) => p,
        Err(errors) => {
            for error in errors {
                eprintln!("{error}");
            }
            return Err("script compilation failed".into());
        }
    };
    let mut bot = Bot::new(Config::default())?;
    bot.outbound(Arc::new(ConsoleMock));
    bot.on(sculk_bot::EventKind::Message, |ctx| async move {
        println!("User (synthetic): {}", ctx.content().unwrap_or(""));
        Ok(())
    })?;
    let bot = program.install_with_errors(bot, Arc::new(|d| eprintln!("{d}")))?;
    let inputs = [
        "안녕",
        "/ping",
        "!ping",
        ">>greet Sculk",
        "봇아 인사 \"John Smith\"",
    ];
    let (tx, source) = sculk_bot::channel(8)?;
    for (index, text) in inputs.iter().enumerate() {
        let packet = sculk_protocol::Packet::from_document(
            0,
            sculk_protocol::Command::new("MSG")?,
            &bson::doc! {"chatId":1_i64,"chatLog":{"logId":index as i64+1,"authorId":2_i64,"type":1,"sendAt":1700000000_i64,"message":text}},
        )?;
        tx.send(Event::Message(Message::from_packet(packet)?))
            .await?;
    }
    drop(tx);
    let (_tx, cancel) = watch::channel(false);
    let report = bot.run(source, cancel).await;
    println!(
        "Offline shutdown: {:?}, commands={}, failures={}, cleanup={:?}",
        report.stop, report.commands, report.failures, report.shutdown
    );
    if report.failures != 0 || report.shutdown.is_err() {
        return Err("offline bot failed".into());
    }
    Ok(())
}

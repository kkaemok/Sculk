//! Synthetic messages only; exercise the shared native/script prefix engine.
use sculk_bot::*;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;
fn event(id: i64, text: &str) -> Event {
    let p=sculk_protocol::Packet::from_document(0,sculk_protocol::Command::new("MSG").unwrap(),&bson::doc!{"chatId":1_i64,"chatLog":{"logId":id,"authorId":2_i64,"type":1,"sendAt":1_i64,"message":text}}).unwrap();
    Event::Message(Message::from_packet(p).unwrap())
}
#[tokio::test]
async fn compiled_longest_prefix_unicode_aliases_and_shared_arguments() {
    let mut bot = Bot::new(Config {
        case_sensitive: false,
        ..Config::default()
    })
    .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    for (name, prefix) in [("p", "!"), ("p", "!!"), ("인사", "봇아 "), ("ping", ">>")] {
        let calls = calls.clone();
        let prefix = prefix.to_owned();
        bot.register(Command::new(name).prefixes([prefix.clone()]), move |ctx| {
            let calls = calls.clone();
            let prefix = prefix.clone();
            async move {
                calls.lock().unwrap().push((
                    prefix,
                    ctx.arguments().map(str::to_owned).collect::<Vec<_>>(),
                ));
                Ok(())
            }
        })
        .unwrap();
    }
    let (tx, source) = channel(8).unwrap();
    for (i, text) in [
        "!!p \"two words\"",
        "!p",
        "봇아 인사 한글",
        ">>PING",
        "!partial",
    ]
    .iter()
    .enumerate()
    {
        tx.send(event(i as i64 + 1, text)).await.unwrap();
    }
    drop(tx);
    let (_tx, cancel) = watch::channel(false);
    let report = bot.run(source, cancel).await;
    assert_eq!(report.commands, 4);
    assert_eq!(report.first_failure, Some(Failure::UnknownCommand));
    assert_eq!(
        *calls.lock().unwrap(),
        [
            ("!!".into(), vec!["two words".into()]),
            ("!".into(), vec![]),
            ("봇아 ".into(), vec!["한글".into()]),
            (">>".into(), vec![])
        ]
    );
}
#[tokio::test]
async fn prefixless_requires_host_opt_in_and_registration_remains_atomic() {
    let mut bot = Bot::new(Config::default()).unwrap();
    assert_eq!(
        bot.register(Command::new("p").prefixes([""]), |_| async { Ok(()) }),
        Err(RegistrationError::Invalid)
    );
    bot.allow_prefixless_commands(true).unwrap();
    bot.register(Command::new("p").prefixes([""]), |_| async { Ok(()) })
        .unwrap();
    assert_eq!(
        bot.allow_prefixless_commands(false),
        Err(RegistrationError::Invalid)
    );
    assert_eq!(
        bot.register(Command::new("p").prefixes(["!", ""]), |_| async { Ok(()) }),
        Err(RegistrationError::Duplicate)
    );
    bot.command("p", |_| async { Ok(()) }).unwrap();
    assert_eq!(
        bot.register(Command::new("x").prefixes(["!", "!"]), |_| async { Ok(()) }),
        Err(RegistrationError::Duplicate)
    );
    let (tx, source) = channel(2).unwrap();
    tx.send(event(1, "p")).await.unwrap();
    tx.send(event(2, "!p")).await.unwrap();
    drop(tx);
    let (_tx, cancel) = watch::channel(false);
    assert_eq!(bot.run(source, cancel).await.commands, 2);
}

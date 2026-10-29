//! Controlled synthetic benchmark; excludes network and BSON parsing.
use sculk_bot::{Bot, BoxFuture, Config, Event, EventKind, EventSource, SourceError};
use std::{hint::black_box, time::Instant};
use tokio::sync::watch;
struct Repeat {
    left: u64,
    event: Event,
}
impl EventSource for Repeat {
    fn recv(
        &mut self,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'_, Result<Option<Event>, SourceError>> {
        Box::pin(async move {
            if self.left == 0 {
                Ok(None)
            } else {
                self.left -= 1;
                Ok(Some(self.event.clone()))
            }
        })
    }
    fn shutdown(&mut self) -> BoxFuture<'_, Result<(), SourceError>> {
        Box::pin(async { Ok(()) })
    }
}
fn message(id: i64) -> sculk_bot::Message {
    let mut text = String::from("!ping one \"two words\" ");
    text.push_str(&"x".repeat(512 - text.len()));
    let p=sculk_protocol::Packet::from_document(0,sculk_protocol::Command::new("MSG").unwrap(),&bson::doc!{"chatId":1_i64,"chatLog":{"logId":id,"authorId":2_i64,"type":1,"sendAt":1700000000_i64,"message":text}}).unwrap();
    sculk_bot::Message::from_packet(p).unwrap()
}
struct Messages {
    left: u64,
    sequence: usize,
    templates: Vec<sculk_bot::Message>,
}
impl EventSource for Messages {
    fn recv(
        &mut self,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'_, Result<Option<Event>, SourceError>> {
        Box::pin(async move {
            if self.left == 0 {
                Ok(None)
            } else {
                let m = self.templates[self.sequence % self.templates.len()].clone();
                self.sequence += 1;
                self.left -= 1;
                Ok(Some(Event::Message(m)))
            }
        })
    }
    fn shutdown(&mut self) -> BoxFuture<'_, Result<(), SourceError>> {
        Box::pin(async { Ok(()) })
    }
}
fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let input = "!ping one \"two words\" 한글";
    let n = 200000;
    let now = Instant::now();
    for _ in 0..n {
        let parsed = sculk_bot::parse(black_box(input), "!").unwrap().unwrap();
        black_box(parsed.arguments().count());
    }
    println!(
        "synthetic quoted parse ns/op={:.1}",
        now.elapsed().as_nanos() as f64 / n as f64
    );
    rt.block_on(async {
        let (_tx, cancel) = watch::channel(false);
        for cap in [8, 16, 32] {
            // Pre-parse 257 internally consistent synthetic packets outside timing.
            // Cycling exceeds the 256-ID dedup window and exercises bounded eviction.
            let templates = (1..=257).map(message).collect();
            let mut bot = Bot::new(Config {
                event_capacity: cap,
                ..Config::default()
            })
            .unwrap();
            bot.on(EventKind::All, |ctx| async move {
                black_box(ctx.event());
                Ok(())
            })
            .unwrap();
            let now = Instant::now();
            let report = bot
                .run(
                    Repeat {
                        left: n,
                        event: Event::Lagged(1),
                    },
                    cancel.clone(),
                )
                .await;
            assert_eq!(report.completed, n);
            assert_eq!(report.failures, 0);
            println!(
                "synthetic dispatch cap={cap} ns/event={:.1}",
                now.elapsed().as_nanos() as f64 / n as f64
            );
            let mut bot = Bot::new(Config {
                event_capacity: cap,
                ..Config::default()
            })
            .unwrap();
            bot.command("ping", |ctx| async move {
                black_box(ctx.arguments().count());
                Ok(())
            })
            .unwrap();
            let now = Instant::now();
            let report = bot
                .run(
                    Messages {
                        left: n,
                        sequence: 0,
                        templates,
                    },
                    cancel.clone(),
                )
                .await;
            assert_eq!(report.commands, n);
            println!(
                "synthetic command text=512 cap={cap} ns/event={:.1}",
                now.elapsed().as_nanos() as f64 / n as f64
            );
        }
    });
}

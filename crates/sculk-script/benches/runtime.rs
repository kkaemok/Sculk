//! Identical native/script workloads, instrumented allocator, synthetic input.
use sculk_bot::{
    Bot, BoxFuture, Command, Config, Event, EventKind, EventSource, HandlerError, Message,
    Outbound, OutboundError, OutboundReceipt, RoomId, SourceError,
};
use sculk_script::{CompiledScripts, Compiler, Source};
use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};
use std::{
    alloc::System,
    hint::black_box,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;
#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;
const N: usize = 30000;
struct Mock {
    calls: AtomicUsize,
}
impl Outbound for Mock {
    fn send<'a>(
        &'a self,
        _: RoomId,
        text: &'a str,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<OutboundReceipt, OutboundError>> {
        Box::pin(async move {
            black_box(text.len());
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(OutboundReceipt::MockAccepted)
        })
    }
}
struct Messages {
    left: usize,
    index: usize,
    templates: Vec<Message>,
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
                let message = self.templates[self.index % self.templates.len()].clone();
                self.index += 1;
                self.left -= 1;
                Ok(Some(Event::Message(message)))
            }
        })
    }
    fn shutdown(&mut self) -> BoxFuture<'_, Result<(), SourceError>> {
        Box::pin(async { Ok(()) })
    }
}
fn messages(text: &str) -> Messages {
    let templates=(1..=257).map(|id|{
        let p=sculk_protocol::Packet::from_document(0,sculk_protocol::Command::new("MSG").unwrap(),&bson::doc!{"chatId":1_i64,"chatLog":{"logId":id as i64,"authorId":2_i64,"type":1,"sendAt":1700000000_i64,"message":text}}).unwrap();Message::from_packet(p).unwrap()
    }).collect();
    Messages {
        left: N,
        index: 0,
        templates,
    }
}
fn source(case: &str) -> String {
    let text = "x".repeat(512);
    match case{
        "message"=>"on message:\n    reply \"ok\"\n".into(),
        "condition"=>format!("on message:\n    if message is \"{text}\":\n        reply \"ok\"\n"),
        "variable"=>format!("on message:\n    set {{_x}} to message\n    if {{_x}} is \"{text}\":\n        reply {{_x}}\n"),
        "command"=>"command ping:\n    trigger:\n        reply \"ok\"\n".into(),
        "interpolation"=>"command greet <name>:\n    trigger:\n        reply \"Hello, %arg-1%!\"\n".into(),
        _=>unreachable!(),
    }
}
fn program(case: &str) -> CompiledScripts {
    Compiler::default()
        .compile(&[Source {
            filename: "benchmark.sculk",
            text: &source(case),
        }])
        .unwrap()
}
fn bot(script: bool, case: &str, mock: Arc<Mock>) -> Bot {
    let mut bot = Bot::new(Config {
        event_capacity: 16,
        ..Config::default()
    })
    .unwrap();
    bot.outbound(mock);
    if script {
        return program(case).install(bot).unwrap();
    }
    match case {
        "message" => bot
            .on(EventKind::Message, |ctx| async move {
                ctx.reply("ok").await?;
                Ok(())
            })
            .unwrap(),
        "condition" => {
            let expected: Arc<str> = "x".repeat(512).into();
            bot.on(EventKind::Message, move |ctx| {
                let expected = expected.clone();
                async move {
                    if ctx.content() == Some(&expected) {
                        ctx.reply("ok").await?;
                    }
                    Ok(())
                }
            })
            .unwrap();
        }
        "variable" => {
            let expected: Arc<str> = "x".repeat(512).into();
            bot.on(EventKind::Message, move |ctx| {
                let expected = expected.clone();
                async move {
                    let local = ctx.content().unwrap().to_owned();
                    if local.as_str() == expected.as_ref() {
                        ctx.reply(&local).await?;
                    }
                    Ok(())
                }
            })
            .unwrap();
        }
        "command" => bot
            .command("ping", |ctx| async move {
                ctx.reply("ok").await?;
                Ok(())
            })
            .unwrap(),
        "interpolation" => bot
            .register(Command::new("greet"), |ctx| async move {
                let name = ctx.arguments().next().ok_or(HandlerError::Failed)?;
                ctx.reply(&format!("Hello, {name}!")).await?;
                Ok(())
            })
            .unwrap(),
        _ => unreachable!(),
    }
    bot
}
fn rss() -> usize {
    std::fs::read_to_string("/proc/self/smaps_rollup")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Rss:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}
async fn idle(script: bool) {
    let baseline = rss();
    let region = Region::new(GLOBAL);
    let mock = Arc::new(Mock {
        calls: AtomicUsize::new(0),
    });
    let bot = bot(script, "variable", mock);
    let (_source_tx, source) = sculk_bot::channel(16).unwrap();
    let (cancel_tx, cancel) = watch::channel(false);
    let running = bot.run(source, cancel);
    tokio::pin!(running);
    tokio::select! {_=&mut running=>panic!("source should be idle"),_=tokio::time::sleep(Duration::from_millis(250))=>{}}
    let resident = rss();
    let stats = region.change();
    println!("idle script={script} RSS baseline={baseline}KiB loaded={resident}KiB live_heap_delta={}bytes",stats.bytes_allocated as isize-stats.bytes_deallocated as isize);
    cancel_tx.send_replace(true);
    let report = running.await;
    assert_eq!(report.received, 0);
}
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    if let Some(mode) = std::env::args()
        .nth(1)
        .filter(|m| matches!(m.as_str(), "native" | "script"))
    {
        runtime.block_on(idle(mode == "script"));
        return;
    }
    for mode in ["native", "script"] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg(mode)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let text = source("variable");
    let compiler = Compiler::default();
    let region = Region::new(GLOBAL);
    let started = Instant::now();
    for _ in 0..1000 {
        black_box(
            compiler
                .compile(&[Source {
                    filename: "benchmark.sc",
                    text: &text,
                }])
                .unwrap(),
        );
    }
    let elapsed = started.elapsed();
    let allocations = region.change();
    println!(
        "lex+parse+semantic compile source={}bytes us/compile={:.2} allocations/compile={:.1}",
        text.len(),
        elapsed.as_secs_f64() * 1e6 / 1000.0,
        allocations.allocations as f64 / 1000.0
    );
    runtime.block_on(async{
        let(_tx,cancel)=watch::channel(false);
        for case in ["message","condition","variable","command","interpolation"]{
            let input=match case{"command"=>format!("!ping {}","x".repeat(506)),"interpolation"=>format!("!greet \"{}\"","x".repeat(503)),_=>"x".repeat(512)};assert_eq!(input.len(),512);
            for script in [false,true]{
                let mock=Arc::new(Mock{calls:AtomicUsize::new(0)});let bot=bot(script,case,mock.clone());let source=messages(&input);
                let region=Region::new(GLOBAL);let started=Instant::now();let report=bot.run(source,cancel.clone()).await;let elapsed=started.elapsed();let allocations=region.change();
                assert_eq!(report.completed,N as u64);assert_eq!(report.failures,0);assert_eq!(mock.calls.load(Ordering::Relaxed),N);
                println!("case={case} script={script} ns/event={:.1} alloc/event={:.2} realloc/event={:.2} requested_bytes/event={:.1}",elapsed.as_nanos() as f64/N as f64,allocations.allocations as f64/N as f64,allocations.reallocations as f64/N as f64,allocations.bytes_allocated as f64/N as f64);
            }
        }
    });
}

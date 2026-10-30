//! Controlled synthetic native/builtin/addon comparisons. No network or live data.
use sculk_bot::{
    Bot, BoxFuture, Config, Event, EventKind, EventSource, Message, Outbound, OutboundError,
    OutboundReceipt, RoomId, SourceError,
};
use sculk_script::{
    addon::{AddonError, Capability, Registry, Syntax},
    Compiler, Source, Type, Value,
};
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
use tokio::sync::{watch, Semaphore};
#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;
const N: usize = 20000;
const ADDON:&str="on message:\n    set {_score} to reputation of sender\n    if trusted score {_score}:\n        announce score {_score}\n";
const BUILTIN:&str="on message:\n    set {_score} to 7\n    if sender is 2:\n        set {_score} to 42\n    if {_score} is 42:\n        reply \"Score: %{_score}%\"\n";
fn score(id: i64) -> i64 {
    if id == 2 {
        42
    } else {
        7
    }
}
fn trusted(value: i64) -> bool {
    value >= 40
}
fn registry() -> Registry {
    let mut r = Registry::new();
    r.expression(
        Syntax::new("reputation of %id%")
            .parameter("id", Type::Integer)
            .returns(Type::Integer)
            .cost(10),
        |_, args| {
            Ok(Value::Integer(score(
                args[0].as_integer().ok_or(AddonError::Type)?,
            )))
        },
    )
    .unwrap();
    r.condition(
        Syntax::new("trusted score %score%")
            .parameter("score", Type::Integer)
            .cost(3),
        |_, args| Ok(trusted(args[0].as_integer().ok_or(AddonError::Type)?)),
    )
    .unwrap();
    r.effect(
        Syntax::new("announce score %score%")
            .parameter("score", Type::Integer)
            .cost(2)
            .requires(Capability::Reply),
        |ctx, args| async move {
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
fn compiler(mode: &str) -> Compiler {
    if mode == "addon" {
        Compiler::default()
            .with_registry(registry())
            .capabilities([Capability::Reply])
    } else {
        Compiler::default()
    }
}
struct Mock {
    count: AtomicUsize,
    ready: Arc<Semaphore>,
}
impl Outbound for Mock {
    fn send<'a>(
        &'a self,
        _: RoomId,
        text: &'a str,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<OutboundReceipt, OutboundError>> {
        Box::pin(async move {
            assert_eq!(text, "Score: 42");
            tokio::task::yield_now().await;
            self.count.fetch_add(1, Ordering::Relaxed);
            self.ready.add_permits(1);
            Ok(OutboundReceipt::MockAccepted)
        })
    }
}
fn bot(mode: &str, mock: Arc<Mock>) -> Bot {
    let mut bot = Bot::new(Config::default()).unwrap();
    bot.outbound(mock);
    if mode == "native" {
        bot.on(EventKind::Message, |ctx| async move {
            let value = score(ctx.sender().unwrap().get());
            if trusted(value) {
                ctx.reply(&format!("Score: {value}")).await?;
            }
            Ok(())
        })
        .unwrap();
        bot
    } else {
        let text = if mode == "addon" { ADDON } else { BUILTIN };
        compiler(mode)
            .compile(&[Source {
                filename: "bench.sculk",
                text,
            }])
            .unwrap()
            .install(bot)
            .unwrap()
    }
}
struct Messages {
    remaining: usize,
    index: usize,
    templates: Vec<Message>,
    ready: Arc<Semaphore>,
}
impl EventSource for Messages {
    fn recv(
        &mut self,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'_, Result<Option<Event>, SourceError>> {
        Box::pin(async move {
            if self.remaining == 0 {
                return Ok(None);
            }
            self.ready.acquire().await.unwrap().forget();
            self.remaining -= 1;
            let message = self.templates[self.index % self.templates.len()].clone();
            self.index += 1;
            Ok(Some(Event::Message(message)))
        })
    }
    fn shutdown(&mut self) -> BoxFuture<'_, Result<(), SourceError>> {
        Box::pin(async { Ok(()) })
    }
}
fn messages(ready: Arc<Semaphore>) -> Messages {
    let text = "x".repeat(512);
    let templates=(1..=257_i64).map(|id|{let p=sculk_protocol::Packet::from_document(0,sculk_protocol::Command::new("MSG").unwrap(),&bson::doc!{"chatId":1_i64,"chatLog":{"logId":id,"authorId":2_i64,"type":1,"sendAt":1_i64,"message":&text}}).unwrap();Message::from_packet(p).unwrap()}).collect();
    Messages {
        remaining: N,
        index: 0,
        templates,
        ready,
    }
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
async fn idle(mode: &str) {
    let baseline = rss();
    let region = Region::new(GLOBAL);
    let mock = Arc::new(Mock {
        count: AtomicUsize::new(0),
        ready: Arc::new(Semaphore::new(1)),
    });
    let bot = bot(mode, mock);
    let (_tx, source) = sculk_bot::channel(16).unwrap();
    let (cancel_tx, cancel) = watch::channel(false);
    let running = bot.run(source, cancel);
    tokio::pin!(running);
    tokio::select! {_=&mut running=>panic!("idle source ended"),_=tokio::time::sleep(Duration::from_millis(250))=>{}}
    let resident = rss();
    let stats = region.change();
    println!(
        "idle {mode} RSS={baseline}->{resident}KiB live_heap_delta={}bytes",
        stats.bytes_allocated as isize - stats.bytes_deallocated as isize
    );
    cancel_tx.send_replace(true);
    assert_eq!(running.await.received, 0);
}
fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    if let Some(mode) = std::env::args()
        .nth(1)
        .filter(|s| matches!(s.as_str(), "native" | "builtin" | "addon"))
    {
        rt.block_on(idle(&mode));
        return;
    }
    for mode in ["native", "builtin", "addon"] {
        assert!(std::process::Command::new(std::env::current_exe().unwrap())
            .arg(mode)
            .status()
            .unwrap()
            .success());
    }
    let region = Region::new(GLOBAL);
    let started = Instant::now();
    for _ in 0..1000 {
        black_box(registry());
    }
    let elapsed = started.elapsed();
    let stats = region.change();
    println!(
        "register 3 entries us/registry={:.2} allocations/registry={:.1}",
        elapsed.as_secs_f64() * 1e6 / 1000.0,
        stats.allocations as f64 / 1000.0
    );
    for (mode, text) in [("builtin", BUILTIN), ("addon", ADDON)] {
        let compiler = compiler(mode);
        let region = Region::new(GLOBAL);
        let started = Instant::now();
        for _ in 0..1000 {
            black_box(
                compiler
                    .compile(&[Source {
                        filename: "bench.sc",
                        text,
                    }])
                    .unwrap(),
            );
        }
        let elapsed = started.elapsed();
        let stats = region.change();
        println!(
            "compile {mode} source={}bytes us/compile={:.2} allocations/compile={:.1}",
            text.len(),
            elapsed.as_secs_f64() * 1e6 / 1000.0,
            stats.allocations as f64 / 1000.0
        );
    }
    rt.block_on(async{let(_tx,cancel)=watch::channel(false);for mode in ["native","builtin","addon"]{
        let ready=Arc::new(Semaphore::new(1));let mock=Arc::new(Mock{count:AtomicUsize::new(0),ready:ready.clone()});let bot=bot(mode,mock.clone());let source=messages(ready);let region=Region::new(GLOBAL);let started=Instant::now();let report=tokio::time::timeout(Duration::from_secs(30),bot.run(source,cancel.clone())).await.expect("benchmark stalled");let elapsed=started.elapsed();let stats=region.change();assert_eq!(report.completed,N as u64);assert_eq!(report.failures,0);assert_eq!(mock.count.load(Ordering::Relaxed),N);
        println!("runtime {mode} ns/event={:.1} allocations/event={:.2} reallocations/event={:.2} requested_bytes/event={:.1}",elapsed.as_nanos() as f64/N as f64,stats.allocations as f64/N as f64,stats.reallocations as f64/N as f64,stats.bytes_allocated as f64/N as f64);
    }});
    // Lifecycle retention, including allocator requests after dropping compiler and
    // program. RSS may retain freed pages; live requested bytes should balance.
    let region = Region::new(GLOBAL);
    {
        let compiler = compiler("addon");
        let program = compiler
            .compile(&[Source {
                filename: "retention.sc",
                text: ADDON,
            }])
            .unwrap();
        black_box(&program);
    }
    let stats = region.change();
    println!(
        "compiler+program live_heap_delta_after_drop={}bytes",
        stats.bytes_allocated as isize - stats.bytes_deallocated as isize
    );
}

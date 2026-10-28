//! Each case runs in a separate process. RSS includes runtime and the local peer,
//! not just logical packet lengths. No live traffic or target-machine claim.
use futures_util::{SinkExt, StreamExt};
use sculk_client::{messaging::Event, Client, Config};
use sculk_protocol::{Command, LocoCodec, Packet};
use std::time::Instant;
use tokio_util::codec::Framed;
fn memory_kib(key: &str) -> usize {
    std::fs::read_to_string("/proc/self/smaps_rollup")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|line| line.starts_with(key))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}
async fn case(capacity: usize, text_bytes: usize) {
    let baseline_rss = memory_kib("Rss:");
    let (a, b) = tokio::io::duplex(131072);
    let client = Client::from_stream(
        a,
        Config {
            events: capacity,
            ..Config::default()
        },
    )
    .unwrap();
    let mut receiver = client.subscribe();
    let p=Packet::from_document(0,Command::new("MSG").unwrap(),&bson::doc!{"chatId":1_i64,"chatLog":{"logId":2_i64,"authorId":3_i64,"type":1,"sendAt":1,"message":"a".repeat(text_bytes)}}).unwrap();
    let body_len = p.body().len();
    let count = 2048;
    let start = Instant::now();
    let server = tokio::spawn(async move {
        let mut framed = Framed::new(b, LocoCodec::default());
        for _ in 0..count {
            framed.send(p.clone()).await.unwrap();
        }
        let ping = framed.next().await.unwrap().unwrap();
        framed.send(ping).await.unwrap();
        let _ = framed.next().await;
    });
    client
        .request(Packet::from_document(0, Command::new("PING").unwrap(), &bson::doc! {}).unwrap())
        .await
        .unwrap();
    let elapsed = start.elapsed();
    let retained_rss = memory_kib("Rss:");
    let mut retained = 0;
    let mut lagged = 0;
    loop {
        match receiver.try_recv() {
            Ok(sculk_client::Event::UnmatchedPacket(p)) => {
                assert!(matches!(Event::from_packet(p), Event::Message(_)));
                retained += 1;
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => lagged += n,
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(e) => panic!("unexpected {e}"),
        }
    }
    assert_eq!(retained, capacity);
    assert_eq!(retained as u64 + lagged, count);
    println!("capacity={capacity} text_bytes={text_bytes} retained_body_bytes={} lagged={lagged} packets_per_second={:.0} baseline_rss_kib={baseline_rss} retained_rss_kib={retained_rss} after_drain_rss_kib={}",retained*body_len,count as f64/elapsed.as_secs_f64(),memory_kib("Rss:"));
    client.disconnect().await;
    server.await.unwrap();
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.len() == 4 && args[1] == "--case" {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(case(args[2].parse().unwrap(), args[3].parse().unwrap()));
    } else {
        for capacity in [8, 16, 32] {
            for text_bytes in [512, 2048, 16384] {
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--case", &capacity.to_string(), &text_bytes.to_string()])
                    .status()
                    .unwrap();
                assert!(status.success());
            }
        }
    }
}

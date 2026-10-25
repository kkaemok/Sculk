//! Synthetic concurrency comparison; includes framing, BSON validation, channels.
use futures_util::{SinkExt, StreamExt};
use sculk_client::{Client, Config};
use sculk_protocol::{Command, LocoCodec, Packet};
use std::time::Instant;
use tokio_util::codec::Framed;
fn main() {
    for (label, runtime) in [
        (
            "current_thread",
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
        ),
        (
            "multi_thread_2",
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap(),
        ),
    ] {
        runtime.block_on(async {
            let (a, b) = tokio::io::duplex(65536);
            let client = Client::from_stream(a, Config::default()).unwrap();
            let server = tokio::spawn(async move {
                let mut framed = Framed::new(b, LocoCodec::default());
                while let Some(Ok(p)) = framed.next().await {
                    framed.send(p).await.unwrap();
                }
            });
            let start = Instant::now();
            let mut workers = Vec::new();
            for _ in 0..16 {
                let c = client.clone();
                workers.push(tokio::spawn(async move {
                    let p = Packet::from_bytes(
                        0,
                        0,
                        Command::new("PING").unwrap(),
                        vec![5, 0, 0, 0, 0].into(),
                    )
                    .unwrap();
                    for _ in 0..1000 {
                        c.request(p.clone()).await.unwrap();
                    }
                }));
            }
            for worker in workers {
                worker.await.unwrap();
            }
            println!(
                "{label}: 16000 request-response pairs, {:.0} pairs/s, elapsed={:?}",
                16000.0 / start.elapsed().as_secs_f64(),
                start.elapsed()
            );
            client.disconnect().await;
            server.await.unwrap();
        });
    }
}

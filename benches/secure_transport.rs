//! Actual GCM byte stream + packet codec + actor, synthetic in-memory workload.
use futures_util::{SinkExt, StreamExt};
use sculk_client::{records::SecureStream, Client, Config};
use sculk_protocol::{secure::GcmRecords, Command, LocoCodec, Packet};
use std::time::Instant;
use tokio_util::codec::Framed;
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (a, b) = tokio::io::duplex(65536);
        let client = Client::from_stream(
            SecureStream::from_codec(a, GcmRecords::new(&[42; 16])),
            Config::default(),
        )
        .unwrap();
        let server = tokio::spawn(async move {
            let mut framed = Framed::new(
                SecureStream::from_codec(b, GcmRecords::new(&[42; 16])),
                LocoCodec::default(),
            );
            while let Some(Ok(p)) = framed.next().await {
                framed.send(p).await.unwrap();
            }
        });
        let p = Packet::from_document(
            0,
            Command::new("BENCH").unwrap(),
            &bson::doc! {"payload":"x".repeat(256)},
        )
        .unwrap();
        let start = Instant::now();
        let mut workers = Vec::new();
        for _ in 0..16 {
            let c = client.clone();
            let p = p.clone();
            workers.push(tokio::spawn(async move {
                for _ in 0..1000 {
                    c.request(p.clone()).await.unwrap();
                }
            }));
        }
        for worker in workers {
            worker.await.unwrap();
        }
        println!(
            "GCM SecureStream + LOCO actor, 16 workers, 16000 pairs: {:.0} pairs/s, elapsed={:?}",
            16000.0 / start.elapsed().as_secs_f64(),
            start.elapsed()
        );
        client.disconnect().await;
        server.await.unwrap();
    });
}

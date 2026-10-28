//! Bounded event ring workload: actual driver, unique wire packets, stalled receiver.
//! Reports payload retention, not allocator RSS or Oracle Micro measurements.
use futures_util::{SinkExt, StreamExt};
use sculk_client::{messaging::Event, Client, Config};
use sculk_protocol::{Command, LocoCodec, Packet};
use std::time::Instant;
use tokio_util::codec::Framed;
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for capacity in [8,16,64,256] {
            for text_bytes in [1024,65536] {
                let (a,b)=tokio::io::duplex(131072);
                let client=Client::from_stream(a,Config{events:capacity,..Config::default()}).unwrap();
                let mut receiver=client.subscribe();
                let p=Packet::from_document(0,Command::new("MSG").unwrap(),&bson::doc!{
                    "chatId":1_i64,"chatLog":{"logId":2_i64,"authorId":3_i64,"type":1,
                    "sendAt":1,"message":"a".repeat(text_bytes)}}).unwrap();
                let body_len=p.body().len();
                let count=2048;
                let start=Instant::now();
                let server=tokio::spawn(async move {
                    let mut framed=Framed::new(b,LocoCodec::default());
                    for _ in 0..count { framed.send(p.clone()).await.unwrap(); }
                    let ping=framed.next().await.unwrap().unwrap(); framed.send(ping).await.unwrap();
                    let _=framed.next().await;
                });
                client.request(Packet::from_document(0,Command::new("PING").unwrap(),&bson::doc!{}).unwrap()).await.unwrap();
                let elapsed=start.elapsed();
                let mut retained=0;
                let mut lagged=0;
                loop {
                    match receiver.try_recv() {
                        Ok(sculk_client::Event::UnmatchedPacket(p))=>{
                            assert!(matches!(Event::from_packet(p),Event::Message(_)));
                            retained+=1;
                        }
                        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n))=>lagged+=n,
                        Err(tokio::sync::broadcast::error::TryRecvError::Empty)=>break,
                        Err(e)=>panic!("unexpected {e}"),
                    }
                }
                assert_eq!(retained,capacity);
                assert_eq!(retained as u64+lagged,count);
                println!("capacity={capacity} text_bytes={text_bytes} retained={retained} lagged={lagged} retained_body_bytes={} packets_per_second={:.0}",retained*body_len,count as f64/elapsed.as_secs_f64());
                client.disconnect().await;server.await.unwrap();
            }
        }
    });
}

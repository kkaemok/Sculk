//! Reproducible synthetic microbenchmark, no live service traffic.
use bytes::BytesMut;
use sculk_protocol::{Command, LocoCodec, Packet};
use std::{hint::black_box, time::Instant};
use tokio_util::codec::{Decoder, Encoder};
fn main() {
    let p = Packet::from_document(
        1,
        Command::new("MSG").unwrap(),
        &bson::doc! {"text": "x".repeat(256), "room": 42_i64},
    )
    .unwrap();
    let mut codec = LocoCodec::default();
    let mut encoded = BytesMut::new();
    codec.encode(p.clone(), &mut encoded).unwrap();
    let n = 200_000;
    let start = Instant::now();
    for _ in 0..n {
        let mut b = encoded.clone();
        black_box(codec.decode(&mut b).unwrap());
    }
    println!(
        "decode + input clone: {:.1} ns/frame, bytes={}, n={n}",
        start.elapsed().as_nanos() as f64 / n as f64,
        encoded.len()
    );
    let start = Instant::now();
    let mut b = BytesMut::with_capacity(encoded.len());
    for _ in 0..n {
        b.clear();
        codec.encode(black_box(p.clone()), &mut b).unwrap();
        black_box(&b);
    }
    println!(
        "encode + packet clone, reused output: {:.1} ns/frame",
        start.elapsed().as_nanos() as f64 / n as f64
    );
}

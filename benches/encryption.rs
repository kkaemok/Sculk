//! AES-GCM record processing separately from request throughput (no live traffic).
use bytes::{Bytes, BytesMut};
use sculk_protocol::secure::GcmRecords;
use std::{hint::black_box, time::Instant};
use tokio_util::codec::{Decoder, Encoder};
fn main() {
    for size in [308, 16384] {
        let n = 20000;
        let payload = Bytes::from(vec![42; size]);
        let mut codec = GcmRecords::new(&[7; 16]);
        let mut wire = BytesMut::new();
        let start = Instant::now();
        for _ in 0..n {
            wire.clear();
            codec.encode(black_box(payload.clone()), &mut wire).unwrap();
            black_box(&wire);
        }
        println!(
            "GCM encrypt incl OS nonce + record encode {size} bytes: {:.1} ns/record, n={n}",
            start.elapsed().as_nanos() as f64 / n as f64
        );
        let start = Instant::now();
        for _ in 0..n {
            let mut input = wire.clone();
            black_box(codec.decode(&mut input).unwrap());
        }
        println!(
            "GCM decrypt + input clone {size} bytes: {:.1} ns/record, n={n}",
            start.elapsed().as_nanos() as f64 / n as f64
        );
    }
    #[cfg(feature = "loco-handshake")]
    {
        let key = sculk_protocol::handshake::ServerKey::from_pkcs1_der(include_bytes!(
            "../tests/fixtures/documented-server-public.der"
        ))
        .unwrap();
        let n = 1000;
        let start = Instant::now();
        for _ in 0..n {
            black_box(key.initiate().unwrap());
        }
        println!(
            "RSA-OAEP/SHA1 key setup incl entropy: {:.1} ns/handshake, n={n}",
            start.elapsed().as_nanos() as f64 / n as f64
        );
    }
}

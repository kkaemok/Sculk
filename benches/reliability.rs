//! Isolated in-process operations plus local TLS session setup/reconnect. All
//! values are synthetic. No performance-improvement or Oracle target claim.
use bson::doc;
use sculk_client::{
    managed::{Budget, Room},
    messaging::{Message, MessageId},
    read_only::RoomId,
    recovery::{connect_with_retry, AttemptError, RetryPolicy},
    session::{AccessToken, Grant, Session},
    synchronization::{Cursor, MemoryStore, Page, Request, Tracker},
    tls::{TlsSettings, TrustStore},
    Config,
};
use sculk_protocol::{Command, Packet};
use std::{
    hint::black_box,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::watch;
fn live(id: i64, prev: i64, text_bytes: usize) -> Packet {
    Packet::from_document(0,Command::new("MSG").unwrap(),&doc!{"chatId":1_i64,"chatLog":{"chatId":1_i64,"logId":id,"prevId":prev,"authorId":2_i64,"type":1,"message":"a".repeat(text_bytes),"sendAt":1700000000}}).unwrap()
}
async fn session() -> Session {
    use tokio_rustls::{
        rustls::{self, pki_types::PrivatePkcs8KeyDer},
        TlsAcceptor,
    };
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["localhost".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let mut roots = TrustStore::empty();
    roots.add(cert.der().clone()).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der().clone()],
        PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
    )
    .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let settings = TlsSettings::with_roots(roots);
    let (a, b) = tokio::io::duplex(16384);
    let (_tx, cancel) = watch::channel(false);
    let (session, peer) = tokio::join!(
        Session::establish(
            a,
            "localhost",
            &settings,
            Duration::from_secs(1),
            Config {
                events: 8,
                ..Config::default()
            },
            cancel.clone()
        ),
        acceptor.accept(b)
    );
    let mut session = session.unwrap();
    session
        .authenticate_with(
            AccessToken::new(
                "synthetic".into(),
                42,
                tokio::time::Instant::now() + Duration::from_secs(60),
            )
            .unwrap(),
            Duration::from_secs(1),
            cancel,
            |_, _| async {
                Ok(Grant {
                    user_id: 42,
                    valid_for: Duration::from_secs(30),
                })
            },
        )
        .await
        .unwrap();
    // The held peer is dropped after the authenticated session is returned by the
    // connector: reconnect benchmark includes detection and cleanup of this EOF.
    drop(peer);
    session
}
fn main() {
    let count = 100_000;
    let p = live(2, 1, 512);
    let start = Instant::now();
    for _ in 0..count {
        black_box(Message::from_packet(p.clone()).unwrap());
    }
    println!(
        "parse_512b ns_per_message={:.1}",
        start.elapsed().as_nanos() as f64 / count as f64
    );
    let room = RoomId::new(1).unwrap();
    let mut store = MemoryStore::new(room, Cursor::new(1).unwrap());
    let mut tracker = Tracker::new(room, 128, &store).unwrap();
    let req = Request::new(room, Cursor::new(1).unwrap(), MessageId::new(2).unwrap(), 1).unwrap();
    let page=Page::parse(req,Packet::from_document(0,Command::new("SYNCMSG").unwrap(),&doc!{"isOK":true,"chatLogs":[{"chatId":1_i64,"logId":2_i64,"authorId":2_i64,"type":1,"message":"a","sendAt":1}]}).unwrap()).unwrap();
    tracker.begin_page(&page).unwrap();
    tracker.acknowledge_history(&page, 0, &mut store).unwrap();
    tracker.finish_page().unwrap();
    for id in 3..130 {
        tracker
            .acknowledge_live(
                &Message::from_packet(live(id, id - 1, 512)).unwrap(),
                &mut store,
            )
            .unwrap();
    }
    let duplicate = Message::from_packet(live(64, 63, 512)).unwrap();
    let start = Instant::now();
    for _ in 0..count {
        black_box(black_box(&tracker).observe(black_box(&duplicate)).unwrap());
    }
    println!(
        "dedup_128 ns_per_lookup={:.1}",
        start.elapsed().as_nanos() as f64 / count as f64
    );
    let messages: Vec<_> = (130..10130)
        .map(|id| Message::from_packet(live(id, id - 1, 512)).unwrap())
        .collect();
    let start = Instant::now();
    for m in &messages {
        tracker.acknowledge_live(m, &mut store).unwrap();
    }
    println!(
        "state_128 ns_per_ack={:.1}",
        start.elapsed().as_nanos() as f64 / messages.len() as f64
    );
    let logs:Vec<_>=(3..23).map(|id|doc!{"chatId":1_i64,"logId":id as i64,"authorId":2_i64,"type":1,"message":"a".repeat(512),"sendAt":1}).collect();
    let response = Packet::from_document(
        0,
        Command::new("SYNCMSG").unwrap(),
        &doc! {"isOK":true,"chatLogs":logs},
    )
    .unwrap();
    let req = Request::new(
        room,
        Cursor::new(2).unwrap(),
        MessageId::new(22).unwrap(),
        20,
    )
    .unwrap();
    let start = Instant::now();
    for _ in 0..10000 {
        black_box(Page::parse(req, response.clone()).unwrap());
    }
    println!(
        "sync_20x512b ns_per_page={:.1}",
        start.elapsed().as_nanos() as f64 / 10000.0
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (_tx, cancel) = watch::channel(false);
        let start = Instant::now();
        for _ in 0..1000 {
            let result: Result<(), AttemptError<()>> =
                connect_with_retry(RetryPolicy::default(), cancel.clone(), || async { Ok(()) })
                    .await;
            result.unwrap();
        }
        println!(
            "retry_success ns_per_attempt={:.1}",
            start.elapsed().as_nanos() as f64 / 1000.0
        );
        let start = Instant::now();
        for _ in 0..100 {
            let calls = std::cell::Cell::new(0);
            let result: Result<(), AttemptError<()>> = connect_with_retry(
                RetryPolicy {
                    attempts: 2,
                    initial: Duration::from_millis(1),
                    maximum: Duration::from_millis(1),
                },
                cancel.clone(),
                || {
                    calls.set(calls.get() + 1);
                    std::future::ready(if calls.get() == 1 {
                        Err(AttemptError::Transient(()))
                    } else {
                        Ok(())
                    })
                },
            )
            .await;
            result.unwrap();
            assert_eq!(calls.get(), 2);
        }
        println!(
            "retry_transient_with_1ms_jitter us_per_cycle={:.1}",
            start.elapsed().as_micros() as f64 / 100.0
        );
        let store = MemoryStore::new(room, Cursor::new(1).unwrap());
        let mut managed = Room::new(room, 128, Budget::default(), &store).unwrap();
        let start = Instant::now();
        // New manager per iteration so explicit shutdown/intervention rules stay
        // intact. Includes certificate generation and local TLS setup, not RTT.
        for _ in 0..100 {
            managed
                .reconnect(
                    RetryPolicy {
                        attempts: 1,
                        ..RetryPolicy::default()
                    },
                    Duration::from_secs(1),
                    cancel.clone(),
                    || async { Ok(session().await) },
                )
                .await
                .unwrap();
            managed.shutdown().await;
            managed = Room::new(room, 128, Budget::default(), &store).unwrap();
        }
        println!(
            "local_tls_setup_and_cleanup us_per_session={:.1}",
            start.elapsed().as_micros() as f64 / 100.0
        );
    });
}

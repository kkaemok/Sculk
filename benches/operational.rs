//! Linux operational samples in isolated processes, with a native local TLS mock.
//! No Kakao account/endpoints. RSS includes the peer and allocator, CPU is the
//! whole process (CLOCK_PROCESS_CPUTIME_ID), not a target-VM prediction.
#[path = "../crates/sculk-client/tests/support/mod.rs"]
mod support;
use bson::doc;
use futures_util::{SinkExt, StreamExt};
use sculk_client::{
    checkpoint::FileStore,
    messaging::{Event, MessageId},
    read_only::RoomId,
    session::{AccessToken, Grant, Session},
    synchronization::{CheckpointStore, Cursor, MemoryStore, Request, Tracker},
    Config,
};
use sculk_protocol::{Command, LocoCodec, Packet};
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::sync::watch;
use tokio_util::codec::Framed;
fn rss() -> u64 {
    fs::read_to_string("/proc/self/smaps_rollup")
        .unwrap()
        .lines()
        .find(|s| s.starts_with("Rss:"))
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}
fn cpu_ns() -> u64 {
    let time = rustix::time::clock_gettime(rustix::time::ClockId::ProcessCPUTime);
    u64::try_from(time.tv_sec).unwrap() * 1_000_000_000 + u64::try_from(time.tv_nsec).unwrap()
}
fn packet(name: &str, body: bson::Document) -> Packet {
    Packet::from_document(0, Command::new(name).unwrap(), &body).unwrap()
}
async fn session() -> (
    Session,
    Framed<tokio_rustls::server::TlsStream<tokio::io::DuplexStream>, LocoCodec>,
) {
    let (tls, acceptor) = support::tls_pair();
    let (a, b) = tokio::io::duplex(32768);
    let (_tx, cancel) = watch::channel(false);
    let (s, peer) = tokio::join!(
        Session::establish(
            a,
            "localhost",
            &tls,
            Duration::from_secs(2),
            Config {
                events: 8,
                ..Config::default()
            },
            cancel.clone()
        ),
        acceptor.accept(b)
    );
    let mut s = s.unwrap();
    s.authenticate_with(
        AccessToken::new(
            "synthetic".into(),
            42,
            tokio::time::Instant::now() + Duration::from_secs(120),
        )
        .unwrap(),
        Duration::from_secs(1),
        cancel,
        |_, _| async {
            Ok(Grant {
                user_id: 42,
                valid_for: Duration::from_secs(60),
            })
        },
    )
    .await
    .unwrap();
    (s, Framed::new(peer.unwrap(), LocoCodec::default()))
}
async fn network_case(name: &str) {
    let (mut session, mut peer) = session().await;
    let (_tx, cancel) = watch::channel(false);
    let baseline_rss = rss();
    if name == "idle" || name == "traffic" {
        let mut events = session.message_events().unwrap();
        let start_cpu = cpu_ns();
        let start = Instant::now();
        if name == "idle" {
            tokio::time::sleep(Duration::from_secs(5)).await;
        } else {
            for i in 1..=5 {
                tokio::time::sleep(Duration::from_secs(1)).await;
                peer.send(packet("MSG",doc!{"chatId":1_i64,"chatLog":{"logId":i as i64,"authorId":2_i64,"sendAt":1,"type":1,"message":"a".repeat(512)}})).await.unwrap();
                assert!(matches!(
                    events.recv(cancel.clone()).await.unwrap(),
                    Event::Message(_)
                ));
            }
        }
        let elapsed = start.elapsed().as_secs_f64();
        let cpu_ms = (cpu_ns() - start_cpu) as f64 / 1e6;
        println!("case={name} wall_seconds={elapsed:.3} process_cpu_ms={cpu_ms:.3} cpu_one_core_pct={:.5} baseline_rss_kib={baseline_rss} final_rss_kib={}",cpu_ms/(elapsed*10.0),rss());
    } else {
        let room = RoomId::new(1).unwrap();
        let logs:Vec<_>=(1..=100).map(|id|doc!{"chatId":1_i64,"logId":id as i64,"authorId":2_i64,"sendAt":1,"type":1,"message":"a".repeat(2048)}).collect();
        let response = packet("SYNCMSG", doc! {"isOK":true,"chatLogs":logs});
        let body_bytes = response.body().len();
        let request =
            Request::new(room, Cursor::default(), MessageId::new(100).unwrap(), 100).unwrap();
        let (page, ()) = tokio::join!(session.synchronize(request, cancel), async {
            let query = peer.next().await.unwrap().unwrap();
            let mut response = response;
            response.id = query.id;
            peer.send(response).await.unwrap();
        });
        let page = page.unwrap();
        let page_rss = rss();
        let mut store = MemoryStore::new(room, Cursor::default());
        let mut tracker = Tracker::new(room, 128, &store).unwrap();
        tracker.begin_page(&page).unwrap();
        let active_rss = rss();
        for i in 0..page.messages().len() {
            tracker.acknowledge_history(&page, i, &mut store).unwrap();
        }
        tracker.finish_page().unwrap();
        drop(page);
        println!("case=sync_100x2048 body_bytes={body_bytes} baseline_rss_kib={baseline_rss} page_rss_kib={page_rss} active_page_rss_kib={active_rss} after_ack_rss_kib={}",rss());
    }
    session.disconnect_checked().await.unwrap();
    assert!(peer.next().await.is_none());
}
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn checkpoint() {
    let path = std::env::temp_dir().join(format!("sculk-io-sample-{}", std::process::id()));
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    let directory = Directory(path);
    let room = RoomId::new(1).unwrap();
    let mut store = FileStore::open(&directory.0, room).unwrap();
    store
        .commit(room, Cursor::default(), Cursor::new(1).unwrap())
        .unwrap();
    let before_rss = rss();
    let cpu = cpu_ns();
    let start = Instant::now();
    for id in 2..102 {
        store
            .commit(room, Cursor::new(id - 1).unwrap(), Cursor::new(id).unwrap())
            .unwrap();
    }
    println!("case=checkpoint_sha256_fsync_rename count=100 wall_us_per_commit={:.1} process_cpu_us_per_commit={:.1} baseline_rss_kib={before_rss} final_rss_kib={}",start.elapsed().as_secs_f64()*1e6/100.0,(cpu_ns()-cpu)as f64/1e3/100.0,rss());
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.len() == 3 && args[1] == "--case" {
        if args[2] == "checkpoint" {
            checkpoint()
        } else {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(network_case(&args[2]));
        }
    } else {
        for name in ["idle", "traffic", "sync", "checkpoint"] {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--case", name])
                .status()
                .unwrap()
                .success());
        }
    }
}

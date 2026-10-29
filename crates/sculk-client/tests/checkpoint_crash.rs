#![cfg(all(feature = "checkpoint-file", unix))]
use sculk_client::{
    checkpoint::FileStore,
    messaging::MessageId,
    read_only::RoomId,
    synchronization::{CheckpointStore, Cursor, Page, Request, Tracker},
};
use sculk_protocol::{Command, Packet};
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command as Process,
};
const ROOM: i64 = 9_007_199_254_740_993;
const OLD: i64 = 9_007_199_254_741_000;
const NEW: i64 = 9_007_199_254_741_080;
fn cursor(n: i64) -> Cursor {
    Cursor::new(n).unwrap()
}
fn room() -> RoomId {
    RoomId::new(ROOM).unwrap()
}
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn directory(name: &str) -> Directory {
    let path = std::env::temp_dir().join(format!("sculk-crash-{}-{name}", std::process::id()));
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    Directory(path)
}
fn page() -> Page {
    let bytes = include_bytes!("../../../tests/fixtures/node-syncmsg-synthetic.bin");
    let source = Packet::from_bytes(
        0,
        0,
        Command::new("SYNCMSG").unwrap(),
        bytes[22..].to_vec().into(),
    )
    .unwrap();
    let mut doc = source.document().unwrap();
    let logs = doc.get_array_mut("chatLogs").unwrap();
    logs.truncate(1);
    let log = logs[0].as_document_mut().unwrap();
    log.insert("logId", NEW);
    log.insert("prevId", OLD);
    Page::parse(
        Request::new(room(), cursor(OLD), MessageId::new(NEW).unwrap(), 1).unwrap(),
        Packet::from_document(0, Command::new("SYNCMSG").unwrap(), &doc).unwrap(),
    )
    .unwrap()
}
fn marker(path: &Path) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path.join("processed"))
        .unwrap();
    file.write_all(b"synthetic application side effect\n")
        .unwrap();
    file.sync_all().unwrap();
}
fn crash_worker() {
    let Ok(mode) = std::env::var("SCULK_TEST_CRASH_MODE") else {
        return;
    };
    let path = PathBuf::from(std::env::var_os("SCULK_TEST_CRASH_DIRECTORY").unwrap());
    let mut store = FileStore::open(&path, room()).unwrap();
    let mut tracker = Tracker::new(room(), 8, &store).unwrap();
    let page = page();
    tracker.begin_page(&page).unwrap();
    match mode.as_str() {
        "before-processing" => {}
        "after-processing" => marker(&path),
        "orphan-temporary" => {
            // A valid staged record is produced by the real writer in another
            // directory. This simulates a complete orphan before rename, rather
            // than pretending to inject a power-loss failure into production I/O.
            let staging = path.join("staging");
            fs::DirBuilder::new().mode(0o700).create(&staging).unwrap();
            let mut staged = FileStore::open(&staging, room()).unwrap();
            staged
                .commit(room(), Cursor::default(), cursor(NEW))
                .unwrap();
            let bytes = fs::read(staging.join("checkpoint")).unwrap();
            let mut next = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path.join("checkpoint.next"))
                .unwrap();
            next.write_all(&bytes).unwrap();
            next.sync_all().unwrap();
        }
        "after-acknowledgment" => {
            marker(&path);
            tracker.acknowledge_history(&page, 0, &mut store).unwrap();
        }
        _ => panic!("unknown synthetic crash mode"),
    }
    // Bypasses Rust destructors, leaving the exclusive store lock behind.
    // This is process-crash/restart evidence, not disk-power-loss simulation.
    std::process::exit(77);
}
#[test]
fn process_restart_replays_unacknowledged_work_and_never_adopts_an_orphan_checkpoint() {
    if std::env::var_os("SCULK_TEST_CRASH_MODE").is_some() {
        crash_worker();
        unreachable!("worker exits abruptly");
    }
    for mode in [
        "before-processing",
        "after-processing",
        "orphan-temporary",
        "after-acknowledgment",
    ] {
        let path = directory(mode);
        let mut initial = FileStore::open(&path.0, room()).unwrap();
        initial
            .commit(room(), Cursor::default(), cursor(OLD))
            .unwrap();
        drop(initial);
        let child = Process::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_restart_replays_unacknowledged_work_and_never_adopts_an_orphan_checkpoint",
                "--nocapture",
            ])
            .env("SCULK_TEST_CRASH_MODE", mode)
            .env("SCULK_TEST_CRASH_DIRECTORY", &path.0)
            .output()
            .unwrap();
        assert_eq!(child.status.code(), Some(77));
        assert!(FileStore::open(&path.0, room()).is_err());
        // Explicit operator action ONLY after the child was reaped; production
        // never automatically takes over a stale lock or uncommitted temp file.
        fs::remove_file(path.0.join("checkpoint.lock")).unwrap();
        let mut store = FileStore::open(&path.0, room()).unwrap();
        let expected = if mode == "after-acknowledgment" {
            NEW
        } else {
            OLD
        };
        assert_eq!(store.load(room()).unwrap(), cursor(expected));
        assert_eq!(
            path.0.join("processed").exists(),
            matches!(mode, "after-processing" | "after-acknowledgment")
        );
        if mode == "orphan-temporary" {
            assert!(store.commit(room(), cursor(OLD), cursor(NEW)).is_err());
            fs::remove_file(path.0.join("checkpoint.next")).unwrap();
        }
        if expected == OLD {
            let mut tracker = Tracker::new(room(), 8, &store).unwrap();
            let p = page();
            tracker.begin_page(&p).unwrap();
            // After-processing crash intentionally replays a possible duplicate;
            // after-ack cursor is already durable. No exactly-once claim.
            tracker.acknowledge_history(&p, 0, &mut store).unwrap();
            assert_eq!(store.load(room()).unwrap(), cursor(NEW));
        }
    }
}

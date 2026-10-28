#![cfg(all(feature = "checkpoint-file", unix))]
use sculk_client::{
    checkpoint::FileStore,
    read_only::RoomId,
    synchronization::{CheckpointStore, Cursor},
};
use std::{
    fs,
    os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "sculk-checkpoint-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn private_atomic_checkpoint_reopens_exact_large_ids_and_excludes_duplicate_writers() {
    let directory = Directory::new();
    let room = RoomId::new(9_007_199_254_740_993).unwrap();
    let next = Cursor::new(9_007_199_254_740_999).unwrap();
    let mut store = FileStore::open(&directory.0, room).unwrap();
    assert_eq!(store.load(room).unwrap(), Cursor::default());
    assert!(FileStore::open(&directory.0, room).is_err());
    store.commit(room, Cursor::default(), next).unwrap();
    assert_eq!(
        fs::metadata(directory.0.join("checkpoint"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(store.commit(room, Cursor::default(), next).is_err());
    assert!(store.commit(room, next, Cursor::default()).is_err());
    drop(store);
    let store = FileStore::open(&directory.0, room).unwrap();
    assert_eq!(store.load(room).unwrap(), next);
    drop(store);
    assert!(FileStore::open(&directory.0, RoomId::new(42).unwrap()).is_err());
    assert!(!directory.0.join("checkpoint.lock").exists());
}
#[test]
fn unsafe_corrupt_symlink_and_crash_stale_files_are_not_replaced() {
    let directory = Directory::new();
    let room = RoomId::new(42).unwrap();
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(FileStore::open(&directory.0, room).is_err());
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(directory.0.join("checkpoint.lock"), "stale").unwrap();
    assert!(FileStore::open(&directory.0, room).is_err());
    fs::remove_file(directory.0.join("checkpoint.lock")).unwrap();
    fs::write(directory.0.join("checkpoint"), "corrupt").unwrap();
    fs::set_permissions(
        directory.0.join("checkpoint"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(FileStore::open(&directory.0, room).is_err());
    assert_eq!(
        fs::read_to_string(directory.0.join("checkpoint")).unwrap(),
        "corrupt"
    );
    fs::remove_file(directory.0.join("checkpoint")).unwrap();
    symlink("checkpoint.lock", directory.0.join("checkpoint")).unwrap();
    assert!(FileStore::open(&directory.0, room).is_err());
    fs::remove_file(directory.0.join("checkpoint")).unwrap();
    let mut store = FileStore::open(&directory.0, room).unwrap();
    fs::write(directory.0.join("checkpoint.next"), "crash-stale").unwrap();
    assert!(store
        .commit(room, Cursor::default(), Cursor::new(1).unwrap())
        .is_err());
    assert_eq!(store.load(room).unwrap(), Cursor::default());
}

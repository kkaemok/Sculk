use crate::{storage::Storage, Result};
use sculk_bot::RoomId;
use sculk_client::{
    checkpoint::FileStore,
    synchronization::{CheckpointStore, Cursor, MemoryStore},
};
use sculk_script::GlobalCounters;
use std::{
    fs::DirBuilder,
    io::ErrorKind,
    os::unix::fs::DirBuilderExt,
    path::Path,
    sync::{Arc, Mutex},
};

struct Disk {
    checkpoint: FileStore,
    attempts: Option<FileStore>,
    storage: Option<Storage>,
}

/// A single runner stages commits through the Tracker's existing store interface.
/// The disk CAS must succeed before the runner continues or reports a commit.
pub(super) struct State {
    room: RoomId,
    pub staged: MemoryStore,
    pub committed: Cursor,
    pub attempted: Cursor,
    pub initialized: bool,
    pub counters: Arc<GlobalCounters>,
    disk: Option<Arc<Mutex<Disk>>>,
}

fn create_directory(path: &Path) -> Result<()> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(_) => Err("cannot create private live state directory".into()),
    }
}

impl State {
    pub fn open(
        room: RoomId,
        path: &Path,
        writes: bool,
        storage_path: Option<&Path>,
    ) -> Result<Self> {
        let path = std::path::absolute(path)?;
        create_directory(&path)?;
        let checkpoint = FileStore::open(&path, room)?;
        let committed = checkpoint.load(room)?;
        // A persisted zero is a real empty-room anchor. Only an entirely
        // absent checkpoint may bootstrap past earlier history.
        let initialized = match path.join("checkpoint").symlink_metadata() {
            Ok(_) => true,
            Err(error) if error.kind() == ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };

        let attempt_path = path.join("write-attempts");
        let attempts = if writes || attempt_path.symlink_metadata().is_ok() {
            create_directory(&attempt_path)?;
            Some(FileStore::open(&attempt_path, room)?)
        } else {
            None
        };
        let attempted = attempts
            .as_ref()
            .map(|store| store.load(room))
            .transpose()?
            .unwrap_or_default();

        let (storage, counters) = match storage_path {
            Some(path) => {
                let (storage, counters) = Storage::open(path)?;
                (Some(storage), Arc::new(counters))
            }
            None => (None, Arc::default()),
        };
        Ok(Self {
            room,
            staged: MemoryStore::new(room, committed),
            committed,
            attempted,
            initialized,
            counters,
            disk: Some(Arc::new(Mutex::new(Disk {
                checkpoint,
                attempts,
                storage,
            }))),
        })
    }

    pub fn probe(room: RoomId) -> Self {
        Self {
            room,
            staged: MemoryStore::new(room, Cursor::default()),
            committed: Cursor::default(),
            attempted: Cursor::default(),
            initialized: false,
            counters: Arc::default(),
            disk: None,
        }
    }

    /// Record the upper bound of live messages allowed to attempt a WRITE
    /// before handing the event to script code. No message body is persisted.
    pub async fn fence(&mut self, next: Cursor) -> Result<()> {
        if next.get() <= self.attempted.get() {
            return Err("write attempt already fenced".into());
        }
        if let Some(disk) = self.disk.clone() {
            let (room, expected) = (self.room, self.attempted);
            tokio::task::spawn_blocking(move || {
                let mut disk = disk.lock().map_err(|_| "private state lock failed")?;
                let store = disk.attempts.as_mut().ok_or("write fence unavailable")?;
                store
                    .commit(room, expected, next)
                    .map_err(|_| "write fence commit failed")
            })
            .await??;
        }
        self.attempted = next;
        Ok(())
    }

    /// Persist script counters before publishing the processed cursor. A failed
    /// commit stops dispatch. Recovery treats historical messages as read-only.
    pub async fn flush(&mut self, save_counters: bool) -> Result<()> {
        let next = self.staged.load(self.room)?;
        if let Some(disk) = self.disk.clone() {
            let (room, expected) = (self.room, self.committed);
            let initialize = !self.initialized;
            let counters = self.counters.clone();
            tokio::task::spawn_blocking(move || {
                let mut disk = disk.lock().map_err(|_| "private state lock failed")?;
                if save_counters {
                    if let Some(storage) = &mut disk.storage {
                        storage
                            .save(&counters)
                            .map_err(|_| "live counter commit failed")?;
                    }
                }
                if initialize || next != expected {
                    disk.checkpoint
                        .commit(room, expected, next)
                        .map_err(|_| "live checkpoint commit failed")?;
                }
                Ok::<(), &'static str>(())
            })
            .await??;
        }
        self.committed = next;
        self.initialized = true;
        Ok(())
    }
}

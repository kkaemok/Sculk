//! Optional Linux/Unix single-room file checkpoint. No database or background task.
//! Use a trusted, owner-private directory. Disk I/O is synchronous: run at startup
//! or on a bounded blocking worker. Crash-stale lock/temp files require operator
//! inspection; they are never removed automatically on open.
#[cfg(unix)]
mod unix {
    use crate::{
        read_only::RoomId,
        synchronization::{CheckpointStore, Cursor, StoreError},
    };
    use sha2::{Digest, Sha256};
    use std::{
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        os::unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
        path::{Path, PathBuf},
    };
    /// Exclusive single-process store, with atomic rename and directory fsync.
    /// Only room/cursor are stored. No credentials or message bodies are persisted.
    pub struct FileStore {
        directory: PathBuf,
        room: RoomId,
        cursor: Cursor,
        _lock: File,
        poisoned: bool,
    }
    impl FileStore {
        /// Directory must already exist, be absolute, non-symlink, owned by the
        /// current uid and have no group/other permission bits. Existing stores
        /// and lock/temp files are never silently replaced on open.
        pub fn open(directory: &Path, room: RoomId) -> Result<Self, StoreError> {
            let metadata = fs::symlink_metadata(directory).map_err(|_| StoreError)?;
            if !directory.is_absolute()
                || directory.as_os_str().as_bytes().is_empty()
                || !metadata.is_dir()
                || metadata.uid() != rustix::process::getuid().as_raw()
                || metadata.mode() & 0o077 != 0
            {
                return Err(StoreError);
            }
            let lock = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(directory.join("checkpoint.lock"))
                .map_err(|_| StoreError)?;
            let mut store = Self {
                directory: directory.to_owned(),
                room,
                cursor: Cursor::default(),
                _lock: lock,
                poisoned: false,
            };
            store.cursor = store.read()?;
            Ok(store)
        }
        fn read(&self) -> Result<Cursor, StoreError> {
            let file = match OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
                .open(self.directory.join("checkpoint"))
            {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Cursor::default()),
                Err(_) => return Err(StoreError),
            };
            let metadata = file.metadata().map_err(|_| StoreError)?;
            if !metadata.is_file()
                || metadata.uid() != rustix::process::getuid().as_raw()
                || metadata.mode() & 0o077 != 0
                || metadata.nlink() != 1
                || metadata.len() > 128
            {
                return Err(StoreError);
            }
            let mut bytes = Vec::new();
            file.take(129)
                .read_to_end(&mut bytes)
                .map_err(|_| StoreError)?;
            if bytes.len() > 128 {
                return Err(StoreError);
            }
            let text = std::str::from_utf8(&bytes).map_err(|_| StoreError)?;
            let mut lines = text.lines();
            if lines.next() != Some("SCULK-CHECKPOINT-2") {
                return Err(StoreError);
            }
            let room = lines
                .next()
                .ok_or(StoreError)?
                .parse::<i64>()
                .map_err(|_| StoreError)?;
            let cursor = Cursor::new(
                lines
                    .next()
                    .ok_or(StoreError)?
                    .parse::<i64>()
                    .map_err(|_| StoreError)?,
            )
            .map_err(|_| StoreError)?;
            let checksum = lines.next().ok_or(StoreError)?;
            let payload = format!("SCULK-CHECKPOINT-2\n{room}\n{}\n", cursor.get());
            let expected = format!("{payload}{:x}\n", Sha256::digest(payload.as_bytes()));
            if room != self.room.get()
                || lines.next().is_some()
                || checksum.len() != 64
                || text != expected
            {
                return Err(StoreError);
            }
            Ok(cursor)
        }
    }
    impl CheckpointStore for FileStore {
        fn load(&self, room: RoomId) -> Result<Cursor, StoreError> {
            if self.poisoned || room != self.room {
                return Err(StoreError);
            }
            let actual = self.read()?;
            if actual != self.cursor {
                return Err(StoreError);
            }
            Ok(actual)
        }
        fn commit(
            &mut self,
            room: RoomId,
            expected: Cursor,
            next: Cursor,
        ) -> Result<(), StoreError> {
            if self.load(room)? != expected || next.get() < expected.get() {
                return Err(StoreError);
            }
            let temporary = self.directory.join("checkpoint.next");
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&temporary)
                .map_err(|_| StoreError)?;
            let mut renamed = false;
            let result: Result<(), StoreError> = (|| {
                let payload = format!("SCULK-CHECKPOINT-2\n{}\n{}\n", room.get(), next.get());
                writeln!(file, "{payload}{:x}", Sha256::digest(payload.as_bytes()))
                    .map_err(|_| StoreError)?;
                file.sync_all().map_err(|_| StoreError)?;
                fs::rename(&temporary, self.directory.join("checkpoint"))
                    .map_err(|_| StoreError)?;
                renamed = true;
                File::open(&self.directory)
                    .and_then(|f| f.sync_all())
                    .map_err(|_| StoreError)?;
                Ok(())
            })();
            if result.is_err() {
                if renamed {
                    self.poisoned = true
                } else {
                    let _ = fs::remove_file(&temporary);
                }
                return Err(StoreError);
            }
            self.cursor = next;
            Ok(())
        }
    }
    impl Drop for FileStore {
        fn drop(&mut self) {
            let _ = fs::remove_file(self.directory.join("checkpoint.lock"));
        }
    }
}
#[cfg(unix)]
pub use unix::FileStore;

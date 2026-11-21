use sculk_script::GlobalCounters;
use std::path::Path;

#[cfg(unix)]
pub use unix::Storage;

#[cfg(not(unix))]
pub struct Storage;

#[cfg(not(unix))]
impl Storage {
    pub fn open(_: &Path) -> crate::Result<(Self, GlobalCounters)> {
        Err("private SQLite storage is supported on Unix".into())
    }
    pub fn save(&mut self, _: &GlobalCounters) -> crate::Result<()> {
        Err("private SQLite storage is supported on Unix".into())
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use rusqlite::{params, Connection, OpenFlags};
    use std::{
        fs::{DirBuilder, File, OpenOptions},
        os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        time::Duration,
    };

    const COUNTERS_SCHEMA: &str =
        "CREATE TABLE counters (name TEXT PRIMARY KEY NOT NULL, value INTEGER NOT NULL) STRICT";

    pub struct Storage {
        connection: Connection,
        prior: Vec<(String, i64)>,
        _lock: File,
    }

    fn private_file(path: &Path) -> crate::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(path)
            .map_err(|_| "cannot open a private state file")?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
            || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(
                "state files require a private regular file owned by the current user".into(),
            );
        }
        Ok(file)
    }

    impl Storage {
        pub fn open(path: &Path) -> crate::Result<(Self, GlobalCounters)> {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .ok_or("state path requires a private parent directory")?;
            match DirBuilder::new().mode(0o700).create(parent) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err("cannot create the state directory".into()),
            }
            let metadata = std::fs::symlink_metadata(parent)?;
            if !metadata.is_dir()
                || metadata.mode() & 0o077 != 0
                || metadata.uid() != rustix::process::geteuid().as_raw()
            {
                return Err("state directory must be private and owned by the current user".into());
            }
            let mut lock_path = path.as_os_str().to_owned();
            lock_path.push(".lock");
            let lock = private_file(Path::new(&lock_path))?;
            rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                .map_err(|_| "state database is already in use")?;
            let _database = private_file(path)?;
            let mut connection = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .map_err(|_| "cannot open the SQLite state database")?;
            connection.busy_timeout(Duration::from_millis(250))?;
            // A bounded page count limits both integrity checks and data growth.
            let pages =
                connection.query_row("PRAGMA page_count", [], |row| row.get::<_, u32>(0))?;
            if pages > 256 {
                return Err("state database exceeds its page limit".into());
            }
            connection.pragma_update(None, "max_page_count", 256)?;
            connection.pragma_update(None, "trusted_schema", false)?;
            connection.pragma_update(None, "synchronous", "FULL")?;
            let check: String =
                connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
            if check != "ok" {
                return Err("state database failed integrity check".into());
            }
            let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            match version {
                0 => {
                    let existing: u32 =
                        connection.query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| {
                            row.get(0)
                        })?;
                    if existing != 0 {
                        return Err("unrecognized unversioned state database".into());
                    }
                    let tx = connection.transaction()?;
                    tx.execute(COUNTERS_SCHEMA, [])?;
                    tx.pragma_update(None, "user_version", 1)?;
                    tx.commit()?;
                }
                1 => {}
                _ => return Err("unsupported state database version".into()),
            }
            let unexpected: u32 = connection.query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE sql IS NOT NULL AND
                 NOT (type = 'table' AND name = 'counters' AND sql = ?1)",
                [COUNTERS_SCHEMA],
                |row| row.get(0),
            )?;
            if unexpected != 0 {
                return Err("unexpected objects in the state database schema".into());
            }
            let counters = {
                let mut stmt = connection
                    .prepare("SELECT name, value FROM counters ORDER BY name LIMIT 257")?;
                let entries = stmt
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<(String, i64)>>>()?;
                GlobalCounters::from_snapshot(entries)?
            };
            let prior = counters.snapshot()?;
            Ok((
                Self {
                    connection,
                    prior,
                    _lock: lock,
                },
                counters,
            ))
        }

        pub fn save(&mut self, counters: &GlobalCounters) -> crate::Result<()> {
            let snapshot = counters.snapshot()?;
            if snapshot == self.prior {
                return Ok(());
            }
            let tx = self.connection.transaction()?;
            tx.execute("DELETE FROM counters", [])?;
            {
                let mut insert =
                    tx.prepare("INSERT INTO counters (name, value) VALUES (?1, ?2)")?;
                for (name, value) in &snapshot {
                    insert.execute(params![name, value])?;
                }
            }
            tx.commit()?;
            self.prior = snapshot;
            Ok(())
        }
    }
}

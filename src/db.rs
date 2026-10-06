//! SQLite access through a single-threaded actor.
//!
//! rusqlite is blocking, so each database lives on its own OS thread and async
//! code sends it closures. Sol keeps every world's data this way, and a
//! world's app can use the same actor for its local cache. The actor is
//! generic, so it can own something richer than a bare `Connection` (for
//! example Geode's stores).

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    thread,
    time::Duration,
};

use anyhow::{Context, anyhow};
use rusqlite::Connection;
use rusqlite_migration::Migrations;
use tokio::sync::{mpsc, oneshot};

type Job<T> = Box<dyn FnOnce(&mut T) + Send>;

/// Owns a value on one dedicated thread; clones share that thread.
pub struct Actor<T> {
    tx: mpsc::UnboundedSender<Job<T>>,
}

impl<T> Clone for Actor<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

impl<T: 'static> Actor<T> {
    /// Starts the thread and waits until `init` has built the value.
    pub fn spawn(
        name: &str,
        init: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Job<T>>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        thread::Builder::new()
            .name(format!("{name}-db"))
            .spawn(move || {
                let mut value = match init() {
                    Ok(value) => {
                        let _ = ready_tx.send(Ok(()));
                        value
                    }
                    Err(err) => {
                        let _ = ready_tx.send(Err(err));
                        return;
                    }
                };
                while let Some(job) = rx.blocking_recv() {
                    // A panicking job drops its reply sender, so the caller sees an
                    // error, and the thread keeps serving everyone else.
                    let _ = catch_unwind(AssertUnwindSafe(|| job(&mut value)));
                }
            })?;
        ready_rx
            .recv()
            .context("database thread exited during start-up")??;
        Ok(Self { tx })
    }

    /// Runs `f` on the actor's thread and returns its result.
    pub async fn call<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut T) -> R + Send + 'static,
    ) -> anyhow::Result<R> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Box::new(move |value| {
                let _ = reply.send(f(value));
            }))
            .map_err(|_| anyhow!("database thread has stopped"))?;
        rx.await.map_err(|_| anyhow!("database call panicked"))
    }
}

/// Opens (or creates) a database file with Sol's pragmas and runs migrations.
pub fn open(path: &Path, migrations: &Migrations) -> anyhow::Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    prepare(conn, migrations)
}

/// Like [`open`], but first copies an existing database into `backups` when
/// its migrations are about to change it, so an update can be undone.
pub fn open_with_backup(
    path: &Path,
    migrations: &Migrations,
    backups: &Path,
) -> anyhow::Result<Connection> {
    if path.exists() {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        let at: usize = migrations.current_version(&conn)?.into();
        if at > 0 && migrations.pending_migrations(&conn)? > 0 {
            std::fs::create_dir_all(backups)
                .with_context(|| format!("creating {}", backups.display()))?;
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("db");
            let to = backups.join(format!(
                "{stem}-schema{at}-{}.db",
                chrono::Utc::now().format("%Y%m%d-%H%M%S")
            ));
            snapshot(&conn, &to)?;
            tracing::info!(from = %path.display(), to = %to.display(), "backed up before migrating");
        }
    }
    open(path, migrations)
}

/// An in-memory database with the same set-up, for tests.
pub fn open_in_memory(migrations: &Migrations) -> anyhow::Result<Connection> {
    prepare(Connection::open_in_memory()?, migrations)
}

/// A consistent copy of the whole database in one new file, while it is in use.
pub fn snapshot(conn: &Connection, to: &Path) -> anyhow::Result<()> {
    let to = to.to_str().context("backup path isn't UTF-8")?;
    conn.execute("VACUUM INTO ?1", [to])
        .with_context(|| format!("copying the database to {to}"))?;
    Ok(())
}

/// Moves everything from the write-ahead log into the database file and
/// empties the log; for a clean shutdown.
pub fn checkpoint(conn: &Connection) -> rusqlite::Result<()> {
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
}

/// Pragmas for a database that sits on a disk which should be left alone as
/// much as possible (a NAS's hard drives, say):
///
/// - Write-ahead logging with `synchronous = NORMAL`: a commit appends to the
///   log without waiting for the disk; only checkpoints sync.
/// - Checkpoints after about 16 MB of log instead of 4 MB, so the database
///   file is rewritten in fewer, larger bursts.
/// - Up to 32 MB of pages cached and 128 MB memory-mapped, so reads come from
///   memory once warm; temporary tables and sorts stay in memory too.
fn prepare(mut conn: Connection, migrations: &Migrations) -> anyhow::Result<Connection> {
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "wal_autocheckpoint", 4000)?;
    conn.pragma_update(None, "journal_size_limit", 64 * 1024 * 1024)?;
    conn.pragma_update(None, "cache_size", -32 * 1024)?;
    conn.pragma_update(None, "mmap_size", 128 * 1024 * 1024)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    migrations.to_latest(&mut conn)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use rusqlite_migration::M;

    use super::*;

    #[tokio::test]
    async fn actor_runs_queries_and_survives_a_panic() {
        const MIGRATIONS: &[M<'static>] = &[M::up("CREATE TABLE t (n INTEGER);")];
        let db = Actor::spawn("test", || {
            open_in_memory(&Migrations::from_slice(MIGRATIONS))
        })
        .unwrap();

        db.call(|c| c.execute("INSERT INTO t VALUES (41)", []))
            .await
            .unwrap()
            .unwrap();
        let panicked = db.call(|_: &mut Connection| -> () { panic!("boom") }).await;
        assert!(panicked.is_err());

        let n: i64 = db
            .call(|c| c.query_row("SELECT n + 1 FROM t", [], |r| r.get(0)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(n, 42);
    }

    #[test]
    fn backs_up_only_before_a_migration() {
        let dir = tempfile::tempdir().unwrap();
        let (path, backups) = (dir.path().join("w.db"), dir.path().join("backups"));
        let one: &[M<'static>] = &[M::up(
            "CREATE TABLE a (n INTEGER); INSERT INTO a VALUES (7);",
        )];
        let two: &[M<'static>] = &[one[0].clone(), M::up("CREATE TABLE b (n INTEGER);")];

        drop(open_with_backup(&path, &Migrations::from_slice(one), &backups).unwrap());
        drop(open_with_backup(&path, &Migrations::from_slice(one), &backups).unwrap());
        assert!(!backups.exists(), "nothing to migrate, nothing to back up");

        drop(open_with_backup(&path, &Migrations::from_slice(two), &backups).unwrap());
        let copies: Vec<_> = std::fs::read_dir(&backups).unwrap().collect();
        assert_eq!(copies.len(), 1);
        let copy = Connection::open(copies[0].as_ref().unwrap().path()).unwrap();
        let n: i64 = copy.query_row("SELECT n FROM a", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 7);
    }
}

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

/// An in-memory database with the same set-up, for tests.
pub fn open_in_memory(migrations: &Migrations) -> anyhow::Result<Connection> {
    prepare(Connection::open_in_memory()?, migrations)
}

fn prepare(mut conn: Connection, migrations: &Migrations) -> anyhow::Result<Connection> {
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
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
}

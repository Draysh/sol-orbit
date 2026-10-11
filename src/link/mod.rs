//! [`Link`]: everything a world's app needs to work with Sol.
//!
//! - **Pairing:** [`Link::connect`] asks Sol for a code, shows it in
//!   [`Status::Pairing`], and finishes by itself once the person approves it.
//! - **The token** goes to the system keyring (see [`crate::keystore`]).
//! - **A local copy** of the world's data, in SQLite, so the app opens at once
//!   and keeps working offline. Writes land there first and are sent in order.
//! - **Sync:** while paired, one task sends local writes and long-polls Sol's
//!   changes; another reads the inbox, claims each delivery (so only one of
//!   the person's devices acts on it) and hands it to the world's [`Handler`].
//!
//! ```no_run
//! # async fn run(dir: std::path::PathBuf) -> anyhow::Result<()> {
//! use orbit::link::{Config, Link, Tokens};
//!
//! let link = Link::open(Config {
//!     world: "terra".into(),
//!     dir,
//!     device: "Desktop".into(),
//!     platform: Some("linux".into()),
//!     version: "0.1.0".into(),
//!     tokens: Tokens::Keyring,
//! })
//! .await?;
//! link.start(std::sync::Arc::new(orbit::link::Ignore));
//! link.put("habits", "walk", serde_json::json!({ "name": "Walk outside" })).await?;
//! # Ok(()) }
//! ```

mod cache;
mod docs;
mod inbox;
mod kept;
mod pairing;
mod settings;
mod sync;
#[cfg(test)]
mod tests;
mod token;

use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use rusqlite_migration::Migrations;
use serde::Serialize;
use tokio::{
    sync::{Notify, broadcast},
    task::JoinHandle,
};

pub use docs::Listed;
pub use kept::{Kept, KeptWrite, Why};
pub use settings::Configure;

use cache::{MIGRATIONS, meta, read_meta, set_meta};
use token::{Token, load_token};

use crate::{
    Actor, WidgetView,
    client::Sol,
    device::{Delivery, PairStarted, Person},
    event::Emit,
};

/// How long one request for changes or deliveries waits on Sol.
const LONG_POLL: Duration = Duration::from_secs(25);
const SETTINGS_EVERY: Duration = Duration::from_secs(10 * 60);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// What the status says while the keyring holding the token is still locked.
const KEYRING_LOCKED: &str = "waiting for the keyring to unlock";

/// The local copy didn't open: the app asks the person whether to start
/// again from Sol's copy ([`set_aside`]), or to quit and leave it as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCopyError {
    pub problem: LocalCopyProblem,
    /// The copy: `<dir>/<world>.db`.
    pub path: PathBuf,
    /// What SQLite said, for the log and a "details" line.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LocalCopyProblem {
    /// Written by a newer version of the app (one that ran here before).
    Newer,
    /// Not a database any more, or damaged inside.
    Damaged,
}

impl std::fmt::Display for LocalCopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.problem {
            LocalCopyProblem::Newer => write!(
                f,
                "{} was written by a newer version of the app",
                self.path.display()
            ),
            LocalCopyProblem::Damaged => {
                write!(f, "{} is damaged ({})", self.path.display(), self.detail)
            }
        }
    }
}

impl std::error::Error for LocalCopyError {}

impl LocalCopyError {
    /// The problem behind a failed open, when it is one of the two.
    fn of(err: &anyhow::Error, path: &std::path::Path) -> Option<Self> {
        use rusqlite::ErrorCode;
        use rusqlite_migration::{Error as M, MigrationDefinitionError};
        let damaged = |e: &rusqlite::Error| {
            matches!(
                e.sqlite_error_code(),
                Some(ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt)
            )
        };
        let problem = err.chain().find_map(|cause| {
            if let Some(m) = cause.downcast_ref::<M>() {
                return match m {
                    M::MigrationDefinition(MigrationDefinitionError::DatabaseTooFarAhead) => {
                        Some(LocalCopyProblem::Newer)
                    }
                    M::RusqliteError { err, .. } if damaged(err) => Some(LocalCopyProblem::Damaged),
                    _ => None,
                };
            }
            cause
                .downcast_ref::<rusqlite::Error>()
                .filter(|e| damaged(e))
                .map(|_| LocalCopyProblem::Damaged)
        })?;
        Some(Self {
            problem,
            path: path.to_owned(),
            detail: format!("{err:#}"),
        })
    }
}

/// Moves a local copy that won't open out of the way (with its log files),
/// so the app starts again from Sol's copy; returns where it went
/// (`<world>.db.set-aside-<time>` beside it). Nothing is deleted.
pub fn set_aside(dir: &std::path::Path, world: &str) -> anyhow::Result<PathBuf> {
    let stamp = Utc::now().format("%Y%m%d-%H%M%S");
    let db = dir.join(format!("{world}.db"));
    let to = dir.join(format!("{world}.db.set-aside-{stamp}"));
    std::fs::rename(&db, &to)?;
    for end in ["-wal", "-shm"] {
        let from = dir.join(format!("{world}.db{end}"));
        if from.exists() {
            std::fs::rename(
                &from,
                dir.join(format!("{world}.db.set-aside-{stamp}{end}")),
            )?;
        }
    }
    Ok(to)
}

/// How a [`Link`] is set up.
pub struct Config {
    /// The world this app is, e.g. `terra`.
    pub world: String,
    /// Where the local copy lives, usually the app's data folder.
    pub dir: PathBuf,
    /// How the device appears in Sol, e.g. the computer's name.
    pub device: String,
    /// The system it runs on, `linux`, for Sol's device list.
    pub platform: Option<String>,
    /// The app's own version, e.g. `0.2.0`; Sol shows it under Devices.
    pub version: String,
    pub tokens: Tokens,
}

/// Where the token is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tokens {
    /// The system keyring, falling back to a private file when there is none
    /// (a headless machine, say).
    Keyring,
    /// A private file in the data folder; for tests.
    File,
}

/// Where the app stands with Sol.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Status {
    /// Not paired. `error` says why, when it ended badly.
    Unpaired { error: Option<String> },
    /// Waiting for the person to approve `code` in Sol.
    Pairing {
        server: String,
        code: String,
        expires_at: DateTime<Utc>,
    },
    /// Paired, and Sol answered last time.
    Online { server: String, user: Person },
    /// Paired, but Sol isn't answering; `error` is `None` while connecting.
    Offline {
        server: String,
        user: Person,
        error: Option<String>,
    },
}

/// What changed, for the app's screens.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "lowercase")]
pub enum Update {
    Status(Status),
    /// Documents in these collections changed, here or on another device.
    Changed(Vec<String>),
    /// The world's settings in Sol changed.
    Settings,
    /// News about the app's own updates (see [`crate::updates`]).
    App(crate::updates::Status),
    /// Writes kept aside instead of sent changed: a conflict, a write Sol
    /// turned down, writes from before the device was unpaired.
    Kept(Kept),
}

pub type Boxed<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The world's own logic, called by the sync.
pub trait Handler: Send + Sync + 'static {
    /// Something Sol delivered: one of the world's actions from a connection,
    /// or `widget.toggle` from Sol's overview. Return an error to have it
    /// logged; the inbox moves on either way.
    fn deliver<'a>(
        &'a self,
        link: &'a Link,
        delivery: &'a Delivery,
    ) -> Boxed<'a, anyhow::Result<()>>;

    /// Documents changed, here or from another device: a good moment to push
    /// fresh widgets. Does nothing unless the world overrides it.
    fn changed<'a>(&'a self, _link: &'a Link, _collections: &'a [String]) -> Boxed<'a, ()> {
        Box::pin(async {})
    }
}

/// A handler that ignores deliveries.
pub struct Ignore;

impl Handler for Ignore {
    fn deliver<'a>(&'a self, _: &'a Link, _: &'a Delivery) -> Boxed<'a, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

enum State {
    Unpaired {
        error: Option<String>,
    },
    Pairing {
        server: String,
        started: PairStarted,
    },
    Paired {
        client: Sol,
        user: Person,
        online: bool,
        error: Option<String>,
    },
}

struct Inner {
    cfg: Config,
    db: Actor<Connection>,
    state: Mutex<State>,
    updates: broadcast::Sender<Update>,
    /// Rings when there are local writes to send.
    dirty: Notify,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    handler: OnceLock<Arc<dyn Handler>>,
}

/// A world app's connection to Sol. Cheap to clone; clones share everything.
#[derive(Clone)]
pub struct Link(Arc<Inner>);

impl Link {
    /// Opens the local copy and picks up an earlier pairing. Before a
    /// migration changes the copy it is backed up into `backups/` beside it.
    /// A copy that can't be opened (written by a newer version of the app,
    /// or damaged) fails with a [`LocalCopyError`], for the app to ask the
    /// person what to do ([`set_aside`] starts again from Sol's copy).
    pub async fn open(cfg: Config) -> anyhow::Result<Self> {
        let path = cfg.dir.join(format!("{}.db", cfg.world));
        let backups = cfg.dir.join("backups");
        let at = path.clone();
        let db = Actor::spawn(&cfg.world, move || {
            crate::db::open_with_backup(&at, &Migrations::from_slice(MIGRATIONS), &backups)
        })
        .map_err(|err| match LocalCopyError::of(&err, &path) {
            Some(problem) => anyhow::Error::new(problem),
            None => err,
        })?;
        let meta = db.call(|c| read_meta(c)).await??;
        let token = match (&meta.server, &meta.device) {
            (Some(_), Some(device)) => load_token(&cfg, device).await,
            _ => Token::None,
        };
        let state = match (meta.server, meta.user, token) {
            (Some(server), Some(user), Token::Found(token)) => State::Paired {
                client: Sol::new(server)
                    .with_version(&cfg.version)
                    .with_token(token),
                user,
                online: false,
                error: None,
            },
            // Paired, but the keyring isn't open yet (it unlocks with the
            // session, after an app that starts at login): offline until it is.
            (Some(server), Some(user), Token::Locked) => State::Paired {
                client: Sol::new(server).with_version(&cfg.version),
                user,
                online: false,
                error: Some(KEYRING_LOCKED.into()),
            },
            _ => State::Unpaired { error: None },
        };
        Ok(Self(Arc::new(Inner {
            cfg,
            db,
            state: Mutex::new(state),
            updates: broadcast::channel(64).0,
            dirty: Notify::new(),
            tasks: Mutex::default(),
            handler: OnceLock::new(),
        })))
    }

    /// Starts syncing (if paired) with `handler` for the world's logic. Call
    /// once, from within the async runtime.
    pub fn start(&self, handler: Arc<dyn Handler>) {
        let _ = self.0.handler.set(handler);
        let locked = match &*self.lock() {
            State::Paired { error, .. } => Some(error.as_deref() == Some(KEYRING_LOCKED)),
            _ => None,
        };
        match locked {
            Some(false) => self.spawn_session(),
            Some(true) => {
                let link = self.clone();
                let task = tokio::spawn(async move { link.wait_for_keyring().await });
                self.0.tasks.lock().expect("tasks lock").push(task);
            }
            None => {}
        }
    }

    /// Asks the keyring for the token again until it opens, then syncs.
    async fn wait_for_keyring(&self) {
        let Ok(Ok(Some(device))) = self.0.db.call(|c| meta(c, "device")).await else {
            return;
        };
        let mut wait = Duration::from_secs(2);
        loop {
            tokio::time::sleep(wait).await;
            match load_token(&self.0.cfg, &device).await {
                Token::Found(token) => {
                    if let State::Paired { client, error, .. } = &mut *self.lock() {
                        *client = client.clone().with_token(token);
                        *error = None;
                    }
                    tracing::info!("the keyring opened; syncing");
                    self.announce();
                    // From a task of its own: this one is among those it stops.
                    let link = self.clone();
                    tokio::spawn(async move { link.spawn_session() });
                    return;
                }
                // The keyring answered and has no token: the person pairs
                // again (what waits to be sent is kept for that).
                Token::None => {
                    *self.lock() = State::Unpaired {
                        error: Some("the keyring had no token for Sol; connect again".into()),
                    };
                    self.announce();
                    return;
                }
                Token::Locked => wait = (wait * 2).min(MAX_BACKOFF),
            }
        }
    }

    pub fn world(&self) -> &str {
        &self.0.cfg.world
    }

    pub fn status(&self) -> Status {
        let server = self.server();
        match &*self.lock() {
            State::Unpaired { error } => Status::Unpaired {
                error: error.clone(),
            },
            State::Pairing { server, started } => Status::Pairing {
                server: server.clone(),
                code: started.code.clone(),
                expires_at: started.expires_at,
            },
            State::Paired {
                user, online: true, ..
            } => Status::Online {
                server: server.unwrap_or_default(),
                user: user.clone(),
            },
            State::Paired { user, error, .. } => Status::Offline {
                server: server.unwrap_or_default(),
                user: user.clone(),
                error: error.clone(),
            },
        }
    }

    /// Status changes and changed collections, for the app's screens.
    pub fn subscribe(&self) -> broadcast::Receiver<Update> {
        self.0.updates.subscribe()
    }

    /// The signed-in person, while paired; their time zone decides "today".
    pub fn person(&self) -> Option<Person> {
        match &*self.lock() {
            State::Paired { user, .. } => Some(user.clone()),
            _ => None,
        }
    }

    /// The person's time zone, which decides "today"; UTC while unpaired
    /// or when Sol sent one this build doesn't know.
    pub fn time_zone(&self) -> chrono_tz::Tz {
        self.person()
            .and_then(|p| p.tz.parse().ok())
            .unwrap_or(chrono_tz::Tz::UTC)
    }

    /// For tests: writes down a pairing with `server` as `person`, as the
    /// device `device`, for the copy to pick up when it is opened again.
    #[cfg(any(test, feature = "testing"))]
    pub(crate) async fn pretend_paired(
        &self,
        server: &str,
        device: &str,
        person: &Person,
    ) -> anyhow::Result<()> {
        let (server, device, user) = (
            server.to_owned(),
            device.to_owned(),
            serde_json::to_string(person)?,
        );
        self.0
            .db
            .call(move |c| -> rusqlite::Result<()> {
                cache::set_meta(c, "server", &server)?;
                cache::set_meta(c, "device", &device)?;
                cache::set_meta(c, "user", &user)
            })
            .await??;
        Ok(())
    }

    /// Posts an event, in order with the writes before it.
    pub async fn emit(&self, mut event: Emit) -> anyhow::Result<()> {
        event.id.get_or_insert_with(uuid::Uuid::now_v7);
        let body = serde_json::to_string(&event)?;
        self.queue("event", None, body).await
    }

    /// Replaces one of the world's widgets on Sol's overview.
    pub async fn push_widget(&self, id: &str, title: &str, view: WidgetView) -> anyhow::Result<()> {
        let body = serde_json::json!({ "title": title, "view": view }).to_string();
        self.queue("widget", Some(id), body).await
    }

    /// Takes one of the world's widgets off Sol's overview.
    pub async fn remove_widget(&self, id: &str) -> anyhow::Result<()> {
        self.queue("unwidget", Some(id), "null".into()).await
    }

    /// Local writes and events not yet in Sol.
    pub async fn unsent(&self) -> anyhow::Result<i64> {
        Ok(self
            .0
            .db
            .call(|c| c.query_row("SELECT count(*) FROM pending", [], |r| r.get(0)))
            .await??)
    }

    // ---- For the rest of orbit -----------------------------------------------

    /// A preference of this device's own (`pref:` keys), kept when it pairs again.
    pub(crate) async fn pref(&self, key: &'static str) -> Option<String> {
        self.0.db.call(move |c| meta(c, key)).await.ok()?.ok()?
    }

    pub(crate) async fn set_pref(&self, key: &'static str, value: &str) -> anyhow::Result<()> {
        debug_assert!(key.starts_with("pref:"));
        let value = value.to_owned();
        self.0.db.call(move |c| set_meta(c, key, &value)).await??;
        Ok(())
    }

    pub(crate) fn send(&self, update: Update) {
        let _ = self.0.updates.send(update);
    }

    pub(crate) fn client(&self) -> Option<Sol> {
        match &*self.lock() {
            State::Paired { client, .. } => Some(client.clone()),
            _ => None,
        }
    }

    // ---- Inside ------------------------------------------------------------

    fn sol(&self, server: &str) -> Sol {
        Sol::new(server).with_version(&self.0.cfg.version)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock().expect("link state lock")
    }

    fn server(&self) -> Option<String> {
        match &*self.lock() {
            State::Paired { client, .. } => Some(client.base().to_owned()),
            State::Pairing { server, .. } => Some(server.clone()),
            State::Unpaired { .. } => None,
        }
    }

    fn announce(&self) {
        let _ = self.0.updates.send(Update::Status(self.status()));
    }

    fn handler(&self) -> Option<Arc<dyn Handler>> {
        self.0.handler.get().cloned()
    }

    fn stop_tasks(&self) {
        for task in self.0.tasks.lock().expect("tasks lock").drain(..) {
            task.abort();
        }
    }

    async fn queue(&self, kind: &str, id: Option<&str>, body: String) -> anyhow::Result<()> {
        let (kind, id) = (kind.to_owned(), id.map(str::to_owned));
        self.0
            .db
            .call(move |c| -> rusqlite::Result<()> {
                // Only the latest look of a widget matters.
                if kind == "widget" || kind == "unwidget" {
                    c.execute(
                        "DELETE FROM pending WHERE kind IN ('widget', 'unwidget') AND id = ?1",
                        [&id],
                    )?;
                }
                c.execute(
                    "INSERT INTO pending (kind, id, body) VALUES (?1, ?2, ?3)",
                    params![kind, id, body],
                )?;
                Ok(())
            })
            .await??;
        self.0.dirty.notify_waiters();
        Ok(())
    }

    async fn wrote(&self, collections: &[String]) {
        self.0.dirty.notify_waiters();
        let _ = self.0.updates.send(Update::Changed(collections.to_vec()));
        if let Some(handler) = self.handler() {
            handler.changed(self, collections).await;
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        for task in self.tasks.lock().expect("tasks lock").drain(..) {
            task.abort();
        }
    }
}

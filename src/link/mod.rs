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

pub use settings::Configure;

use cache::{MIGRATIONS, meta, read_meta, set_meta};
use token::load_token;

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

/// How a [`Link`] is set up.
pub struct Config {
    /// The world this app is, e.g. `terra`.
    pub world: String,
    /// Where the local copy lives, usually the app's data folder.
    pub dir: PathBuf,
    /// How the device appears in Sol, e.g. the computer's name.
    pub device: String,
    /// `linux`, `windows`, `macos`, `android` or `ios`.
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
    /// Opens the local copy and picks up an earlier pairing.
    pub async fn open(cfg: Config) -> anyhow::Result<Self> {
        let path = cfg.dir.join(format!("{}.db", cfg.world));
        let db = Actor::spawn(&cfg.world, move || {
            crate::db::open(&path, &Migrations::from_slice(MIGRATIONS))
        })?;
        let meta = db.call(read_meta).await??;
        let token = match (&meta.server, &meta.device) {
            (Some(_), Some(device)) => load_token(&cfg, device).await,
            _ => None,
        };
        let state = match (meta.server, meta.user, token) {
            (Some(server), Some(user), Some(token)) => State::Paired {
                client: Sol::new(server)
                    .with_version(&cfg.version)
                    .with_token(token),
                user,
                online: false,
                error: None,
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
        if matches!(*self.lock(), State::Paired { .. }) {
            self.spawn_session();
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

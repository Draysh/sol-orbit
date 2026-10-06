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
//!     tokens: Tokens::Keyring,
//! })
//! .await?;
//! link.start(std::sync::Arc::new(orbit::link::Ignore));
//! link.put("habits", "walk", serde_json::json!({ "name": "Walk outside" })).await?;
//! # Ok(()) }
//! ```

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};
use rusqlite_migration::{M, Migrations};
use serde::Serialize;
use tokio::{
    sync::{Notify, broadcast},
    task::JoinHandle,
};

use crate::{
    Actor, Doc, WidgetView,
    client::{Error as ClientError, Sol},
    device::{Delivery, PairStarted, Paired, Person},
    doc::{valid_collection, valid_id},
    event::Emit,
    keystore,
};

const MIGRATIONS: &[M<'static>] = &[M::up(include_str!("link/cache.sql"))];
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
                client: Sol::new(server).with_token(token),
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

    /// Starts pairing with the Sol at `server`; the code is in the returned
    /// [`Status::Pairing`]. Finishes by itself once the person approves it.
    pub async fn connect(&self, server: &str) -> anyhow::Result<Status> {
        let server = normalise(server)?;
        let probe = Sol::new(&server);
        match probe.probe().await {
            Ok(true) => {}
            Ok(false) => bail!("{server} answered, but it isn't Sol"),
            Err(_) => bail!(
                "no answer from {server}; is Sol running, and is this device on the same network or tailnet?"
            ),
        }
        let started = match probe
            .pair(
                &self.0.cfg.world,
                &self.0.cfg.device,
                self.0.cfg.platform.as_deref(),
            )
            .await
        {
            Ok(started) => started,
            Err(ClientError::Api { message, .. }) => bail!("Sol says: {message}"),
            Err(err) => return Err(err.into()),
        };
        self.stop_tasks();
        *self.lock() = State::Pairing {
            server: server.clone(),
            started: started.clone(),
        };
        let link = self.clone();
        let task = tokio::spawn(async move { link.wait_for_approval(server, started).await });
        self.0.tasks.lock().expect("tasks lock").push(task);
        self.announce();
        Ok(self.status())
    }

    /// Stops a pairing that hasn't been approved.
    pub fn cancel(&self) {
        if matches!(*self.lock(), State::Pairing { .. }) {
            self.stop_tasks();
            *self.lock() = State::Unpaired { error: None };
            self.announce();
        }
    }

    /// Unpairs this device: tells Sol, forgets the token and the local copy.
    pub async fn forget(&self) -> anyhow::Result<()> {
        self.stop_tasks();
        if let Some(client) = self.client() {
            // Best effort: Sol also lets the person unpair it from there.
            let _ = client.unpair().await;
        }
        self.reset(None).await
    }

    // ---- Data ------------------------------------------------------------

    /// Every live document in a collection, from the local copy.
    pub async fn list(&self, collection: &str) -> anyhow::Result<Vec<Doc>> {
        let collection = collection.to_owned();
        Ok(self
            .0
            .db
            .call(move |c| {
                c.prepare_cached(&format!(
                    "SELECT {COLUMNS} FROM docs WHERE collection = ?1 AND deleted = 0 ORDER BY id"
                ))?
                .query_map([collection], doc)?
                .collect::<rusqlite::Result<Vec<_>>>()
            })
            .await??)
    }

    pub async fn get(&self, collection: &str, id: &str) -> anyhow::Result<Option<Doc>> {
        let (collection, id) = (collection.to_owned(), id.to_owned());
        Ok(self
            .0
            .db
            .call(move |c| local(c, &collection, &id))
            .await??
            .filter(|d| !d.deleted))
    }

    /// Writes a document here at once and sends it to Sol in the background.
    pub async fn put(
        &self,
        collection: &str,
        id: &str,
        data: serde_json::Value,
    ) -> anyhow::Result<Doc> {
        check_names(collection, id)?;
        anyhow::ensure!(!data.is_null(), "use delete to remove a document");
        let (c2, i2) = (collection.to_owned(), id.to_owned());
        let written = self
            .0
            .db
            .call(move |c| -> rusqlite::Result<Doc> {
                let tx = c.transaction()?;
                let version = local(&tx, &c2, &i2)?.map_or(0, |d| d.version);
                tx.execute(
                    "INSERT INTO docs (collection, id, version, updated_at, deleted, data)
                     VALUES (?1, ?2, ?3, ?4, 0, ?5)
                     ON CONFLICT (collection, id) DO UPDATE SET
                       updated_at = excluded.updated_at, deleted = 0, data = excluded.data",
                    params![c2, i2, version, now(), data.to_string()],
                )?;
                tx.execute(
                    "INSERT INTO pending (kind, collection, id, body) VALUES ('put', ?1, ?2, ?3)",
                    params![c2, i2, data.to_string()],
                )?;
                let written = local(&tx, &c2, &i2)?.expect("just written");
                tx.commit()?;
                Ok(written)
            })
            .await??;
        self.wrote(&[collection.to_owned()]).await;
        Ok(written)
    }

    /// Deletes a document here at once and in Sol in the background.
    pub async fn delete(&self, collection: &str, id: &str) -> anyhow::Result<()> {
        check_names(collection, id)?;
        let (c2, i2) = (collection.to_owned(), id.to_owned());
        self.0
            .db
            .call(move |c| -> rusqlite::Result<()> {
                let tx = c.transaction()?;
                tx.execute(
                    "UPDATE docs SET deleted = 1, data = 'null', updated_at = ?3
                     WHERE collection = ?1 AND id = ?2",
                    params![c2, i2, now()],
                )?;
                tx.execute(
                    "INSERT INTO pending (kind, collection, id, body) VALUES ('delete', ?1, ?2, 'null')",
                    params![c2, i2],
                )?;
                tx.commit()
            })
            .await??;
        self.wrote(&[collection.to_owned()]).await;
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

    /// The world's settings as set in Sol, from the last sync.
    pub async fn settings(&self) -> anyhow::Result<BTreeMap<String, serde_json::Value>> {
        let json = self.0.db.call(|c| meta(c, "settings")).await??;
        Ok(json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default())
    }

    /// Local writes and events not yet in Sol.
    pub async fn unsent(&self) -> anyhow::Result<i64> {
        Ok(self
            .0
            .db
            .call(|c| c.query_row("SELECT count(*) FROM pending", [], |r| r.get(0)))
            .await??)
    }

    // ---- Inside ------------------------------------------------------------

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock().expect("link state lock")
    }

    fn client(&self) -> Option<Sol> {
        match &*self.lock() {
            State::Paired { client, .. } => Some(client.clone()),
            _ => None,
        }
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

    fn set_connectivity(&self, ok: bool, err: Option<String>) {
        let changed = {
            let mut state = self.lock();
            match &mut *state {
                State::Paired { online, error, .. } if *online != ok || *error != err => {
                    *online = ok;
                    *error = err;
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.announce();
        }
    }

    async fn wait_for_approval(&self, server: String, started: PairStarted) {
        let client = Sol::new(&server);
        loop {
            if Utc::now() > started.expires_at {
                return self.pairing_ended("the code expired; connect again for a new one");
            }
            match client.claim(&started, LONG_POLL).await {
                Ok(Some(paired)) => {
                    if let Err(err) = self.finish(&server, paired).await {
                        tracing::warn!(error = ?err, "couldn't keep the pairing");
                        self.pairing_ended(&format!("paired, but couldn't save it: {err}"));
                    }
                    return;
                }
                Ok(None) => {}
                Err(ClientError::PairingGone) => {
                    return self.pairing_ended("Sol declined the request, or it expired");
                }
                Err(_) => tokio::time::sleep(Duration::from_secs(3)).await,
            }
        }
    }

    fn pairing_ended(&self, why: &str) {
        *self.lock() = State::Unpaired {
            error: Some(why.to_owned()),
        };
        self.announce();
    }

    async fn finish(&self, server: &str, paired: Paired) -> anyhow::Result<()> {
        save_token(&self.0.cfg, &paired.device_id, &paired.token).await?;
        let (server_s, device, user) = (
            server.to_owned(),
            paired.device_id.clone(),
            serde_json::to_string(&paired.user)?,
        );
        self.0
            .db
            .call(move |c| -> rusqlite::Result<()> {
                let tx = c.transaction()?;
                // A new pairing starts from Sol's copy.
                tx.execute_batch("DELETE FROM docs; DELETE FROM pending; DELETE FROM meta;")?;
                for (k, v) in [("server", server_s), ("device", device), ("user", user)] {
                    tx.execute(
                        "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                        params![k, v],
                    )?;
                }
                tx.commit()
            })
            .await??;
        *self.lock() = State::Paired {
            client: Sol::new(server).with_token(paired.token),
            user: paired.user,
            online: true,
            error: None,
        };
        self.announce();
        self.spawn_session();
        Ok(())
    }

    /// Back to unpaired: no token, no local copy.
    async fn reset(&self, error: Option<String>) -> anyhow::Result<()> {
        let device = self.0.db.call(|c| meta(c, "device")).await??;
        if let Some(device) = device {
            forget_token(&self.0.cfg, &device).await;
        }
        self.0
            .db
            .call(|c| c.execute_batch("DELETE FROM docs; DELETE FROM pending; DELETE FROM meta;"))
            .await??;
        *self.lock() = State::Unpaired { error };
        self.announce();
        let _ = self.0.updates.send(Update::Changed(Vec::new()));
        Ok(())
    }

    fn spawn_session(&self) {
        self.stop_tasks();
        let (sync, inbox) = (self.clone(), self.clone());
        let mut tasks = self.0.tasks.lock().expect("tasks lock");
        tasks.push(tokio::spawn(async move { sync.sync_loop().await }));
        tasks.push(tokio::spawn(async move { inbox.inbox_loop().await }));
    }

    /// Sends local writes, keeps the settings fresh, and long-polls changes,
    /// coming back early whenever there is something new to send.
    async fn sync_loop(&self) {
        let mut backoff = Duration::from_secs(1);
        let mut settings_at: Option<Instant> = None;
        while let Some(client) = self.client() {
            let step = async {
                self.flush(&client).await?;
                if settings_at.is_none_or(|t| t.elapsed() > SETTINGS_EVERY) {
                    let settings = serde_json::to_string(&client.settings().await?)?;
                    let fresh = self
                        .0
                        .db
                        .call(move |c| -> rusqlite::Result<bool> {
                            let fresh = meta(c, "settings")?.as_deref() != Some(settings.as_str());
                            set_meta(c, "settings", &settings)?;
                            Ok(fresh)
                        })
                        .await??;
                    if fresh {
                        let _ = self.0.updates.send(Update::Settings);
                    }
                    settings_at = Some(Instant::now());
                }
                let notified = self.0.dirty.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.unsent().await? > 0 {
                    return anyhow::Ok(());
                }
                let after = self.0.db.call(|c| cursor(c, "changes")).await??;
                self.set_connectivity(true, None);
                tokio::select! {
                    page = client.changes(after, LONG_POLL) => self.apply(page?).await?,
                    () = notified => {}
                }
                anyhow::Ok(())
            }
            .await;
            match step {
                Ok(()) => backoff = Duration::from_secs(1),
                Err(err) if unpaired(&err) => return self.lost().await,
                Err(err) => {
                    self.set_connectivity(false, Some(friendly(&err)));
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    }

    /// Hands every delivery to the world's handler, in order.
    async fn inbox_loop(&self) {
        let mut backoff = Duration::from_secs(1);
        while let Some(client) = self.client() {
            let step = async {
                let after = self.0.db.call(|c| cursor(c, "inbox")).await??;
                let page = client.inbox(after, LONG_POLL).await?;
                for delivery in &page.deliveries {
                    if delivery.action == "sol.settings" {
                        // For every device, not claimed: the settings changed in Sol.
                        let settings = serde_json::to_string(&client.settings().await?)?;
                        self.0
                            .db
                            .call(move |c| set_meta(c, "settings", &settings))
                            .await??;
                        let _ = self.0.updates.send(Update::Settings);
                    } else if client.claim_delivery(delivery.n).await?
                        && let Some(handler) = self.handler()
                        && let Err(err) = handler.deliver(self, delivery).await
                    {
                        tracing::warn!(action = %delivery.action, error = ?err, "delivery failed");
                    }
                    let n = delivery.n;
                    self.0
                        .db
                        .call(move |c| set_meta(c, "cursor:inbox", &n.to_string()))
                        .await??;
                }
                anyhow::Ok(())
            }
            .await;
            match step {
                Ok(()) => backoff = Duration::from_secs(1),
                Err(err) if unpaired(&err) => return self.lost().await,
                Err(_) => {
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    }

    /// Sol no longer knows this device (someone unpaired it there).
    async fn lost(&self) {
        if self.client().is_none() {
            return;
        }
        let _ = self
            .reset(Some(
                "Sol unpaired this device; connect again to pair it".into(),
            ))
            .await;
        self.stop_tasks();
    }

    /// Sends what's waiting, oldest first. Writes Sol rejects as invalid are
    /// dropped (and logged) so they can't block everything after them.
    async fn flush(&self, client: &Sol) -> anyhow::Result<()> {
        loop {
            let next = self
                .0
                .db
                .call(|c| {
                    c.query_row(
                        "SELECT seq, kind, collection, id, body FROM pending ORDER BY seq LIMIT 1",
                        [],
                        |r| {
                            Ok((
                                r.get::<_, i64>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, Option<String>>(2)?,
                                r.get::<_, Option<String>>(3)?,
                                r.get::<_, String>(4)?,
                            ))
                        },
                    )
                    .optional()
                })
                .await??;
            let Some((seq, kind, collection, id, body)) = next else {
                return Ok(());
            };
            let (col, key) = (collection.unwrap_or_default(), id.unwrap_or_default());
            let sent = match kind.as_str() {
                "put" => client
                    .put(&col, &key, serde_json::from_str(&body)?, None)
                    .await
                    .map(Some),
                "delete" => match client.delete(&col, &key).await {
                    Err(ClientError::Api { status: 404, .. }) => Ok(None),
                    other => other.map(Some),
                },
                "event" => client
                    .emit(&serde_json::from_str(&body)?)
                    .await
                    .map(|_| None),
                "widget" => {
                    let w: serde_json::Value = serde_json::from_str(&body)?;
                    let view = serde_json::from_value(w["view"].clone())?;
                    client
                        .push_widget(&key, w["title"].as_str().unwrap_or_default(), view)
                        .await
                        .map(|()| None)
                }
                "unwidget" => client.remove_widget(&key).await.map(|()| None),
                other => {
                    tracing::warn!(kind = other, "dropping an unknown pending write");
                    Ok(None)
                }
            };
            let doc = match sent {
                Ok(doc) => doc,
                Err(ClientError::Api {
                    status: 400 | 413,
                    message,
                    ..
                }) => {
                    tracing::warn!(%kind, %message, "Sol rejected a write; dropping it");
                    None
                }
                Err(err) => return Err(err.into()),
            };
            self.0
                .db
                .call(move |c| -> rusqlite::Result<()> {
                    let tx = c.transaction()?;
                    tx.execute("DELETE FROM pending WHERE seq = ?1", [seq])?;
                    // Take Sol's version, unless a newer local write is still waiting.
                    if let Some(doc) = doc
                        && !waiting(&tx, &doc.collection, &doc.id)?
                    {
                        store(&tx, &doc)?;
                    }
                    tx.commit()
                })
                .await??;
        }
    }

    /// Takes in a page of changes from Sol.
    async fn apply(&self, page: crate::doc::ChangePage) -> anyhow::Result<()> {
        if page.changes.is_empty() {
            return Ok(());
        }
        let touched = self
            .0
            .db
            .call(move |c| -> rusqlite::Result<Vec<String>> {
                let tx = c.transaction()?;
                let mut touched = BTreeSet::new();
                for doc in &page.changes {
                    // A local write that hasn't reached Sol yet wins for now;
                    // Sol takes it in order, and its version comes back later.
                    if waiting(&tx, &doc.collection, &doc.id)? {
                        continue;
                    }
                    store(&tx, doc)?;
                    touched.insert(doc.collection.clone());
                }
                set_meta(&tx, "cursor:changes", &page.next.to_string())?;
                tx.commit()?;
                Ok(touched.into_iter().collect())
            })
            .await??;
        if !touched.is_empty() {
            let _ = self.0.updates.send(Update::Changed(touched.clone()));
            if let Some(handler) = self.handler() {
                handler.changed(self, &touched).await;
            }
        }
        Ok(())
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        for task in self.tasks.lock().expect("tasks lock").drain(..) {
            task.abort();
        }
    }
}

const COLUMNS: &str = "collection, id, version, updated_at, deleted, data";

fn doc(row: &Row) -> rusqlite::Result<Doc> {
    let data: String = row.get(5)?;
    Ok(Doc {
        collection: row.get(0)?,
        id: row.get(1)?,
        version: row.get(2)?,
        updated_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(3)?)
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_default(),
        deleted: row.get(4)?,
        data: serde_json::from_str(&data).unwrap_or_default(),
    })
}

fn local(c: &Connection, collection: &str, id: &str) -> rusqlite::Result<Option<Doc>> {
    c.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM docs WHERE collection = ?1 AND id = ?2"
    ))?
    .query_row(params![collection, id], doc)
    .optional()
}

fn store(c: &Connection, d: &Doc) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO docs (collection, id, version, updated_at, deleted, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (collection, id) DO UPDATE SET version = excluded.version,
           updated_at = excluded.updated_at, deleted = excluded.deleted, data = excluded.data",
        params![
            d.collection,
            d.id,
            d.version,
            d.updated_at.to_rfc3339(),
            d.deleted,
            d.data.to_string()
        ],
    )?;
    Ok(())
}

fn waiting(c: &Connection, collection: &str, id: &str) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT EXISTS (SELECT 1 FROM pending WHERE collection = ?1 AND id = ?2)",
        params![collection, id],
        |r| r.get(0),
    )
}

fn meta(c: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()
}

fn set_meta(c: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn cursor(c: &mut Connection, name: &str) -> rusqlite::Result<i64> {
    Ok(meta(c, &format!("cursor:{name}"))?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0))
}

struct Meta {
    server: Option<String>,
    device: Option<String>,
    user: Option<Person>,
}

fn read_meta(c: &mut Connection) -> rusqlite::Result<Meta> {
    Ok(Meta {
        server: meta(c, "server")?,
        device: meta(c, "device")?,
        user: meta(c, "user")?.and_then(|u| serde_json::from_str(&u).ok()),
    })
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn check_names(collection: &str, id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        valid_collection(collection),
        "a collection name is lowercase letters, digits, - and _"
    );
    anyhow::ensure!(valid_id(id), "a document id is letters, digits, - and _");
    Ok(())
}

/// `https://sol.example.ts.net/` → `https://sol.example.ts.net`.
fn normalise(server: &str) -> anyhow::Result<String> {
    let s = server.trim().trim_end_matches('/');
    let s = if s.contains("://") {
        s.to_owned()
    } else {
        format!("http://{s}")
    };
    anyhow::ensure!(
        (s.starts_with("http://") || s.starts_with("https://")) && s.len() > "https://".len(),
        "use an address like https://sol.your-tailnet.ts.net"
    );
    Ok(s)
}

fn unpaired(err: &anyhow::Error) -> bool {
    matches!(
        err.downcast_ref::<ClientError>(),
        Some(ClientError::Api { status: 401, .. })
    )
}

fn friendly(err: &anyhow::Error) -> String {
    match err.downcast_ref::<ClientError>() {
        Some(ClientError::Transport(_)) => "Sol isn't answering".into(),
        Some(ClientError::Api { message, .. }) => message.clone(),
        _ => err.to_string(),
    }
}

fn token_key(cfg: &Config, device: &str) -> String {
    format!("{}:{device}", cfg.world)
}

fn token_file(cfg: &Config, device: &str) -> PathBuf {
    cfg.dir.join(format!("{}-{device}.token", cfg.world))
}

async fn save_token(cfg: &Config, device: &str, token: &str) -> anyhow::Result<()> {
    if cfg.tokens == Tokens::Keyring {
        let (key, token2) = (token_key(cfg, device), token.to_owned());
        let saved = tokio::task::spawn_blocking(move || keystore::save(&key, &token2)).await?;
        match saved {
            Ok(_) => return Ok(()),
            Err(why) => tracing::warn!(%why, "no keyring; keeping the token in a private file"),
        }
    }
    write_private(&token_file(cfg, device), token)
}

async fn load_token(cfg: &Config, device: &str) -> Option<String> {
    if cfg.tokens == Tokens::Keyring {
        let key = token_key(cfg, device);
        if let Ok(Some(token)) = tokio::task::spawn_blocking(move || keystore::load(&key)).await {
            return Some(token);
        }
    }
    std::fs::read_to_string(token_file(cfg, device))
        .ok()
        .map(|t| t.trim().to_owned())
}

async fn forget_token(cfg: &Config, device: &str) {
    if cfg.tokens == Tokens::Keyring {
        let key = token_key(cfg, device);
        let _ = tokio::task::spawn_blocking(move || keystore::forget(&key)).await;
    }
    let _ = std::fs::remove_file(token_file(cfg, device));
}

fn write_private(path: &std::path::Path, contents: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    async fn offline() -> (Link, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let link = Link::open(Config {
            world: "terra".into(),
            dir: dir.path().to_owned(),
            device: "Test".into(),
            platform: None,
            tokens: Tokens::File,
        })
        .await
        .unwrap();
        (link, dir)
    }

    #[tokio::test]
    async fn works_offline_and_queues_in_order() {
        let (link, _dir) = offline().await;
        assert_eq!(link.status(), Status::Unpaired { error: None });

        let walk = link
            .put("habits", "walk", json!({ "name": "Walk" }))
            .await
            .unwrap();
        assert_eq!(walk.version, 0, "Sol hasn't seen it yet");
        link.put("habits", "read", json!({ "name": "Read" }))
            .await
            .unwrap();
        link.delete("habits", "read").await.unwrap();
        link.emit(Emit::new("terra.habit.checked", "Did “Walk”"))
            .await
            .unwrap();
        link.push_widget("today", "Today", WidgetView::default())
            .await
            .unwrap();
        link.push_widget("today", "Today", WidgetView::default())
            .await
            .unwrap();

        let names: Vec<_> = link
            .list("habits")
            .await
            .unwrap()
            .into_iter()
            .map(|d| d.id)
            .collect();
        assert_eq!(names, ["walk"]);
        assert!(link.get("habits", "read").await.unwrap().is_none());
        // Two writes, a delete, an event and one widget (the newer push replaced the older).
        assert_eq!(link.unsent().await.unwrap(), 5);
        assert!(link.put("Habits", "x", json!({})).await.is_err());
    }

    #[tokio::test]
    async fn changes_from_sol_wait_behind_local_writes() {
        let (link, _dir) = offline().await;
        link.put("habits", "walk", json!({ "name": "Mine" }))
            .await
            .unwrap();
        let theirs = |id: &str, version: i64| Doc {
            collection: "habits".into(),
            id: id.into(),
            version,
            updated_at: Utc::now(),
            deleted: false,
            data: json!({ "name": "Theirs" }),
        };
        link.apply(crate::doc::ChangePage {
            changes: vec![theirs("walk", 7), theirs("read", 8)],
            next: 8,
            more: false,
        })
        .await
        .unwrap();
        // The unsent local write wins for now; the other change comes in.
        assert_eq!(
            link.get("habits", "walk").await.unwrap().unwrap().data["name"],
            "Mine"
        );
        assert_eq!(
            link.get("habits", "read").await.unwrap().unwrap().version,
            8
        );
        let next = link
            .0
            .db
            .call(|c| cursor(c, "changes"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next, 8);
    }

    #[test]
    fn addresses() {
        assert_eq!(
            normalise("sol.local:8080/").unwrap(),
            "http://sol.local:8080"
        );
        assert_eq!(
            normalise(" https://sol.example.ts.net ").unwrap(),
            "https://sol.example.ts.net"
        );
        assert!(normalise("ftp://x").is_err());
    }
}

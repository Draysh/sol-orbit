//! The sync: local writes go to Sol in order, Sol's changes come in, and
//! the settings stay fresh.

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use rusqlite::{OptionalExtension, params};

use super::{
    LONG_POLL, Link, MAX_BACKOFF, SETTINGS_EVERY, State, Update,
    cache::{cursor, meta, set_meta, store, waiting},
    kept,
};
use crate::{
    Doc,
    client::{Error as ClientError, Sol},
};

/// What became of one write sent to Sol.
enum Sent {
    /// Sol took it; a document write comes back as Sol's copy.
    Taken(Option<Doc>),
    /// Another device changed the document first: Sol's copy now.
    Conflict(Option<Box<Doc>>),
    /// Sol turned it down, saying why.
    Rejected(String),
}

impl Link {
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

    pub(super) fn spawn_session(&self) {
        self.stop_tasks();
        let (sync, inbox) = (self.clone(), self.clone());
        let mut tasks = self.0.tasks.lock().expect("tasks lock");
        tasks.push(tokio::spawn(keep_alive("sync", move || {
            let sync = sync.clone();
            async move { sync.sync_loop().await }
        })));
        tasks.push(tokio::spawn(keep_alive("inbox", move || {
            let inbox = inbox.clone();
            async move { inbox.inbox_loop().await }
        })));
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
                        .call(move |c| remember_settings(c, &settings))
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
                    page = client.changes(after, LONG_POLL) => {
                        let page = page?;
                        if self.follows("changes", after, page.database.clone()).await? {
                            self.apply(page).await?;
                        } else {
                            // Sol was restored: everything again, settings too.
                            settings_at = None;
                        }
                    }
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
                    match retry_after(&err) {
                        // Sol asked for a pause (too many writes); the
                        // writes wait in order meanwhile.
                        Some(wait) => tokio::time::sleep(wait).await,
                        None => {
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(MAX_BACKOFF);
                        }
                    }
                }
            }
        }
    }

    /// Sol no longer knows this device (someone unpaired it there).
    pub(super) async fn lost(&self) {
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

    /// Sends what's waiting, oldest first. A document another device
    /// changed first comes back as a conflict and is kept aside with Sol's
    /// copy in its place; a write Sol rejects is kept aside with Sol's
    /// reason. Neither blocks what comes after it, and neither is lost.
    async fn flush(&self, client: &Sol) -> anyhow::Result<()> {
        loop {
            let next = self
                .0
                .db
                .call(|c| {
                    c.query_row(
                        "SELECT seq, kind, collection, id, body, base FROM pending ORDER BY seq LIMIT 1",
                        [],
                        |r| {
                            Ok((
                                r.get::<_, i64>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, Option<String>>(2)?,
                                r.get::<_, Option<String>>(3)?,
                                r.get::<_, String>(4)?,
                                r.get::<_, Option<i64>>(5)?,
                            ))
                        },
                    )
                    .optional()
                })
                .await??;
            let Some((seq, kind, collection, id, body, base)) = next else {
                return Ok(());
            };
            let (col, key) = (collection.unwrap_or_default(), id.unwrap_or_default());
            // A body that no longer reads (damaged on this computer) is
            // kept aside like a write Sol rejected, so the rest still go.
            let sent: Result<Result<Option<Doc>, ClientError>, serde_json::Error> = async {
                Ok(match kind.as_str() {
                    "put" => client
                        .put(&col, &key, serde_json::from_str(&body)?, base)
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
                })
            }
            .await;
            let outcome = match sent {
                Err(err) => Sent::Rejected(format!(
                    "This write was damaged on this computer and could not be sent ({err})"
                )),
                Ok(Ok(doc)) => Sent::Taken(doc),
                Ok(Err(ClientError::Conflict { current })) => Sent::Conflict(current),
                Ok(Err(ClientError::Api {
                    status: 400 | 413,
                    message,
                    ..
                })) => Sent::Rejected(message),
                Ok(Err(err)) => return Err(err.into()),
            };
            let kept = self
                .0
                .db
                .call(move |c| -> rusqlite::Result<bool> {
                    let tx = c.transaction()?;
                    let kept = match outcome {
                        Sent::Taken(doc) => {
                            tx.execute("DELETE FROM pending WHERE seq = ?1", [seq])?;
                            if let Some(doc) = doc {
                                // Later writes to it waiting here were made on top of
                                // this one: they build on Sol's new version.
                                tx.execute(
                                    "UPDATE pending SET base = ?3
                                     WHERE collection = ?1 AND id = ?2 AND base IS NOT NULL",
                                    params![doc.collection, doc.id, doc.version],
                                )?;
                                // Take Sol's version, unless a newer local write is still waiting.
                                if !waiting(&tx, &doc.collection, &doc.id)? {
                                    store(&tx, &doc)?;
                                }
                            }
                            false
                        }
                        Sent::Conflict(current) => {
                            tracing::info!(collection = %col, id = %key, "changed on another device too; keeping both");
                            kept::hold_conflict(&tx, &col, &key, current.as_deref())?
                        }
                        Sent::Rejected(message) => {
                            tracing::warn!(%kind, %message, "a write could not be sent; keeping it aside");
                            kept::hold_rejected(&tx, seq, &message)?;
                            true
                        }
                    };
                    tx.commit()?;
                    Ok(kept)
                })
                .await??;
            if kept {
                self.announce_kept().await;
            }
        }
    }

    /// Whether a page Sol sent for `cursor` (`changes` or `inbox`), asked
    /// for after `after`, still applies. Sol's database id tells a restore
    /// apart: under a new id (an older copy of Sol's data) the versions and
    /// the inbox start again, so both cursors go back to 0 and everything is
    /// read again. Documents stay on screen meanwhile, marked unconfirmed
    /// (version -1) until the full read keeps or drops them, and writes
    /// waiting for Sol stay too. Writes nothing while the id is unchanged.
    pub(super) async fn follows(
        &self,
        cursor_name: &'static str,
        after: i64,
        database: Option<String>,
    ) -> anyhow::Result<bool> {
        let (follows, restored) = self
            .0
            .db
            .call(move |c| -> rusqlite::Result<(bool, bool)> {
                let tx = c.transaction()?;
                let known = meta(&tx, "database")?;
                let mut restored = false;
                if let Some(id) = database
                    && known.as_deref() != Some(id.as_str())
                {
                    set_meta(&tx, "database", &id)?;
                    if known.is_some() {
                        restored = true;
                        tx.execute(
                            "UPDATE docs SET version = -1 WHERE version > 0 AND NOT EXISTS
                               (SELECT 1 FROM pending p WHERE p.collection = docs.collection AND p.id = docs.id)",
                            [],
                        )?;
                        if let Some(seen) = meta(&tx, "inbox:seen")? {
                            set_meta(&tx, "inbox:skip-until", &seen)?;
                        }
                        // The copy's versions run again from an older point, so
                        // the versions these writes were made on mean nothing
                        // there: they go in over the older copy.
                        tx.execute("UPDATE pending SET base = NULL", [])?;
                        set_meta(&tx, "cursor:changes", "0")?;
                        set_meta(&tx, "cursor:inbox", "0")?;
                        set_meta(&tx, "resync", "1")?;
                    }
                }
                // Another loop may have started over while this page was on its way.
                let follows = !restored && cursor(&tx, cursor_name)? == after;
                tx.commit()?;
                Ok((follows, restored))
            })
            .await??;
        if restored {
            tracing::info!("Sol's data was restored from a copy; reading everything again");
        }
        Ok(follows)
    }

    /// Takes in a page of changes from Sol.
    pub(super) async fn apply(&self, page: crate::doc::ChangePage) -> anyhow::Result<()> {
        if page.changes.is_empty()
            && (page.more || self.0.db.call(|c| meta(c, "resync")).await??.is_none())
        {
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
                // The full read after a restore is done: what Sol no longer
                // has goes, unless a local write for it is still waiting.
                if !page.more && meta(&tx, "resync")?.is_some() {
                    let gone: Vec<String> = tx
                        .prepare(
                            "DELETE FROM docs WHERE version = -1 AND NOT EXISTS
                               (SELECT 1 FROM pending p WHERE p.collection = docs.collection AND p.id = docs.id)
                             RETURNING collection",
                        )?
                        .query_map([], |r| r.get(0))?
                        .collect::<rusqlite::Result<_>>()?;
                    touched.extend(gone);
                    tx.execute("DELETE FROM meta WHERE key = 'resync'", [])?;
                }
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

/// Runs a loop again when it panics (a bug, a handler that fell over), after
/// a pause, so one bad moment never leaves the app without sync. The panic
/// is in the log (see [`crate::telemetry`]).
/// Keeps the settings Sol sent, writing them down only when they differ
/// from the ones kept (an idle app checks every few minutes), and says
/// whether they did.
fn remember_settings(c: &rusqlite::Connection, settings: &str) -> rusqlite::Result<bool> {
    let fresh = meta(c, "settings")?.as_deref() != Some(settings);
    if fresh {
        set_meta(c, "settings", settings)?;
    }
    Ok(fresh)
}

async fn keep_alive<F>(name: &'static str, make: impl Fn() -> F)
where
    F: std::future::Future<Output = ()> + Send,
{
    use futures_util::FutureExt;
    loop {
        match std::panic::AssertUnwindSafe(make()).catch_unwind().await {
            Ok(()) => return,
            Err(panic) => {
                let why = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                tracing::error!(task = name, %why, "the {name} loop crashed; starting it again");
                tokio::time::sleep(PANIC_PAUSE).await;
            }
        }
    }
}

#[cfg(test)]
pub(super) async fn keep_alive_for_tests<F>(make: impl Fn() -> F)
where
    F: std::future::Future<Output = ()> + Send,
{
    keep_alive("test", make).await
}

/// How long a crashed loop waits before it starts again.
const PANIC_PAUSE: Duration = Duration::from_secs(5);

/// How long Sol asked the app to wait, when it did (a `429`).
pub(super) fn retry_after(err: &anyhow::Error) -> Option<Duration> {
    err.downcast_ref::<ClientError>()?.retry_after()
}

pub(super) fn unpaired(err: &anyhow::Error) -> bool {
    matches!(
        err.downcast_ref::<ClientError>(),
        Some(ClientError::Api { status: 401, .. })
    )
}

fn friendly(err: &anyhow::Error) -> String {
    match err.downcast_ref::<ClientError>() {
        Some(ClientError::Transport(_)) => "Sol isn't answering".into(),
        // Sol and this app speak different versions of the protocol.
        Some(ClientError::Api {
            status: 426,
            message,
            ..
        }) => message.clone(),
        Some(ClientError::Api { message, .. }) => message.clone(),
        _ => err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_settings_are_not_written_again() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE writes (n INTEGER);
             CREATE TRIGGER counted AFTER UPDATE ON meta BEGIN INSERT INTO writes VALUES (1); END;",
        )
        .unwrap();
        let writes = |c: &rusqlite::Connection| -> i64 {
            c.query_row("SELECT COUNT(*) FROM writes", [], |r| r.get(0))
                .unwrap()
        };
        assert!(remember_settings(&c, r#"{"a":1}"#).unwrap());
        assert!(!remember_settings(&c, r#"{"a":1}"#).unwrap());
        assert_eq!(writes(&c), 0, "the same settings were written again");
        assert!(remember_settings(&c, r#"{"a":2}"#).unwrap());
        assert_eq!(writes(&c), 1);
    }
}

//! The sync: local writes go to Sol in order, Sol's changes come in, and
//! the settings stay fresh.

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use rusqlite::OptionalExtension;

use super::{
    LONG_POLL, Link, MAX_BACKOFF, SETTINGS_EVERY, State, Update,
    cache::{cursor, meta, set_meta, store, waiting},
};
use crate::client::{Error as ClientError, Sol};

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
    pub(super) async fn apply(&self, page: crate::doc::ChangePage) -> anyhow::Result<()> {
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

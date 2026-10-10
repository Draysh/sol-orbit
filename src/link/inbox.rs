//! The inbox: what other worlds (and Sol's overview) ask of this one.

use std::time::Duration;

use chrono::{DateTime, Utc};

use super::{
    LONG_POLL, Link, MAX_BACKOFF, Update,
    cache::{cursor, meta, set_meta},
    sync::{retry_after, unpaired},
};

impl Link {
    /// Hands every delivery to the world's handler, in order.
    pub(super) async fn inbox_loop(&self) {
        let mut backoff = Duration::from_secs(1);
        while let Some(client) = self.client() {
            let step = async {
                let after = self.0.db.call(|c| cursor(c, "inbox")).await??;
                let page = client.inbox(after, LONG_POLL).await?;
                if !self.follows("inbox", after, page.database.clone()).await? {
                    return anyhow::Ok(());
                }
                // After a restore, deliveries up to the last one handled
                // before it were handled already (or taken by another device).
                let skip_until: Option<DateTime<Utc>> = self
                    .0
                    .db
                    .call(|c| meta(c, "inbox:skip-until"))
                    .await??
                    .and_then(|t| DateTime::parse_from_rfc3339(&t).ok())
                    .map(|t| t.with_timezone(&Utc));
                for delivery in &page.deliveries {
                    if skip_until.is_some_and(|t| delivery.created_at <= t) {
                        // Handled before Sol was restored.
                    } else if delivery.action == "sol.settings" {
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
                    let (n, at) = (delivery.n, delivery.created_at.to_rfc3339());
                    let past = skip_until.is_some_and(|t| delivery.created_at > t);
                    self.0
                        .db
                        .call(move |c| -> rusqlite::Result<()> {
                            set_meta(c, "cursor:inbox", &n.to_string())?;
                            set_meta(c, "inbox:seen", &at)?;
                            if past {
                                c.execute("DELETE FROM meta WHERE key = 'inbox:skip-until'", [])?;
                            }
                            Ok(())
                        })
                        .await??;
                }
                anyhow::Ok(())
            }
            .await;
            match step {
                Ok(()) => backoff = Duration::from_secs(1),
                Err(err) if unpaired(&err) => return self.lost().await,
                Err(err) => match retry_after(&err) {
                    Some(wait) => tokio::time::sleep(wait).await,
                    None => {
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                    }
                },
            }
        }
    }
}

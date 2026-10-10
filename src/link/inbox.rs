//! The inbox: what other worlds (and Sol's overview) ask of this one.

use std::time::Duration;

use super::{
    LONG_POLL, Link, MAX_BACKOFF, Update,
    cache::{cursor, set_meta},
    sync::unpaired,
};

impl Link {
    /// Hands every delivery to the world's handler, in order.
    pub(super) async fn inbox_loop(&self) {
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
}

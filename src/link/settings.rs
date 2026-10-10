//! The world's settings, as Sol last sent them, and following their changes.

use std::{collections::BTreeMap, future::Future};

use tokio::sync::broadcast::error::RecvError;

use super::{Link, Status, Update, cache::meta};

/// Why [`Link::on_settings`] calls back.
#[derive(Debug, Clone, PartialEq)]
pub enum Configure {
    /// Once, when it starts.
    Start,
    /// The world's settings in Sol changed.
    Settings,
    /// Where the app stands with Sol changed: a finished pairing brings
    /// Sol's settings, an unpairing takes them away.
    Status(Status),
    /// Updates came faster than they were read and some were missed (a
    /// first sync is a flood of changes); any of them may have been one of
    /// the others.
    Lagged,
}

impl Link {
    /// The world's settings as set in Sol, from the last sync.
    pub async fn settings(&self) -> anyhow::Result<BTreeMap<String, serde_json::Value>> {
        let json = self.0.db.call(|c| meta(c, "settings")).await??;
        Ok(json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default())
    }

    /// Calls `f` once now and again whenever the world's settings may have
    /// changed, saying why, one call at a time, until the link is gone. It
    /// listens from the moment it is called; spawn the future it returns.
    ///
    /// ```no_run
    /// # async fn run(link: orbit::link::Link) {
    /// use orbit::link::Configure;
    ///
    /// tokio::spawn(link.on_settings(|why| async move {
    ///     if !matches!(why, Configure::Status(_)) {
    ///         // Read `link.settings()` again and set the engine up with them.
    ///     }
    /// }));
    /// # }
    /// ```
    pub fn on_settings<F, Fut>(&self, mut f: F) -> impl Future<Output = ()> + Send + 'static
    where
        F: FnMut(Configure) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send,
    {
        let mut updates = self.subscribe();
        async move {
            f(Configure::Start).await;
            loop {
                let why = match updates.recv().await {
                    Ok(Update::Settings) => Configure::Settings,
                    Ok(Update::Status(status)) => Configure::Status(status),
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => Configure::Lagged,
                    Err(RecvError::Closed) => return,
                };
                f(why).await;
            }
        }
    }
}

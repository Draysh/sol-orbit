//! Pairing with Sol and unpairing: the code, its approval, and starting
//! again from nothing.

use std::time::Duration;

use anyhow::bail;
use chrono::Utc;
use rusqlite::params;

use super::{
    LONG_POLL, Link, State, Status, Update,
    cache::meta,
    token::{forget_token, save_token},
};
use crate::{
    client::Error as ClientError,
    device::{PairStarted, Paired},
};

impl Link {
    /// Starts pairing with the Sol at `server`; the code is in the returned
    /// [`Status::Pairing`]. Finishes by itself once the person approves it.
    pub async fn connect(&self, server: &str) -> anyhow::Result<Status> {
        let server = normalise(server)?;
        let probe = self.sol(&server);
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

    async fn wait_for_approval(&self, server: String, started: PairStarted) {
        let client = self.sol(&server);
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
                tx.execute_batch("DELETE FROM docs; DELETE FROM pending; DELETE FROM meta WHERE key NOT LIKE 'pref:%';")?;
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
            client: self.sol(server).with_token(paired.token),
            user: paired.user,
            online: true,
            error: None,
        };
        self.announce();
        self.spawn_session();
        Ok(())
    }

    /// Back to unpaired: no token, no local copy.
    pub(super) async fn reset(&self, error: Option<String>) -> anyhow::Result<()> {
        let device = self.0.db.call(|c| meta(c, "device")).await??;
        if let Some(device) = device {
            forget_token(&self.0.cfg, &device).await;
        }
        self.0
            .db
            .call(|c| c.execute_batch("DELETE FROM docs; DELETE FROM pending; DELETE FROM meta WHERE key NOT LIKE 'pref:%';"))
            .await??;
        *self.lock() = State::Unpaired { error };
        self.announce();
        let _ = self.0.updates.send(Update::Changed(Vec::new()));
        Ok(())
    }
}

/// `https://sol.example.ts.net/` → `https://sol.example.ts.net`.
pub(super) fn normalise(server: &str) -> anyhow::Result<String> {
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

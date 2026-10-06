//! Keeping a world's app up to date, through Sol.
//!
//! [`Updates`] asks Sol now and then whether a newer version of the app is
//! out ([`crate::update`] describes the exchange), shows it to the person,
//! and on their word (or by itself, when they turned that on) downloads it
//! through Sol, checks its signature against the key built into the app,
//! and installs it:
//!
//! - **Linux, installed by the app itself:** the new binary replaces the old
//!   one in place; it runs from the next start, or at once after
//!   [`Next::Restart`].
//! - **Windows:** the new setup program runs and starts the app again.
//! - **`.deb` / `.rpm`:** left to the package manager; the app says where
//!   the new package is.
//!
//! Checking only touches the network; nothing is written unless an update
//! is installed.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, anyhow, bail};
use base64::Engine;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use serde::Serialize;

use crate::{
    install::{self, App, Here},
    link::{Link, Update},
    update::{Kind, Offer, Query, target},
};

/// How often the app asks Sol, after a first look soon after start-up.
const EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const FIRST_AFTER: Duration = Duration::from_secs(20);
const AUTO_KEY: &str = "pref:update-auto";

/// Where the app stands with updates, for its screens.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    /// The version running now.
    pub current: String,
    /// How this copy was installed, which decides how it updates.
    pub here: Here,
    /// Install updates without asking (only where the app can install them
    /// quietly, i.e. Linux).
    pub auto: bool,
    #[serde(flatten)]
    pub state: State,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum State {
    /// Not asked yet.
    Unknown,
    Checking,
    /// Nothing newer; `message` says why not, when Sol couldn't tell.
    Current {
        latest: Option<String>,
        message: Option<String>,
        checked_at: DateTime<Utc>,
    },
    /// A newer version. `installs_itself` is false for package-manager
    /// copies, which only point the person to the new package.
    Available {
        offer: Offer,
        installs_itself: bool,
        checked_at: DateTime<Utc>,
    },
    Downloading {
        version: String,
        done: u64,
        total: u64,
    },
    /// Installed; it runs from the next start.
    Ready {
        version: String,
    },
    Failed {
        error: String,
    },
}

/// What the app does after [`Updates::install`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "next", content = "path", rename_all = "lowercase")]
pub enum Next {
    /// The new version is in place; [`install::relaunch`] this path and
    /// quit to switch to it now, or carry on and it starts next time.
    Restart(PathBuf),
    /// The setup program is running; quit now so it can replace the app.
    Quit,
}

/// The app's updates. Make one at start-up and run [`Updates::watch`].
pub struct Updates {
    link: Link,
    app: App,
    here: Here,
    status: Mutex<Status>,
    /// One check or install at a time.
    busy: tokio::sync::Mutex<()>,
}

impl Updates {
    pub async fn new(link: Link, app: App) -> Arc<Self> {
        let here = install::here(&app);
        Self::new_at(link, app, here).await
    }

    /// As if this copy were installed `here`; for tests.
    pub async fn new_at(link: Link, app: App, here: Here) -> Arc<Self> {
        let auto = link.pref(AUTO_KEY).await.as_deref() == Some("true");
        Arc::new(Self {
            status: Mutex::new(Status {
                current: app.version.to_owned(),
                here: here.clone(),
                auto,
                state: State::Unknown,
            }),
            link,
            app,
            here,
            busy: tokio::sync::Mutex::new(()),
        })
    }

    pub fn status(&self) -> Status {
        self.status.lock().expect("updates lock").clone()
    }

    /// Asks Sol soon after start-up and then every few hours, installing by
    /// itself when the person turned that on. Run it in the background.
    pub async fn watch(self: Arc<Self>) {
        tokio::time::sleep(FIRST_AFTER).await;
        loop {
            if let Ok(State::Available {
                installs_itself: true,
                ..
            }) = self.check().await
                && self.status().auto
                && self.here.kind() == Some(Kind::Portable)
                && let Err(err) = self.install().await
            {
                tracing::warn!(error = ?err, "couldn't install the update");
            }
            tokio::time::sleep(EVERY).await;
        }
    }

    /// Asks Sol whether there is a newer version.
    pub async fn check(&self) -> anyhow::Result<State> {
        let _busy = self.busy.lock().await;
        if matches!(self.status().state, State::Ready { .. }) {
            return Ok(self.status().state);
        }
        let Some(kind) = self.here.kind() else {
            return Ok(self.set(State::Current {
                latest: None,
                message: Some("A development build doesn't update itself.".into()),
                checked_at: Utc::now(),
            }));
        };
        let Some(client) = self.link.client() else {
            bail!("connect to Sol first; updates come through it");
        };
        let before = self.status().state;
        self.set(State::Checking);
        let query = Query {
            version: self.app.version.to_owned(),
            target: target(),
            kind,
        };
        let check = match client.update(&query).await {
            Ok(check) => check,
            Err(err) => {
                self.set(before);
                return Err(err).context("couldn't ask Sol about updates");
            }
        };
        Ok(self.set(match check.offer {
            Some(offer) => State::Available {
                offer,
                installs_itself: kind.installs_itself(),
                checked_at: Utc::now(),
            },
            None => State::Current {
                latest: check.latest,
                message: check.message,
                checked_at: Utc::now(),
            },
        }))
    }

    /// Downloads the offered version, checks its signature and installs it.
    pub async fn install(&self) -> anyhow::Result<Next> {
        let _busy = self.busy.lock().await;
        let State::Available { offer, .. } = self.status().state else {
            bail!("no update to install; check first");
        };
        match self.install_offer(&offer).await {
            Ok(next) => {
                self.set(State::Ready {
                    version: offer.version.clone(),
                });
                Ok(next)
            }
            Err(err) => {
                self.set(State::Failed {
                    error: format!("{err:#}"),
                });
                Err(err)
            }
        }
    }

    pub async fn set_auto(&self, auto: bool) -> anyhow::Result<Status> {
        self.link
            .set_pref(AUTO_KEY, if auto { "true" } else { "false" })
            .await?;
        self.status.lock().expect("updates lock").auto = auto;
        self.announce();
        Ok(self.status())
    }

    async fn install_offer(&self, offer: &Offer) -> anyhow::Result<Next> {
        let kind = self
            .here
            .kind()
            .context("this copy doesn't update itself")?;
        anyhow::ensure!(
            kind.installs_itself(),
            "this copy came from a package; install the new one the same way"
        );
        let pubkey = self
            .app
            .pubkey
            .context("this build has no key to check updates with, so it won't install any")?;
        let client = self.link.client().context("connect to Sol first")?;

        let res = client.download(&offer.download).await?;
        let total = res.content_length().unwrap_or(offer.size);
        let mut bytes = Vec::with_capacity(total as usize);
        let mut stream = res.bytes_stream();
        let mut shown = 0;
        while let Some(chunk) = stream.next().await {
            bytes.extend_from_slice(&chunk.context("the download broke off")?);
            let done = bytes.len() as u64;
            // About a hundred progress updates in all.
            if done - shown > total / 100 || done == total {
                shown = done;
                self.set(State::Downloading {
                    version: offer.version.clone(),
                    done,
                    total,
                });
            }
        }
        verify(pubkey, &offer.signature, &bytes)?;

        match kind {
            Kind::Portable => {
                let path = match &self.here {
                    Here::Installed { path, .. } | Here::Loose { path } => path.clone(),
                    _ => bail!("can't tell where this copy lives"),
                };
                let binary = install::unpack(&self.app, &bytes)?;
                let at = path.clone();
                tokio::task::spawn_blocking(move || install::replace(&at, &binary)).await??;
                tracing::info!(version = %offer.version, path = %path.display(), "updated");
                Ok(Next::Restart(path))
            }
            Kind::Setup => {
                let setup = std::env::temp_dir().join(&offer.file);
                tokio::fs::write(&setup, &bytes).await?;
                // Passive: a progress window and no questions; /R starts the app again.
                std::process::Command::new(&setup)
                    .args(["/P", "/R"])
                    .spawn()
                    .context("starting the setup program")?;
                Ok(Next::Quit)
            }
            Kind::Deb | Kind::Rpm => unreachable!("checked above"),
        }
    }

    fn set(&self, state: State) -> State {
        self.status.lock().expect("updates lock").state = state.clone();
        self.announce();
        state
    }

    fn announce(&self) {
        self.link.send(Update::App(self.status()));
    }
}

/// Checks a minisign signature made by `tauri signer sign`: the key and the
/// signature are each the base64 of minisign's two-line text form.
pub fn verify(pubkey: &str, signature: &str, data: &[u8]) -> anyhow::Result<()> {
    let text = |b64: &str, what: &str| -> anyhow::Result<String> {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .with_context(|| format!("the {what} isn't base64"))?;
        String::from_utf8(raw).with_context(|| format!("the {what} isn't text"))
    };
    let key = minisign_verify::PublicKey::decode(&text(pubkey, "update key")?)
        .map_err(|e| anyhow!("the update key doesn't parse: {e}"))?;
    let sig = minisign_verify::Signature::decode(&text(signature, "signature")?)
        .map_err(|e| anyhow!("the signature doesn't parse: {e}"))?;
    key.verify(data, &sig, false)
        .map_err(|_| anyhow!("the download's signature doesn't match; not installing it"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Made once with `tauri signer generate` (a throwaway key, no password)
    // and `tauri signer sign` over DATA.
    const PUBKEY: &str = include_str!("../tests/fixtures/update.pub");
    const SIGNATURE: &str = include_str!("../tests/fixtures/update.txt.sig");
    const DATA: &[u8] = include_bytes!("../tests/fixtures/update.txt");

    #[test]
    fn signatures() {
        verify(PUBKEY, SIGNATURE, DATA).unwrap();
        let mut forged = DATA.to_vec();
        forged[0] ^= 1;
        assert!(verify(PUBKEY, SIGNATURE, &forged).is_err());
        assert!(verify("bm90IGEga2V5", SIGNATURE, DATA).is_err());
    }
}

//! Keeping a world's app up to date, through Sol.
//!
//! [`Updates`] asks Sol now and then whether a newer version of the app is
//! out ([`crate::update`] describes the exchange), shows it to the person,
//! and on their word (or by itself, when they turned that on) downloads it
//! through Sol, checks its signature against the key built into the app,
//! and installs it:
//!
//! the new binary replaces the old one in place, and runs from the next
//! start, or at once after [`Next::Restart`]. A copy the app doesn't manage
//! itself (see [`install::Here`]) is only told that there is a newer one.
//!
//! Checking only touches the network; nothing is written unless an update
//! is installed.

use std::{
    path::{Path, PathBuf},
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
    /// Install updates without asking.
    pub auto: bool,
    /// An update that didn't start here and was taken back; installing
    /// that version again is left to the person.
    pub rolled_back: Option<install::RolledBack>,
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
    /// A newer version. `installs_itself` is false for a copy the app
    /// doesn't manage itself.
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
    /// Never returned any more (it was for a Windows setup program); kept
    /// so apps that still match on it build.
    #[deprecated(note = "updates always end in `Next::Restart`")]
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
                rolled_back: install::rolled_back(&app),
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
                offer,
                ..
            }) = self.check().await
                && self.status().auto
                // Not again by itself: it didn't start here last time.
                && self
                    .status()
                    .rolled_back
                    .is_none_or(|r| r.version != offer.version)
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
            "this copy doesn't install updates itself; get the new version the way this one came"
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
        verify_offer(pubkey, offer, &bytes, &self.app, &target())?;

        let path = match &self.here {
            Here::Installed { path, .. } | Here::Loose { path } => path.clone(),
            _ => bail!("can't tell where this copy lives"),
        };
        let binary = install::unpack(&self.app, &bytes)?;
        let (app, at, version) = (self.app, path.clone(), offer.version.clone());
        tokio::task::spawn_blocking(move || put_in_place(&app, &at, &binary, &version)).await??;
        tracing::info!(version = %offer.version, path = %path.display(), "updated");
        Ok(Next::Restart(path))
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

/// Tries the new binary beside the old one (`--version`, which must answer
/// with the offered version and, when it says, the Qt this app runs on),
/// then switches to it, keeping the old one as `<bin>.old`, and writes down
/// that it is on trial until it starts well ([`install::on_start`]).
fn put_in_place(app: &App, path: &Path, binary: &[u8], version: &str) -> anyhow::Result<()> {
    let staged = install::stage(path, binary)?;
    let tried = install::try_start(&staged, TRY_START).and_then(|said| {
        anyhow::ensure!(
            said.contains(version),
            "it says it is {:?}, not {version}",
            said.trim()
        );
        if let (Some(theirs), Some(ours)) = (install::qt_of(&said), app.qt.and_then(qt_parts)) {
            anyhow::ensure!(
                theirs == ours,
                "it was built for Qt {}.{}, and this computer has Qt {}.{}; it would not start here. \
                 It installs once Qt here and the release match (a system update, or a release built for this Qt)",
                theirs.0,
                theirs.1,
                ours.0,
                ours.1
            );
        }
        Ok(())
    });
    if let Err(err) = tried {
        let _ = std::fs::remove_file(&staged);
        return Err(err.context(format!(
            "version {version} didn't start here, so it isn't installed"
        )));
    }
    install::switch(path, &staged)?;
    if let Err(err) = install::begin_trial(app, path, version) {
        tracing::warn!(error = ?err, "couldn't write down the update's trial; it stays without a rollback");
    }
    Ok(())
}

/// How long the new binary gets to answer `--version`.
const TRY_START: Duration = Duration::from_secs(15);

fn qt_parts(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split('.').map(|p| p.trim().parse::<u32>().ok());
    Some((parts.next()??, parts.next()??))
}

/// Checks a download against the offer before anything is installed: the
/// version only goes up, the file is the one for this world, version and
/// platform, and its signature is by one of `pubkeys` and names that file
/// in its trusted comment (so a signed file can't be passed off as another).
pub fn verify_offer(
    pubkeys: &str,
    offer: &Offer,
    data: &[u8],
    app: &App,
    target: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        crate::update::newer(&offer.version, app.version) == Some(true),
        "Sol offered version {}, which isn't newer than {}; not installing it",
        offer.version,
        app.version
    );
    let expected = crate::update::file_name(app.world, &offer.version, target);
    anyhow::ensure!(
        offer.file == expected,
        "the offered file is {}, not {expected}; not installing it",
        offer.file
    );
    let comment = verify_signed(pubkeys, &offer.signature, data)?;
    let signed = comment
        .split('\t')
        .find_map(|part| part.trim().strip_prefix("file:"))
        .unwrap_or_default();
    anyhow::ensure!(
        signed == offer.file,
        "the signature is for {:?}, not {}; not installing it",
        signed,
        offer.file
    );
    Ok(())
}

/// Checks a minisign signature over `data`. The keys and the signature each
/// come as the base64 of minisign's text form: a key file's two lines
/// (`untrusted comment:` and the key) and a `.minisig` file's four.
/// `pubkeys` holds one key or several, one per line; any of them will do.
pub fn verify(pubkeys: &str, signature: &str, data: &[u8]) -> anyhow::Result<()> {
    verify_signed(pubkeys, signature, data).map(|_| ())
}

/// [`verify`], returning the signature's trusted comment.
fn verify_signed(pubkeys: &str, signature: &str, data: &[u8]) -> anyhow::Result<String> {
    let text = |b64: &str, what: &str| -> anyhow::Result<String> {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .with_context(|| format!("the {what} isn't base64"))?;
        String::from_utf8(raw).with_context(|| format!("the {what} isn't text"))
    };
    let keys: Vec<minisign_verify::PublicKey> = pubkeys
        .split_whitespace()
        .filter_map(|line| {
            let key = text(line, "update key").ok()?;
            minisign_verify::PublicKey::decode(&key).ok()
        })
        .collect();
    anyhow::ensure!(!keys.is_empty(), "the update key doesn't parse");
    let sig = minisign_verify::Signature::decode(&text(signature, "signature")?)
        .map_err(|e| anyhow!("the signature doesn't parse: {e}"))?;
    if keys.iter().any(|key| key.verify(data, &sig, false).is_ok()) {
        Ok(sig.trusted_comment().to_owned())
    } else {
        bail!("the download's signature doesn't match; not installing it")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A throwaway minisign key (no password) and its signature over DATA,
    // each file's text in base64.
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

    // A release signed with another throwaway key (Sol's test fixture).
    const RELEASE_KEY: &str = include_str!("../tests/fixtures/release.pub");
    const RELEASE: &[u8] = include_bytes!("../tests/fixtures/sol-terra-0.2.0-linux-x86_64.tar.gz");
    const RELEASE_SIG: &str =
        include_str!("../tests/fixtures/sol-terra-0.2.0-linux-x86_64.tar.gz.sig");

    fn app(version: &'static str) -> App {
        App {
            world: "terra",
            name: "Terra",
            comment: "",
            version,
            wm_class: "terra",
            icons: &[],
            pubkey: None,
            qt: None,
        }
    }

    fn offer(version: &str, file: &str, signature: &str) -> Offer {
        Offer {
            version: version.into(),
            notes: None,
            published_at: None,
            file: file.into(),
            size: 0,
            download: String::new(),
            signature: signature.into(),
        }
    }

    #[test]
    fn several_keys_are_trusted() {
        let both = format!("{PUBKEY}\n{RELEASE_KEY}\n");
        verify(&both, SIGNATURE, DATA).unwrap();
        verify(&both, RELEASE_SIG, RELEASE).unwrap();
        // One key alone, as every world has it today.
        verify(RELEASE_KEY, RELEASE_SIG, RELEASE).unwrap();
        assert!(verify(PUBKEY, RELEASE_SIG, RELEASE).is_err());
    }

    #[test]
    fn an_offer_is_checked_before_it_installs() {
        let file = "sol-terra-0.2.0-linux-x86_64.tar.gz";
        let target = "linux-x86_64";
        let good = offer("0.2.0", file, RELEASE_SIG);
        verify_offer(RELEASE_KEY, &good, RELEASE, &app("0.1.0"), target).unwrap();

        // Never down, nor sideways.
        let err = verify_offer(RELEASE_KEY, &good, RELEASE, &app("0.3.0"), target).unwrap_err();
        assert!(err.to_string().contains("isn't newer"), "{err}");
        assert!(verify_offer(RELEASE_KEY, &good, RELEASE, &app("0.2.0"), target).is_err());
        // A file for another version or platform.
        let other = offer("0.2.1", file, RELEASE_SIG);
        assert!(verify_offer(RELEASE_KEY, &other, RELEASE, &app("0.1.0"), target).is_err());
        assert!(verify_offer(RELEASE_KEY, &good, RELEASE, &app("0.1.0"), "linux-aarch64").is_err());
        // A file signed with the right key, but under another name: the
        // trusted comment says `file:update.txt`.
        let renamed = offer("0.2.0", file, SIGNATURE);
        let err = verify_offer(PUBKEY, &renamed, DATA, &app("0.1.0"), target).unwrap_err();
        assert!(err.to_string().contains("update.txt"), "{err}");
    }
}

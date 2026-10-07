//! Moons that run by themselves.
//!
//! A moon is a world that works for another, its planet (`parent` in its
//! manifest): Titan learns from what Saturn watched, Triton makes stations
//! for Neptune, Luna keeps Terra's evenings. What a moon does arrives as
//! deliveries in its inbox, so it has to be running for anything to happen,
//! and nobody should have to remember to open it. So:
//!
//! - an installed moon writes down where it is ([`settle`]) in a folder
//!   every Sol app on the computer shares;
//! - its planet's app, at start-up, holds a lock for as long as it runs and
//!   starts every moon written down for it with [`BACKGROUND`] ([`start`]);
//! - the moon, started that way, opens no window. It syncs and acts on what
//!   Sol delivers like any copy, and leaves once its planet has been gone a
//!   while ([`planet_gone`]). Opened from the app menu it shows its window,
//!   and while its planet runs, closing the window leaves it working.
//!
//! The person can keep a moon from running with its planet on a computer
//! ([`set_enabled`]); it's on until then.

use std::{
    fs::{File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::install::{self, App, Here};

/// The argument a planet starts its moons with: no window, just the work.
pub const BACKGROUND: &str = "--background";

/// How often a moon in the background looks whether its planet still runs.
const LOOK_EVERY: Duration = Duration::from_secs(20);
/// How long its planet must be gone before it leaves: long enough for what
/// the planet did last to reach the moon through Sol.
const LINGER: Duration = Duration::from_secs(90);

/// What a moon writes down for its planet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    /// The moon's installed binary.
    path: PathBuf,
    /// Whether it runs with its planet.
    #[serde(default = "yes")]
    on: bool,
}

fn yes() -> bool {
    true
}

/// A planet's hold on its "running" lock; it lets go when dropped.
#[derive(Debug)]
pub struct Running(#[allow(dead_code)] File);

/// Whether this process was started by its planet, to work without a window.
pub fn in_background() -> bool {
    std::env::args().skip(1).any(|a| a == BACKGROUND)
}

/// For a planet, at start-up: marks it as running for as long as the
/// returned value lives (keep it for the app's whole life), and starts each
/// of its moons in the background. A moon already running stays as it is.
pub fn start(world: &str) -> Option<Running> {
    let base = base()?;
    let running = hold(&base, world);
    for bin in moons_of(&base, world) {
        if let Err(err) = spawn(&bin) {
            tracing::warn!(moon = %bin.display(), error = ?err, "couldn't start a moon");
        }
    }
    running
}

/// Whether `world`'s app is running on this computer.
pub fn running(world: &str) -> bool {
    base().is_some_and(|base| is_running(&base, world))
}

/// For a moon in the background: resolves once its planet has stopped
/// running and stayed away for a while.
pub async fn planet_gone(parent: &str) {
    let mut gone_since: Option<Instant> = None;
    loop {
        tokio::time::sleep(LOOK_EVERY).await;
        if running(parent) {
            gone_since = None;
        } else if gone_since.get_or_insert_with(Instant::now).elapsed() >= LINGER {
            return;
        }
    }
}

/// For a moon, at start-up: writes down where its installed binary is, so
/// its planet can start it. Development builds and copies that were never
/// installed are left out.
pub fn settle(app: &App, parent: &str) {
    let Here::Installed { path, .. } = install::here(app) else {
        return;
    };
    let Some(base) = base() else { return };
    if let Err(err) = settle_in(&base, parent, app.world, path) {
        tracing::warn!(error = ?err, "couldn't tell the planet where this moon is");
    }
}

/// For a moon just installed from a terminal (`--install`): writes down the
/// installed binary, as [`settle`] does when it starts.
pub fn settle_installed(app: &App, parent: &str, bin: &Path) {
    let Some(base) = base() else { return };
    if let Err(err) = settle_in(&base, parent, app.world, bin.to_owned()) {
        tracing::warn!(error = ?err, "couldn't tell the planet where this moon is");
    }
}

/// Whether the moon runs with its planet on this computer; `None` when this
/// copy can't (a development build, or one that isn't installed).
pub fn enabled(app: &App, parent: &str) -> Option<bool> {
    if !matches!(install::here(app), Here::Installed { .. }) {
        return None;
    }
    let base = base()?;
    Some(read(&entry_path(&base, parent, app.world)).is_none_or(|e| e.on))
}

/// Lets the moon run with its planet on this computer, or not.
pub fn set_enabled(app: &App, parent: &str, on: bool) -> anyhow::Result<()> {
    let Here::Installed { path, .. } = install::here(app) else {
        anyhow::bail!("only an installed copy runs with its planet");
    };
    let base = base().context("no data folder to write to")?;
    write(&entry_path(&base, parent, app.world), &Entry { path, on })
}

/// Takes the moon off every planet's list; for uninstalling.
pub fn unsettle(world: &str) {
    let Some(base) = base() else { return };
    let Ok(planets) = std::fs::read_dir(base.join("moons")) else {
        return;
    };
    for planet in planets.flatten() {
        let _ = std::fs::remove_file(planet.path().join(format!("{world}.json")));
    }
}

// ---- Inside ----------------------------------------------------------------

/// The folder every Sol app on this computer shares, for the person alone.
fn base() -> Option<PathBuf> {
    let var = |key: &str| {
        std::env::var_os(key)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let data = if cfg!(target_os = "windows") {
        var("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|h| h.join("Library/Application Support"))
    } else {
        var("XDG_DATA_HOME").or_else(|| var("HOME").map(|h| h.join(".local/share")))
    };
    data.map(|d| d.join("sol-worlds"))
}

fn entry_path(base: &Path, parent: &str, moon: &str) -> PathBuf {
    base.join("moons").join(parent).join(format!("{moon}.json"))
}

fn lock_path(base: &Path, world: &str) -> PathBuf {
    base.join("running").join(format!("{world}.lock"))
}

fn read(path: &Path) -> Option<Entry> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Writes the entry unless the file already says exactly that.
fn write(path: &Path, entry: &Entry) -> anyhow::Result<()> {
    if read(path).as_ref() == Some(entry) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(entry)?)
        .with_context(|| format!("writing {}", path.display()))
}

fn settle_in(base: &Path, parent: &str, moon: &str, path: PathBuf) -> anyhow::Result<()> {
    let file = entry_path(base, parent, moon);
    let on = read(&file).is_none_or(|e| e.on);
    write(&file, &Entry { path, on })
}

/// The binaries of the moons that run with `world`.
fn moons_of(base: &Path, world: &str) -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir(base.join("moons").join(world)) else {
        return Vec::new();
    };
    dir.flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| read(&e.path()))
        .filter(|e| e.on && e.path.is_file())
        .map(|e| e.path)
        .collect()
}

fn hold(base: &Path, world: &str) -> Option<Running> {
    let path = lock_path(base, world);
    std::fs::create_dir_all(path.parent()?).ok()?;
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .ok()?;
    // A moon looking at this very moment holds it shared for an instant.
    for _ in 0..40 {
        match file.try_lock() {
            Ok(()) => return Some(Running(file)),
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(25)),
            Err(TryLockError::Error(err)) => {
                tracing::warn!(error = ?err, "couldn't mark the app as running");
                return None;
            }
        }
    }
    None
}

fn is_running(base: &Path, world: &str) -> bool {
    let Ok(file) = File::open(lock_path(base, world)) else {
        return false;
    };
    match file.try_lock_shared() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(_)) => false,
    }
}

/// Starts a moon without a window, on its own: no terminal, and a process
/// group of its own, so a Ctrl+C meant for the planet leaves it alone.
fn spawn(bin: &Path) -> std::io::Result<()> {
    let mut cmd = Command::new(bin);
    cmd.arg(BACKGROUND)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn()?;
    // Reaped when it leaves (at once, when one was running already), so it
    // never stays behind as a zombie of the planet.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_planet_finds_its_settled_moons() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let bin = base.join("sol-titan");
        std::fs::write(&bin, b"").unwrap();

        assert!(moons_of(base, "saturn").is_empty());
        settle_in(base, "saturn", "titan", bin.clone()).unwrap();
        assert_eq!(moons_of(base, "saturn"), vec![bin.clone()]);
        assert!(moons_of(base, "terra").is_empty());

        // Turned off, it stays off when it settles again.
        let file = entry_path(base, "saturn", "titan");
        write(
            &file,
            &Entry {
                path: bin.clone(),
                on: false,
            },
        )
        .unwrap();
        settle_in(base, "saturn", "titan", bin.clone()).unwrap();
        assert!(moons_of(base, "saturn").is_empty());
        assert!(!read(&file).unwrap().on);

        // A moon whose binary is gone is skipped.
        write(
            &file,
            &Entry {
                path: bin.clone(),
                on: true,
            },
        )
        .unwrap();
        std::fs::remove_file(&bin).unwrap();
        assert!(moons_of(base, "saturn").is_empty());
    }

    #[test]
    fn settling_again_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        settle_in(base, "neptune", "triton", base.join("sol-triton")).unwrap();
        let file = entry_path(base, "neptune", "triton");
        let before = std::fs::metadata(&file).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        settle_in(base, "neptune", "triton", base.join("sol-triton")).unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            before
        );
    }

    #[test]
    fn running_lasts_as_long_as_the_hold() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        assert!(!is_running(base, "saturn"));
        let held = hold(base, "saturn").expect("the lock");
        assert!(is_running(base, "saturn"));
        assert!(!is_running(base, "terra"));
        // Looking doesn't take it away.
        assert!(is_running(base, "saturn"));
        drop(held);
        assert!(!is_running(base, "saturn"));
        // And the planet can take it again.
        let _again = hold(base, "saturn").expect("the lock again");
        assert!(is_running(base, "saturn"));
    }
}

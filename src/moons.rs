//! Moons beside their planet.
//!
//! A moon is a world that works for another, its planet (`parent` in its
//! manifest): Titan learns from what Saturn watched, Triton makes stations
//! for Neptune, Luna keeps Terra's evenings. Today every moon is carried in
//! its planet's app (its pages in the planet's window, its work in the
//! planet's process). Moons that had an app of their own wrote down where
//! it was in a folder every Sol app on the computer shares, and waited on
//! their planet's "running" lock to leave a few seconds after it; this module
//! keeps what such an app may still use:
//!
//! - a planet's app, at start-up, holds that lock for as long as it runs and
//!   starts every moon still written down for it, but those it carries,
//!   with [`BACKGROUND`] ([`start_except`]);
//! - [`in_background`] tells such a start (no window, just the work);
//! - uninstalling takes a moon off every list ([`unsettle`]).

use std::{
    fs::{File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use serde::{Deserialize, Serialize};

/// The argument a planet starts its moons with: no window, just the work.
pub const BACKGROUND: &str = "--background";

/// What a moon with an app of its own wrote down for its planet.
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
/// of its moons written down for it in the background, leaving out the
/// moons this app carries itself (their pages are in its window, their work
/// in its process). A moon already running stays as it is.
pub fn start_except(world: &str, carried: &[&str]) -> Option<Running> {
    let base = base()?;
    let running = hold(&base, world);
    for bin in moons_to_start(&base, world, carried) {
        if let Err(err) = spawn(&bin, &[BACKGROUND]) {
            tracing::warn!(moon = %bin.display(), error = ?err, "couldn't start a moon");
        }
    }
    running
}

/// Whether `world`'s app is running on this computer.
pub fn running(world: &str) -> bool {
    base().is_some_and(|base| is_running(&base, world))
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
pub(crate) fn base() -> Option<PathBuf> {
    let var = |key: &str| {
        std::env::var_os(key)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let data = var("XDG_DATA_HOME").or_else(|| var("HOME").map(|h| h.join(".local/share")));
    data.map(|d| d.join("sol-worlds"))
}

fn lock_path(base: &Path, world: &str) -> PathBuf {
    base.join("running").join(format!("{world}.lock"))
}

fn read(path: &Path) -> Option<Entry> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// The moons a planet starts: its settled ones but those it carries.
fn moons_to_start(base: &Path, world: &str, carried: &[&str]) -> Vec<PathBuf> {
    moons_of(base, world)
        .into_iter()
        .filter(|bin| {
            let name = bin.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            !carried.iter().any(|m| name == format!("sol-{m}"))
        })
        .collect()
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

/// Starts an app on its own: no terminal, and a process group of its own,
/// so a Ctrl+C meant for this one leaves it alone. With [`BACKGROUND`] a
/// moon opens no window; with no arguments an app shows its window (or,
/// running already, brings it up).
pub(crate) fn spawn(bin: &Path, args: &[&str]) -> std::io::Result<()> {
    let mut cmd = Command::new(bin);
    cmd.args(args)
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
    // never stays behind as a zombie of the one that started it.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a moon with an app of its own wrote down for `parent`.
    fn settle(base: &Path, parent: &str, moon: &str, path: &Path, on: bool) {
        let dir = base.join("moons").join(parent);
        std::fs::create_dir_all(&dir).unwrap();
        let entry = Entry {
            path: path.to_owned(),
            on,
        };
        std::fs::write(
            dir.join(format!("{moon}.json")),
            serde_json::to_vec(&entry).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn a_planet_finds_its_settled_moons() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let bin = base.join("sol-titan");
        std::fs::write(&bin, b"").unwrap();

        assert!(moons_of(base, "saturn").is_empty());
        settle(base, "saturn", "titan", &bin, true);
        assert_eq!(moons_of(base, "saturn"), vec![bin.clone()]);
        assert!(moons_of(base, "terra").is_empty());

        // One turned off stays where it is.
        settle(base, "saturn", "titan", &bin, false);
        assert!(moons_of(base, "saturn").is_empty());

        // A moon whose binary is gone is skipped.
        settle(base, "saturn", "titan", &bin, true);
        std::fs::remove_file(&bin).unwrap();
        assert!(moons_of(base, "saturn").is_empty());
    }

    #[test]
    fn a_planet_never_starts_a_moon_it_carries() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let titan = base.join("sol-titan");
        std::fs::write(&titan, b"").unwrap();
        settle(base, "saturn", "titan", &titan, true);
        assert_eq!(moons_to_start(base, "saturn", &[]), vec![titan.clone()]);
        assert!(moons_to_start(base, "saturn", &["titan"]).is_empty());
        assert_eq!(moons_to_start(base, "saturn", &["luna"]), vec![titan]);
    }

    #[test]
    fn an_entry_written_before_on_existed_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let bin = base.join("sol-luna");
        std::fs::write(&bin, b"").unwrap();
        let folder = base.join("moons").join("terra");
        std::fs::create_dir_all(&folder).unwrap();
        let json = serde_json::json!({ "path": bin });
        std::fs::write(folder.join("luna.json"), json.to_string()).unwrap();
        assert_eq!(moons_of(base, "terra"), vec![bin]);
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

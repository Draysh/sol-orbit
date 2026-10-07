//! Doors between the Sol apps on one computer.
//!
//! Every world is an app of its own, so going from Saturn to Titan, or from
//! Triton to the player that plays its stations, means opening another app.
//! An installed app writes down where its binary is ([`settle`]), in the
//! folder every Sol app on the computer shares (the one moons use too), and
//! any other app can then say which worlds are here ([`installed`]) and open
//! one ([`open`]). Started plainly, an app shows its window; one that is
//! already running (a moon working in the background, say) brings its window
//! up through its single-instance guard instead.
//!
//! Development builds and copies that were never installed write nothing
//! down, so a door only ever leads to an app that is really there.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::{
    install::{self, App, Here},
    moons,
};

/// What an app writes down about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    /// The app's installed binary.
    path: PathBuf,
}

/// For every app, at start-up: writes down where its installed binary is,
/// so the other apps can open it. Development builds and copies that aren't
/// installed are left out.
pub fn settle(app: &App) {
    let Here::Installed { path, .. } = install::here(app) else {
        return;
    };
    let Some(base) = moons::base() else { return };
    if let Err(err) = settle_in(&base, app.world, path) {
        tracing::warn!(error = ?err, "couldn't tell the other apps where this one is");
    }
}

/// For an app just installed from a terminal (`--install`): writes down the
/// installed binary, as [`settle`] does when it starts.
pub fn settle_installed(app: &App, bin: &Path) {
    let Some(base) = moons::base() else { return };
    if let Err(err) = settle_in(&base, app.world, bin.to_owned()) {
        tracing::warn!(error = ?err, "couldn't tell the other apps where this one is");
    }
}

/// Takes the app's door away; for uninstalling.
pub fn leave(world: &str) {
    let Some(base) = moons::base() else { return };
    let _ = std::fs::remove_file(entry_path(&base, world));
}

/// The worlds whose apps are installed on this computer, in no particular order.
pub fn installed() -> Vec<String> {
    moons::base()
        .map(|base| installed_in(&base))
        .unwrap_or_default()
}

/// Whether `world`'s app is installed on this computer.
pub fn here(world: &str) -> bool {
    moons::base().is_some_and(|base| bin_of(&base, world).is_some())
}

/// Opens `world`'s app: starts it, or, when it is running already, has it
/// show its window.
pub fn open(world: &str) -> anyhow::Result<()> {
    let base = moons::base().context("no data folder to look in")?;
    let bin = bin_of(&base, world)
        .with_context(|| format!("{world}'s app isn't installed on this computer"))?;
    moons::spawn(&bin, &[]).with_context(|| format!("starting {}", bin.display()))
}

// ---- Inside ----------------------------------------------------------------

fn entry_path(base: &Path, world: &str) -> PathBuf {
    base.join("apps").join(format!("{world}.json"))
}

fn read(path: &Path) -> Option<Entry> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Writes the entry unless the file already says exactly that.
fn settle_in(base: &Path, world: &str, path: PathBuf) -> anyhow::Result<()> {
    let file = entry_path(base, world);
    let entry = Entry { path };
    if read(&file).as_ref() == Some(&entry) {
        return Ok(());
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(&file, serde_json::to_vec_pretty(&entry)?)
        .with_context(|| format!("writing {}", file.display()))
}

fn bin_of(base: &Path, world: &str) -> Option<PathBuf> {
    read(&entry_path(base, world))
        .map(|e| e.path)
        .filter(|p| p.is_file())
}

fn installed_in(base: &Path) -> Vec<String> {
    let Ok(dir) = std::fs::read_dir(base.join("apps")) else {
        return Vec::new();
    };
    let mut worlds: Vec<String> = dir
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter(|e| read(&e.path()).is_some_and(|entry| entry.path.is_file()))
        .filter_map(|e| Some(e.path().file_stem()?.to_str()?.to_owned()))
        .collect();
    worlds.sort();
    worlds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apps_find_each_other_once_settled() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let saturn = base.join("sol-saturn");
        let titan = base.join("sol-titan");
        std::fs::write(&saturn, b"").unwrap();
        std::fs::write(&titan, b"").unwrap();

        assert!(installed_in(base).is_empty());
        settle_in(base, "saturn", saturn.clone()).unwrap();
        settle_in(base, "titan", titan.clone()).unwrap();
        assert_eq!(installed_in(base), vec!["saturn", "titan"]);
        assert_eq!(bin_of(base, "titan"), Some(titan.clone()));

        // An app whose binary is gone is no door any more.
        std::fs::remove_file(&titan).unwrap();
        assert_eq!(installed_in(base), vec!["saturn"]);
        assert_eq!(bin_of(base, "titan"), None);
    }

    #[test]
    fn settling_again_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        settle_in(base, "terra", base.join("sol-terra")).unwrap();
        let file = entry_path(base, "terra");
        let before = std::fs::metadata(&file).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        settle_in(base, "terra", base.join("sol-terra")).unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            before
        );
    }
}

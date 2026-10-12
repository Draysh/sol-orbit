//! A world's app installing, updating and removing itself.
//!
//! The release's `.tar.gz` holds just the app's binary. Run from wherever it
//! was unpacked, the app offers to install itself for the person alone: the
//! binary to `~/.local/bin/sol-<world>`, its icons and an entry in the app
//! menu, no root needed. Installed that way it can also replace itself with
//! a newer version ([`crate::updates`]). A copy somewhere else the system
//! owns (`/usr`, `/opt`) is left to whoever put it there.

use std::{
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use serde::Serialize;

use crate::update::Kind;

/// What the app knows about itself, for installing and updating.
#[derive(Debug, Clone, Copy)]
pub struct App {
    /// The world, e.g. `terra`.
    pub world: &'static str,
    /// Its name in the app menu, e.g. `Terra`.
    pub name: &'static str,
    /// One line about it, for the app menu.
    pub comment: &'static str,
    /// The version running now.
    pub version: &'static str,
    /// The window class, so the desktop matches windows to the menu entry.
    pub wm_class: &'static str,
    /// PNG icons by size in pixels.
    pub icons: &'static [(u32, &'static [u8])],
    /// The minisign public key release files are signed with: the key
    /// file's text (its comment line and its key line), encoded in base64.
    /// Without one the app never installs updates. Several keys, one per
    /// line, are all trusted (for moving to a new key).
    pub pubkey: Option<&'static str>,
    /// The Qt version this app runs on, e.g. `6.10.1`. An update built for
    /// another Qt (major and minor) isn't installed. `None`: not checked.
    pub qt: Option<&'static str>,
}

impl App {
    /// The binary's name once installed.
    pub fn bin_name(&self) -> String {
        format!("sol-{}", self.world)
    }
}

/// Where this copy of the app came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Here {
    /// A development build; it neither installs nor updates.
    Dev,
    /// Not installed: run from wherever it was unpacked. It can install itself.
    Loose { path: PathBuf },
    /// Installed; `kind` says how it updates (always [`Kind::Portable`]
    /// today).
    Installed { kind: Kind, path: PathBuf },
    /// Somewhere the app doesn't manage itself: a folder the system owns, or
    /// a system other than Linux.
    Unsupported,
}

impl Here {
    /// How a newer version gets here, if it can.
    pub fn kind(&self) -> Option<Kind> {
        match self {
            Self::Installed { kind, .. } => Some(*kind),
            Self::Loose { .. } => Some(Kind::Portable),
            Self::Dev | Self::Unsupported => None,
        }
    }
}

/// Works out where this copy of the app came from.
pub fn here(app: &App) -> Here {
    if cfg!(debug_assertions) {
        return Here::Dev;
    }
    let Ok(exe) = current_exe() else {
        return Here::Unsupported;
    };
    if !cfg!(target_os = "linux") || exe.starts_with("/usr") || exe.starts_with("/opt") {
        return Here::Unsupported;
    }
    match linux::bin(app) {
        Some(bin) if bin == exe => Here::Installed {
            kind: Kind::Portable,
            path: exe,
        },
        _ => Here::Loose { path: exe },
    }
}

/// The running binary's path, even after an update replaced the file.
pub fn current_exe() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    // Linux reports a replaced binary as "<path> (deleted)".
    Ok(
        match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
            Some(path) => PathBuf::from(path),
            None => exe,
        },
    )
}

/// Installs this copy for the person alone and returns where the
/// installed binary is; start that one and quit this one.
pub fn install(app: &App) -> anyhow::Result<PathBuf> {
    if !cfg!(target_os = "linux") {
        bail!("the app installs itself on Linux only");
    }
    let bin = linux::bin(app).context("no home folder to install into")?;
    let exe = current_exe()?;
    if exe != bin {
        replace(&bin, &std::fs::read(&exe)?)?;
    }
    linux::integrate(app, &bin)?;
    Ok(bin)
}

/// Keeps the app-menu entry and icons current; call at start-up. Does
/// nothing unless this copy is the installed one.
pub fn refresh(app: &App) {
    if let Here::Installed {
        kind: Kind::Portable,
        path,
    } = here(app)
        && let Err(err) = linux::integrate(app, &path)
    {
        tracing::warn!(error = ?err, "couldn't update the app-menu entry");
    }
}

/// Takes the installed app away again: binary, icons, menu entry. The data
/// stays in Sol; the local copy goes with `Link::forget`.
pub fn uninstall(app: &App) -> anyhow::Result<()> {
    match here(app) {
        Here::Installed {
            kind: Kind::Portable,
            ..
        } => {
            crate::moons::unsettle(app.world);
            crate::doors::leave(app.world);
            linux::remove(app)
        }
        Here::Unsupported => {
            bail!("this copy belongs to the system; remove it the way it was put there")
        }
        _ => bail!("this copy isn't installed"),
    }
}

/// For a planet whose app now carries `moon`: takes the moon's own
/// installed app away (its menu entry, icons, its place on the planet's
/// list) and leaves `~/.local/bin/<moon's binary>` as a link to `planet`,
/// so whatever starts the moon by name (the desktop's commands, an older
/// planet, another app's door) reaches the planet's app. The moon's data,
/// its token and its settings stay. True when there was something to take
/// away; false when it had been done.
pub fn uninstall_other(moon: &App, planet: &Path) -> anyhow::Result<bool> {
    let Some(bin) = linux::bin(moon) else {
        return Ok(false);
    };
    let linked = std::fs::read_link(&bin).is_ok_and(|to| to == planet);
    if linked && !linux::has_entry(moon) {
        return Ok(false);
    }
    crate::moons::unsettle(moon.world);
    linux::remove(moon)?;
    if let Some(dir) = bin.parent() {
        std::fs::create_dir_all(dir)?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(planet, &bin)
        .with_context(|| format!("linking {} to {}", bin.display(), planet.display()))?;
    Ok(true)
}

/// Starts `bin` once this process has quit (call, then quit).
pub fn relaunch(bin: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        // The app allows one instance, so the new one waits for this one.
        std::process::Command::new("sh")
            .arg("-c")
            .arg(r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; exec "$0""#)
            .arg(bin)
            .arg(std::process::id().to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("starting the app again")?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = bin;
        bail!("start the app again yourself")
    }
}

/// Puts `bytes` at `path` as an executable in one step: written next to it
/// first, then renamed over it, so a failure never leaves half a binary.
/// A running binary can be replaced this way; it keeps running as it was.
pub(crate) fn replace(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let staged = stage(path, bytes)?;
    std::fs::rename(&staged, path).with_context(|| format!("replacing {}", path.display()))?;
    sync_dir(path);
    Ok(())
}

/// Writes `bytes` beside `path` (`.<name>.new`) as an executable, on the
/// disk for good, and returns where; [`switch`] puts it in place.
pub(crate) fn stage(path: &Path, bytes: &[u8]) -> anyhow::Result<PathBuf> {
    use std::io::Write;
    let dir = path.parent().context("no folder to install into")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("app");
    let temp = dir.join(format!(".{name}.new"));
    let mut file =
        std::fs::File::create(&temp).with_context(|| format!("writing {}", temp.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("writing {}", temp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(temp)
}

/// The binary before the last update, kept beside it: `<bin>.old`.
pub fn old_path(bin: &Path) -> PathBuf {
    let mut name = bin.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

/// Puts a [`stage`]d binary in place of `path`, keeping the one there as
/// `<path>.old` for a rollback.
pub(crate) fn switch(path: &Path, staged: &Path) -> anyhow::Result<()> {
    let old = old_path(path);
    if path.exists() {
        let _ = std::fs::remove_file(&old);
        // The same file under a second name; a copy where that can't be.
        if std::fs::hard_link(path, &old).is_err() {
            std::fs::copy(path, &old).with_context(|| format!("keeping {}", old.display()))?;
        }
    }
    std::fs::rename(staged, path).with_context(|| format!("replacing {}", path.display()))?;
    sync_dir(path);
    Ok(())
}

/// Makes a rename in `path`'s folder last through a power cut.
fn sync_dir(path: &Path) {
    if let Some(dir) = path.parent()
        && let Ok(dir) = std::fs::File::open(dir)
    {
        let _ = dir.sync_all();
    }
}

/// Starts `bin --version` and waits up to `timeout` for it to answer; its
/// output, or why it didn't start (a library missing, a crash, no answer).
pub(crate) fn try_start(bin: &Path, timeout: std::time::Duration) -> anyhow::Result<String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {}", bin.display()))?;
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let mut out = String::new();
            let mut err = String::new();
            if let Some(mut o) = child.stdout.take() {
                let _ = o.read_to_string(&mut out);
            }
            if let Some(mut e) = child.stderr.take() {
                let _ = e.read_to_string(&mut err);
            }
            if !status.success() {
                let said = err.lines().chain(out.lines()).next().unwrap_or_default();
                bail!("it stopped at once ({status}): {said}");
            }
            return Ok(out);
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("it didn't answer within {} s", timeout.as_secs());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The Qt version a binary says it was built with in its `--version`
/// line (`Terra 1.0.0 (Qt 6.10.1)`), as major and minor.
pub fn qt_of(said: &str) -> Option<(u32, u32)> {
    let rest = &said[said.find("(Qt ")? + 4..];
    let version = rest.split(')').next()?;
    let mut parts = version.split('.').map(|p| p.trim().parse::<u32>().ok());
    Some((parts.next()??, parts.next()??))
}

// ---- Trying an update, and rolling it back ------------------------------------

/// An update put in place and not yet seen to start well, written down
/// beside the binary (`.<bin>.trial`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Trial {
    /// The version it replaced (in `<bin>.old`).
    pub from: String,
    /// The version put in place.
    pub to: String,
    /// How many times the new version started without saying it started well.
    #[serde(default)]
    pub starts: u32,
    pub at: chrono::DateTime<chrono::Utc>,
}

/// An update that didn't start, and was taken back (`.<bin>.rolled-back`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct RolledBack {
    /// The version that didn't start.
    pub version: String,
    /// The version running again.
    pub back_to: String,
    pub at: chrono::DateTime<chrono::Utc>,
}

/// What [`on_start`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// Nothing to do.
    Normal,
    /// This version was just put in place: call [`started_well`] once it
    /// is up (its window has loaded).
    Trying,
    /// This version started before and never came up, so the one before
    /// it is back in place: start the binary again and leave.
    RolledBack(Trial),
}

fn beside(bin: &Path, end: &str) -> PathBuf {
    let name = bin.file_name().and_then(|n| n.to_str()).unwrap_or("app");
    bin.with_file_name(format!(".{name}.{end}"))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// The running binary, when it is a release build that may be on trial.
fn this_binary() -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        return None;
    }
    current_exe().ok()
}

/// Writes down that version `to` replaced this one at `bin`, to be confirmed.
pub(crate) fn begin_trial(app: &App, bin: &Path, to: &str) -> anyhow::Result<()> {
    let trial = Trial {
        from: app.version.to_owned(),
        to: to.to_owned(),
        starts: 0,
        at: chrono::Utc::now(),
    };
    std::fs::write(beside(bin, "trial"), serde_json::to_vec_pretty(&trial)?)?;
    Ok(())
}

/// For the app, first thing at start-up (before its window): counts this
/// start of a version just put in place, and rolls it back when it already
/// started once and never came up.
pub fn on_start(app: &App) -> Start {
    this_binary().map_or(Start::Normal, |bin| on_start_at(&bin, app))
}

fn on_start_at(bin: &Path, app: &App) -> Start {
    let file = beside(bin, "trial");
    let Some(mut trial) = read_json::<Trial>(&file) else {
        return Start::Normal;
    };
    if trial.to != app.version {
        return Start::Normal;
    }
    if trial.starts == 0 {
        trial.starts = 1;
        let _ = std::fs::write(&file, serde_json::to_vec_pretty(&trial).unwrap_or_default());
        return Start::Trying;
    }
    match roll_back_at(bin, &trial) {
        Ok(()) => Start::RolledBack(trial),
        Err(err) => {
            tracing::error!(error = ?err, "the update didn't start, and couldn't be rolled back");
            let _ = std::fs::remove_file(&file);
            Start::Normal
        }
    }
}

/// For the app, once it has come up after an update: the update stays.
pub fn started_well(app: &App) {
    if let Some(bin) = this_binary() {
        started_well_at(&bin, app);
    }
}

fn started_well_at(bin: &Path, app: &App) {
    let file = beside(bin, "trial");
    if read_json::<Trial>(&file).is_some_and(|t| t.to == app.version) {
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(beside(bin, "rolled-back"));
        tracing::info!(version = app.version, "the update started well");
    }
}

/// For the app whose window failed to load right after an update: puts the
/// version before it back at once (start the binary again, then leave).
/// `None` when this isn't a version on trial.
pub fn roll_back(app: &App) -> Option<Trial> {
    let bin = this_binary()?;
    let trial: Trial = read_json(&beside(&bin, "trial"))?;
    if trial.to != app.version {
        return None;
    }
    match roll_back_at(&bin, &trial) {
        Ok(()) => Some(trial),
        Err(err) => {
            tracing::error!(error = ?err, "couldn't roll the update back");
            None
        }
    }
}

fn roll_back_at(bin: &Path, trial: &Trial) -> anyhow::Result<()> {
    let old = old_path(bin);
    anyhow::ensure!(old.is_file(), "{} isn't there", old.display());
    std::fs::rename(&old, bin).with_context(|| format!("putting {} back", old.display()))?;
    sync_dir(bin);
    let _ = std::fs::remove_file(beside(bin, "trial"));
    let note = RolledBack {
        version: trial.to.clone(),
        back_to: trial.from.clone(),
        at: chrono::Utc::now(),
    };
    std::fs::write(
        beside(bin, "rolled-back"),
        serde_json::to_vec_pretty(&note)?,
    )?;
    tracing::warn!(version = %trial.to, back_to = %trial.from, "the update didn't start; rolled back");
    Ok(())
}

/// The last update that was rolled back here, while the app still runs the
/// version it went back to.
pub fn rolled_back(app: &App) -> Option<RolledBack> {
    rolled_back_at(&this_binary()?, app)
}

fn rolled_back_at(bin: &Path, app: &App) -> Option<RolledBack> {
    read_json::<RolledBack>(&beside(bin, "rolled-back")).filter(|n| n.back_to == app.version)
}

/// The app's binary out of a release's `.tar.gz`.
pub(crate) fn unpack(app: &App, archive: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let want = app.bin_name();
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if entry.header().entry_type().is_file()
            && path.file_name().and_then(|n| n.to_str()) == Some(want.as_str())
        {
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut bytes)?;
            return Ok(bytes);
        }
    }
    bail!("the download has no {want} in it")
}

mod linux {
    use std::path::{Path, PathBuf};

    use anyhow::Context;

    use super::App;

    fn home() -> Option<PathBuf> {
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
    }

    fn data() -> Option<PathBuf> {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".local/share")))
    }

    pub fn bin(app: &App) -> Option<PathBuf> {
        home().map(|h| h.join(".local/bin").join(app.bin_name()))
    }

    fn entry(app: &App) -> Option<PathBuf> {
        data().map(|d| {
            d.join("applications")
                .join(format!("{}.desktop", app.bin_name()))
        })
    }

    fn icon(size: u32, app: &App) -> Option<PathBuf> {
        data().map(|d| {
            d.join(format!(
                "icons/hicolor/{size}x{size}/apps/{}.png",
                app.bin_name()
            ))
        })
    }

    /// Writes `bytes` to `path` unless it already holds exactly that.
    fn put(path: &Path, bytes: &[u8]) -> anyhow::Result<bool> {
        if std::fs::read(path).is_ok_and(|b| b == bytes) {
            return Ok(false);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
        Ok(true)
    }

    pub fn integrate(app: &App, bin: &Path) -> anyhow::Result<()> {
        for (size, png) in app.icons {
            if let Some(path) = icon(*size, app) {
                put(&path, png)?;
            }
        }
        let desktop = format!(
            "[Desktop Entry]\nType=Application\nName={}\nComment={}\nExec=\"{}\"\nIcon={}\nTerminal=false\nCategories=Utility;\nStartupWMClass={}\n",
            app.name,
            app.comment,
            bin.display(),
            app.bin_name(),
            app.wm_class,
        );
        let path = entry(app).context("no data folder for the app menu")?;
        if put(&path, desktop.as_bytes())? {
            // Not every desktop needs this; where it exists it refreshes menus at once.
            let _ = std::process::Command::new("update-desktop-database")
                .arg("-q")
                .arg(path.parent().unwrap_or(&path))
                .status();
        }
        Ok(())
    }

    /// Whether the app has its menu entry.
    pub fn has_entry(app: &App) -> bool {
        entry(app).is_some_and(|p| p.exists())
    }

    pub fn remove(app: &App) -> anyhow::Result<()> {
        let mut paths: Vec<PathBuf> = app
            .icons
            .iter()
            .filter_map(|(size, _)| icon(*size, app))
            .collect();
        paths.extend(entry(app));
        paths.extend(bin(app));
        for path in paths {
            match std::fs::remove_file(&path) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                    return Err(err).with_context(|| format!("removing {}", path.display()));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: App = App {
        world: "terra",
        name: "Terra",
        comment: "Days, habits and how you feel",
        version: "0.1.0",
        wm_class: "terra",
        icons: &[(128, b"png")],
        pubkey: None,
        qt: None,
    };

    fn tarball(name: &str, body: &[u8]) -> Vec<u8> {
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, name, body).unwrap();
        tar.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn unpacks_the_binary() {
        let archive = tarball("sol-terra", b"\x7fELF new");
        assert_eq!(unpack(&APP, &archive).unwrap(), b"\x7fELF new");
        assert!(unpack(&APP, &tarball("something-else", b"x")).is_err());
    }

    #[test]
    fn replaces_in_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin/sol-terra");
        replace(&path, b"one").unwrap();
        replace(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    #[test]
    fn an_update_keeps_the_old_binary_and_rolls_back_if_it_never_comes_up() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin/sol-terra");
        replace(&bin, b"0.1.0").unwrap();
        let staged = stage(&bin, b"0.2.0").unwrap();
        switch(&bin, &staged).unwrap();
        assert_eq!(std::fs::read(&bin).unwrap(), b"0.2.0");
        assert_eq!(std::fs::read(old_path(&bin)).unwrap(), b"0.1.0");
        begin_trial(&APP, &bin, "0.2.0").unwrap();

        // The old version, still running, leaves it alone.
        assert_eq!(on_start_at(&bin, &APP), Start::Normal);
        let new = App {
            version: "0.2.0",
            ..APP
        };
        // The new one's first start is counted; it never says it came up.
        assert_eq!(on_start_at(&bin, &new), Start::Trying);
        // So the next start puts 0.1.0 back.
        assert!(matches!(on_start_at(&bin, &new), Start::RolledBack(t) if t.from == "0.1.0"));
        assert_eq!(std::fs::read(&bin).unwrap(), b"0.1.0");
        assert!(!old_path(&bin).exists());
        let note = rolled_back_at(&bin, &APP).expect("a note");
        assert_eq!(
            (note.version.as_str(), note.back_to.as_str()),
            ("0.2.0", "0.1.0")
        );
        // And it is quiet from then on.
        assert_eq!(on_start_at(&bin, &APP), Start::Normal);
    }

    #[test]
    fn an_update_that_comes_up_stays() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin/sol-terra");
        replace(&bin, b"0.1.0").unwrap();
        switch(&bin, &stage(&bin, b"0.2.0").unwrap()).unwrap();
        begin_trial(&APP, &bin, "0.2.0").unwrap();
        let new = App {
            version: "0.2.0",
            ..APP
        };
        assert_eq!(on_start_at(&bin, &new), Start::Trying);
        started_well_at(&bin, &new);
        assert_eq!(on_start_at(&bin, &new), Start::Normal);
        assert_eq!(on_start_at(&bin, &new), Start::Normal);
        assert_eq!(std::fs::read(&bin).unwrap(), b"0.2.0");
        assert!(rolled_back_at(&bin, &new).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_new_binary_is_tried_before_it_goes_in() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("sol-terra");
        let t = std::time::Duration::from_secs(5);
        let good = stage(&bin, b"#!/bin/sh\necho 'Terra 0.2.0 (Qt 6.10.1)'\n").unwrap();
        let said = try_start(&good, t).unwrap();
        assert_eq!(qt_of(&said), Some((6, 10)));
        let broken = stage(
            &bin,
            b"#!/bin/sh\necho 'libQt6Core.so.6: version Qt_6.11 not found' >&2\nexit 127\n",
        )
        .unwrap();
        let err = try_start(&broken, t).unwrap_err().to_string();
        assert!(err.contains("Qt_6.11"), "{err}");
        let stuck = stage(&bin, b"#!/bin/sh\nsleep 30\n").unwrap();
        assert!(try_start(&stuck, std::time::Duration::from_millis(300)).is_err());
        assert_eq!(qt_of("Terra 0.1.0"), None);
    }

    #[test]
    fn dev_builds_stay_put() {
        assert_eq!(here(&APP), Here::Dev);
        assert_eq!(Here::Dev.kind(), None);
    }
}

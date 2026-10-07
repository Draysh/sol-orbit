//! A world's app installing, updating and removing itself.
//!
//! On Linux the release's `.tar.gz` holds just the app's binary. Run from
//! wherever it was unpacked, the app offers to install itself for the
//! person alone: the binary to `~/.local/bin/sol-<world>`, its icons and an
//! entry in the app menu, no root needed. Installed that way it can also
//! replace itself with a newer version ([`crate::updates`]). Copies from a
//! `.deb` or `.rpm` belong to the package manager and are left to it.
//!
//! On Windows the setup program installs the app, and runs again to update.

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
    /// The public key release files are signed with (`tauri signer
    /// generate` writes it); without one the app never installs updates.
    pub pubkey: Option<&'static str>,
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
    /// Installed; `kind` says how it updates.
    Installed { kind: Kind, path: PathBuf },
    /// A platform where the app can't manage itself.
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
    if cfg!(target_os = "windows") {
        return Here::Installed {
            kind: Kind::Setup,
            path: exe,
        };
    }
    if !cfg!(target_os = "linux") {
        return Here::Unsupported;
    }
    if exe.starts_with("/usr") || exe.starts_with("/opt") {
        let kind = if Path::new("/var/lib/dpkg/status").exists() {
            Kind::Deb
        } else {
            Kind::Rpm
        };
        return Here::Installed { kind, path: exe };
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

/// Installs this copy for the person alone (Linux) and returns where the
/// installed binary is; start that one and quit this one.
pub fn install(app: &App) -> anyhow::Result<PathBuf> {
    if !cfg!(target_os = "linux") {
        bail!("on this system, the app's setup program installs it");
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
        Here::Installed { kind, .. } => {
            bail!("this copy came from a {kind:?} package; remove it the way it was installed")
        }
        _ => bail!("this copy isn't installed"),
    }
}

/// Starts `bin` once this process has quit (call, then quit). Linux; on
/// Windows the setup program starts the app again by itself.
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
    let dir = path.parent().context("no folder to install into")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("app");
    let temp = dir.join(format!(".{name}.new"));
    std::fs::write(&temp, bytes).with_context(|| format!("writing {}", temp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&temp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
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
    fn dev_builds_stay_put() {
        assert_eq!(here(&APP), Here::Dev);
        assert_eq!(Here::Dev.kind(), None);
    }
}

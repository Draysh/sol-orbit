//! Updates for the worlds' apps, handed out by Sol.
//!
//! The worlds' repositories can be private, so apps never ask GitHub
//! themselves. They ask Sol (`GET /api/v1/update`) with their version, their
//! target and how they were installed; Sol looks at the world's latest
//! GitHub release with its own token and offers the matching file, which the
//! app then downloads through Sol (`GET /api/v1/update/download/{asset}`).
//!
//! Every file is signed on the person's own computer with their update key,
//! a minisign key, and the app checks the signature against the public key
//! built into it before installing anything, so neither Sol nor GitHub has
//! to be trusted with what runs on the device.
//!
//! Release files are named `sol-<world>-<version>-<target><suffix>`, e.g.
//! `sol-terra-0.2.0-linux-x86_64.tar.gz`, with the signature next to each as
//! `<file>.sig`. What ships is the Linux `.tar.gz`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// How a copy of an app was installed, which decides how it updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// The app's own binary, installed by itself (Linux): it updates itself
    /// from a `.tar.gz`.
    Portable,
    /// A kind this build doesn't know (an older one, such as `deb`, or a
    /// newer one); Sol offers no file for it.
    #[serde(other)]
    Unknown,
}

impl Kind {
    /// How the release file for this kind ends; `None` for [`Kind::Unknown`].
    pub fn suffix(self) -> Option<&'static str> {
        match self {
            Self::Portable => Some(".tar.gz"),
            Self::Unknown => None,
        }
    }

    /// Whether the app can install the update by itself.
    pub fn installs_itself(self) -> bool {
        self == Self::Portable
    }

    pub fn of_file(name: &str) -> Option<Self> {
        [Self::Portable]
            .into_iter()
            .find(|k| k.suffix().is_some_and(|s| name.ends_with(s)))
    }
}

/// The platform an app runs on, as release files name it: `linux-x86_64`
/// or `linux-aarch64`.
pub fn target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// The file name a release carries for one world, version and target.
pub fn file_name(world: &str, version: &str, target: &str) -> String {
    format!("sol-{world}-{version}-{target}.tar.gz")
}

/// `GET /api/v1/update`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct Query {
    /// The version running now, e.g. `0.1.0`.
    pub version: String,
    /// See [`target`].
    pub target: String,
    pub kind: Kind,
}

/// Sol's answer to an app asking for updates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Check {
    /// The newest version released, when Sol could find out.
    pub latest: Option<String>,
    /// A newer version for this app, when there is one.
    pub offer: Option<Offer>,
    /// Why there is no offer, for people (no release yet, GitHub unreachable,
    /// no file for this platform, …); `None` when the app is up to date.
    pub message: Option<String>,
}

/// A newer version, ready to download through Sol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Offer {
    pub version: String,
    /// The release notes, in Markdown.
    pub notes: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    /// The release file's name.
    pub file: String,
    pub size: u64,
    /// Path on Sol to download it from, with the device's token.
    pub download: String,
    /// The file's minisign signature: the `.minisig` text (two comment
    /// lines and two base64 lines), itself encoded in base64.
    pub signature: String,
}

/// Compares two versions (`v` prefix allowed); `None` if either doesn't parse.
pub fn newer(candidate: &str, than: &str) -> Option<bool> {
    let parse = |v: &str| semver::Version::parse(v.trim().trim_start_matches('v')).ok();
    Some(parse(candidate)? > parse(than)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_kinds() {
        let name = file_name("terra", "0.2.0", "linux-x86_64");
        assert_eq!(name, "sol-terra-0.2.0-linux-x86_64.tar.gz");
        assert_eq!(Kind::of_file(&name), Some(Kind::Portable));
        assert_eq!(
            Kind::of_file("sol-terra-0.2.0-windows-x86_64-setup.exe"),
            None
        );
        assert_eq!(
            Kind::of_file("sol-terra-0.2.0-linux-x86_64.tar.gz.sig"),
            None
        );
    }

    #[test]
    fn kinds_this_build_does_not_know() {
        for old in ["\"deb\"", "\"setup\"", "\"flatpak\""] {
            assert_eq!(serde_json::from_str::<Kind>(old).unwrap(), Kind::Unknown);
        }
        assert_eq!(Kind::Unknown.suffix(), None);
        assert!(!Kind::Unknown.installs_itself());
    }

    #[test]
    fn versions() {
        assert_eq!(newer("v0.2.0", "0.1.9"), Some(true));
        assert_eq!(newer("0.10.0", "0.9.0"), Some(true));
        assert_eq!(newer("0.2.0", "0.2.0"), Some(false));
        assert_eq!(newer("0.2.0-rc.1", "0.2.0"), Some(false));
        assert_eq!(newer("latest", "0.2.0"), None);
    }
}

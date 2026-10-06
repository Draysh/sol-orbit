//! Where a world's app keeps its Sol token.
//!
//! The token goes to an operating-system credential store, which encrypts it
//! at rest and unlocks it with the desktop session. This module picks a store
//! that actually works on the machine it finds itself on. (From Asonica,
//! which keeps its Navidrome password the same way.)
//!
//! # Why there is more than one
//!
//! The standard Linux answer is the **Secret Service** D-Bus API, at the name
//! `org.freedesktop.secrets`. One process owns that name, and on a desktop
//! with more than one keyring installed, it is whichever started first.
//!
//! On KDE that is frequently `gnome-keyring-daemon` — a GNOME component,
//! often pulled in as somebody's dependency and socket-activated early. It
//! claims the name and then cannot serve it, because the keyring it would
//! serve from (`login`) is unlocked by GNOME's PAM module, and a KDE session
//! runs `plasma-kwallet-pam` instead. The result is a keyring that answers
//! the door and has nothing behind it:
//!
//! ```text
//! org.freedesktop.DBus.Error.UnknownMethod:
//!   Object does not exist at path "/org/freedesktop/secrets/collection/login"
//! ```
//!
//! Meanwhile the store that *is* unlocked — KWallet — is running perfectly
//! well next door under a different name. So rather than telling the user to
//! go and mask a systemd unit, we knock on the second door.
//!
//! # Order, and what counts as failure
//!
//! [`Backend::SecretService`] is tried first because it is the portable one:
//! the `keyring` crate maps it to the Secret Service on Linux, the Credential
//! Manager on Windows, and the Keychain on macOS. [`Backend::KWallet`] is a
//! Linux-only fallback and is compiled out everywhere else.
//!
//! The distinction that makes this work is between *"the store works and has
//! nothing for you"* and *"the store is broken"*. Only the second falls
//! through to the next backend — otherwise a first run, where nothing is
//! stored yet, would silently save into the wrong place.

/// Identifies Sol's entries, so they cannot collide with another app's.
const SERVICE: &str = "sol";

/// A name we only ever write and erase, to ask a store whether it is working.
const PROBE_USER: &str = "sol-availability-probe";

/// Shown by KWallet when it asks whether to grant access.
const APP_ID: &str = "Sol";

/// Which credential store a password lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// The platform's own: Secret Service on Linux, Credential Manager on
    /// Windows, Keychain on macOS.
    SecretService,
    /// KWallet, reached directly over D-Bus. Linux only, and only used when
    /// the Secret Service is unreachable.
    KWallet,
}

impl Backend {
    /// A name to show a person, rather than a protocol name.
    pub fn label(self) -> &'static str {
        match self {
            Backend::SecretService => "the system keyring",
            Backend::KWallet => "KWallet",
        }
    }
}

/// Every credential store failed. Carries what each one said, because when
/// this happens the reason is always environmental and always specific.
#[derive(Debug)]
pub struct Unavailable {
    pub reasons: Vec<(Backend, String)>,
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.reasons.is_empty() {
            return write!(f, "no credential store is available on this system");
        }
        write!(f, "no credential store would accept the secret")?;
        for (backend, why) in &self.reasons {
            write!(f, "; {}: {why}", backend.label())?;
        }
        Ok(())
    }
}

impl std::error::Error for Unavailable {}

/// Saves a secret, returning the store that took it.
pub fn save(user: &str, password: &str) -> Result<Backend, Unavailable> {
    let mut reasons = Vec::new();
    for backend in backends() {
        match write(backend, user, password) {
            Ok(()) => return Ok(backend),
            Err(why) => reasons.push((backend, why)),
        }
    }
    Err(Unavailable { reasons })
}

/// Reads a saved secret, or `None` if nothing is stored for `user`.
///
/// A missing entry is not an error — it is the ordinary first-run case, and
/// the caller shows the sign-in screen.
///
/// **Every** store is asked, even after one answers "nothing here". A store
/// can be half-working: the broken `gnome-keyring` case that this module
/// exists for answers reads with an empty result while refusing writes, so
/// the password ends up in KWallet while the Secret Service still politely
/// says it has none. Stopping at the first answer would never find it.
pub fn load(user: &str) -> Option<String> {
    backends()
        .into_iter()
        .find_map(|backend| read(backend, user).ok().flatten())
}

/// Forgets the saved secret, in whichever store holds it.
///
/// Every store is asked, not just the first that works: a password saved
/// before the environment changed could be sitting in the other one, and
/// "sign me out" has to mean it.
pub fn forget(user: &str) -> Result<(), Unavailable> {
    let mut reasons = Vec::new();
    let mut any = false;
    for backend in backends() {
        match erase(backend, user) {
            Ok(()) => any = true,
            Err(why) => reasons.push((backend, why)),
        }
    }
    if any {
        Ok(())
    } else {
        Err(Unavailable { reasons })
    }
}

/// Which store would be used, or `None` if nothing can be saved at all on
/// this machine.
///
/// This *writes* a probe entry and immediately erases it, rather than merely
/// reading. A read probe is not good enough: the broken `gnome-keyring` case
/// answers reads happily and only fails on writes, so a read probe would
/// report a store that cannot actually keep anything. Saving is what the
/// offer promises, so saving is what gets tested.
///
/// Not cheap — it is a couple of D-Bus round trips — so call it once, off the
/// UI thread.
pub fn available() -> Option<Backend> {
    backends().into_iter().find(|&backend| {
        let works = write(backend, PROBE_USER, "probe").is_ok();
        if works {
            let _ = erase(backend, PROBE_USER);
        }
        works
    })
}

/// Tried in order. KWallet only exists on Linux.
fn backends() -> Vec<Backend> {
    #[cfg(target_os = "linux")]
    {
        vec![Backend::SecretService, Backend::KWallet]
    }
    #[cfg(not(target_os = "linux"))]
    {
        vec![Backend::SecretService]
    }
}

fn write(backend: Backend, user: &str, password: &str) -> Result<(), String> {
    match backend {
        Backend::SecretService => secret_service::write(user, password),
        #[cfg(target_os = "linux")]
        Backend::KWallet => kwallet::write(user, password),
        #[cfg(not(target_os = "linux"))]
        Backend::KWallet => Err("not available on this platform".into()),
    }
}

/// `Ok(None)` means the store works but holds nothing; `Err` means the store
/// itself could not be reached.
fn read(backend: Backend, user: &str) -> Result<Option<String>, String> {
    match backend {
        Backend::SecretService => secret_service::read(user),
        #[cfg(target_os = "linux")]
        Backend::KWallet => kwallet::read(user),
        #[cfg(not(target_os = "linux"))]
        Backend::KWallet => Err("not available on this platform".into()),
    }
}

fn erase(backend: Backend, user: &str) -> Result<(), String> {
    match backend {
        Backend::SecretService => secret_service::erase(user),
        #[cfg(target_os = "linux")]
        Backend::KWallet => kwallet::erase(user),
        #[cfg(not(target_os = "linux"))]
        Backend::KWallet => Err("not available on this platform".into()),
    }
}

/// The portable path, via the `keyring` crate.
mod secret_service {
    use super::SERVICE;

    fn entry(user: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, user).map_err(|e| e.to_string())
    }

    pub fn write(user: &str, password: &str) -> Result<(), String> {
        entry(user)?
            .set_password(password)
            .map_err(|e| e.to_string())
    }

    pub fn read(user: &str) -> Result<Option<String>, String> {
        match entry(user)?.get_password() {
            Ok(password) => Ok(Some(password)),
            // The store answered, and the answer is "nothing here".
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn erase(user: &str) -> Result<(), String> {
        match entry(user)?.delete_credential() {
            // Already gone is the desired end state, not a failure.
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// KWallet over D-Bus.
///
/// Deliberately not the Secret Service protocol, even though KDE also speaks
/// that at `org.kde.secretservicecompat`: KWallet's own interface needs no
/// session negotiation and no prompt handling, because `open` puts KWallet's
/// own unlock dialog on screen and returns when the user has answered.
#[cfg(target_os = "linux")]
mod kwallet {
    use super::{APP_ID, SERVICE};

    #[zbus::proxy(
        interface = "org.kde.KWallet",
        default_service = "org.kde.kwalletd6",
        default_path = "/modules/kwalletd6"
    )]
    trait KWallet {
        /// KWallet's methods are camelCase, which is not what zbus derives
        /// from a snake_case Rust name, so each one is spelled out.
        #[zbus(name = "localWallet")]
        fn local_wallet(&self) -> zbus::Result<String>;

        /// `wid` is a parent window id for the unlock dialog; 0 means none.
        #[zbus(name = "open")]
        fn open(&self, wallet: &str, wid: i64, appid: &str) -> zbus::Result<i32>;

        #[zbus(name = "createFolder")]
        fn create_folder(&self, handle: i32, folder: &str, appid: &str) -> zbus::Result<bool>;

        #[zbus(name = "hasFolder")]
        fn has_folder(&self, handle: i32, folder: &str, appid: &str) -> zbus::Result<bool>;

        #[zbus(name = "hasEntry")]
        fn has_entry(
            &self,
            handle: i32,
            folder: &str,
            key: &str,
            appid: &str,
        ) -> zbus::Result<bool>;

        #[zbus(name = "writePassword")]
        fn write_password(
            &self,
            handle: i32,
            folder: &str,
            key: &str,
            value: &str,
            appid: &str,
        ) -> zbus::Result<i32>;

        #[zbus(name = "readPassword")]
        fn read_password(
            &self,
            handle: i32,
            folder: &str,
            key: &str,
            appid: &str,
        ) -> zbus::Result<String>;

        #[zbus(name = "removeEntry")]
        fn remove_entry(
            &self,
            handle: i32,
            folder: &str,
            key: &str,
            appid: &str,
        ) -> zbus::Result<i32>;
    }

    /// Opens the user's wallet and runs `f` against it.
    ///
    /// The handle is not closed afterwards: KWallet keeps a wallet open for
    /// the session on behalf of every app using it, and closing ours would
    /// be closing theirs.
    fn with_wallet<T>(
        f: impl FnOnce(&KWalletProxyBlocking<'_>, i32) -> Result<T, String>,
    ) -> Result<T, String> {
        let connection = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
        let wallet = KWalletProxyBlocking::new(&connection).map_err(|e| e.to_string())?;

        let name = wallet.local_wallet().map_err(|e| e.to_string())?;
        let handle = wallet.open(&name, 0, APP_ID).map_err(|e| e.to_string())?;
        if handle < 0 {
            return Err(format!("KWallet would not open the wallet '{name}'"));
        }

        f(&wallet, handle)
    }

    pub fn write(user: &str, password: &str) -> Result<(), String> {
        with_wallet(|wallet, handle| {
            if !wallet
                .has_folder(handle, SERVICE, APP_ID)
                .map_err(|e| e.to_string())?
            {
                wallet
                    .create_folder(handle, SERVICE, APP_ID)
                    .map_err(|e| e.to_string())?;
            }
            match wallet.write_password(handle, SERVICE, user, password, APP_ID) {
                // KWallet reports success as 0.
                Ok(0) => Ok(()),
                Ok(code) => Err(format!("KWallet refused to store the secret (code {code})")),
                Err(e) => Err(e.to_string()),
            }
        })
    }

    pub fn read(user: &str) -> Result<Option<String>, String> {
        with_wallet(|wallet, handle| {
            // `readPassword` returns an empty string for a missing entry, so
            // ask first rather than guessing what "" meant.
            if !wallet
                .has_entry(handle, SERVICE, user, APP_ID)
                .map_err(|e| e.to_string())?
            {
                return Ok(None);
            }
            wallet
                .read_password(handle, SERVICE, user, APP_ID)
                .map(Some)
                .map_err(|e| e.to_string())
        })
    }

    pub fn erase(user: &str) -> Result<(), String> {
        with_wallet(|wallet, handle| {
            if !wallet
                .has_entry(handle, SERVICE, user, APP_ID)
                .map_err(|e| e.to_string())?
            {
                // Already gone is the desired end state.
                return Ok(());
            }
            wallet
                .remove_entry(handle, SERVICE, user, APP_ID)
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_service_is_preferred() {
        // Order is the policy: the portable store wins when both work, so a
        // password does not migrate into KWallet on a machine where the
        // standard keyring is fine.
        assert_eq!(backends().first(), Some(&Backend::SecretService));
    }

    #[test]
    fn kwallet_is_linux_only() {
        let has_kwallet = backends().contains(&Backend::KWallet);
        assert_eq!(has_kwallet, cfg!(target_os = "linux"));
    }

    #[test]
    fn unavailable_explains_every_store_it_tried() {
        let error = Unavailable {
            reasons: vec![(Backend::SecretService, "no such object".into())],
        };
        let text = error.to_string();
        assert!(text.contains("the system keyring"), "{text}");
        assert!(text.contains("no such object"), "{text}");
    }
}

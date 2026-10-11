//! Orbit: the protocol between Sol and its worlds.
//!
//! Sol is the one server. It keeps every world's data, the person's settings
//! and the connections between worlds. Each world is an app of its own (a
//! desktop app) that pairs with Sol and works with its data
//! there, the way a music player works with a Navidrome server.
//!
//! This crate holds what both sides share: the shapes on the wire
//! ([`world`], [`doc`], [`event`], [`device`], [`widget`], [`update`]), the SQLite actor
//! ([`db`]) and logging ([`telemetry`]). Features add the app side:
//! `client` the HTTP client ([`client::Sol`]), `keystore` the system keyring,
//! and `app` both plus [`link::Link`], the whole engine a world's app runs on,
//! with [`clock`] for the person's day, [`install`] and [`updates`] for
//! installing and updating itself,
//! [`moons`] for moons that run with their planet, and [`doors`] for opening
//! the other worlds' apps on the same computer. `testing` adds
//! the `testing` module: an offline link, a stand-in Sol and a canned HTTP server.
//! `docs/protocol.md` describes the protocol.

pub mod db;
pub mod device;
pub mod doc;
pub mod error;
pub mod event;
pub mod telemetry;
pub mod update;
pub mod widget;
pub mod world;

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "app")]
pub mod clock;
#[cfg(feature = "app")]
pub mod doors;
#[cfg(feature = "app")]
pub mod install;
#[cfg(feature = "keystore")]
pub mod keystore;
#[cfg(feature = "app")]
pub mod link;
#[cfg(feature = "app")]
pub mod moons;
#[cfg(all(feature = "app", any(test, feature = "testing")))]
pub mod testing;
#[cfg(feature = "app")]
pub mod updates;

pub use db::Actor;
pub use doc::Doc;
pub use error::ErrorBody;
pub use event::Envelope;
pub use widget::WidgetView;
pub use world::WorldManifest;

/// Version of the protocol described in `docs/protocol.md`. Protocol 2 is
/// Sol 1.0's, frozen; the number goes up only when a change breaks apps or
/// Sols that speak an older one (see "Compatibility" there).
pub const PROTOCOL: u32 = 2;

/// Header an app sends with every request to `/api/v1`: the protocol it
/// speaks. Sol requires it.
pub const PROTOCOL_HEADER: &str = "sol-protocol";
/// Header an app sends with every request: its own version, e.g. `0.2.0`.
pub const VERSION_HEADER: &str = "sol-app-version";

//! Orbit: the protocol between Sol and its worlds.
//!
//! Sol is the one server. It keeps every world's data, the person's settings
//! and the connections between worlds. Each world is an app of its own (a
//! desktop app, later a phone app) that pairs with Sol and works with its data
//! there, the way a music player works with a Navidrome server.
//!
//! This crate holds what both sides share: the shapes on the wire
//! ([`world`], [`doc`], [`event`], [`device`], [`widget`], [`update`]), the SQLite actor
//! ([`db`]) and logging ([`telemetry`]). Features add the app side:
//! `client` the HTTP client ([`client::Sol`]), `keystore` the system keyring,
//! and `app` both plus [`link::Link`], the whole engine a world's app runs on,
//! with [`install`] and [`updates`] for installing and updating itself,
//! [`moons`] for moons that run with their planet, [`doors`] for opening
//! the other worlds' apps on the same computer, and on Linux [`frames`],
//! so its window paints at the screen's rate, and [`mouse`], so the mouse's
//! back and forward buttons reach the page.
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
pub mod doors;
#[cfg(all(feature = "app", target_os = "linux"))]
pub mod frames;
#[cfg(feature = "app")]
pub mod install;
#[cfg(feature = "keystore")]
pub mod keystore;
#[cfg(feature = "app")]
pub mod link;
#[cfg(feature = "app")]
pub mod moons;
#[cfg(all(feature = "app", target_os = "linux"))]
pub mod mouse;
#[cfg(feature = "app")]
pub mod updates;

pub use db::Actor;
pub use doc::Doc;
pub use error::ErrorBody;
pub use event::Envelope;
pub use widget::WidgetView;
pub use world::WorldManifest;

/// Version of the protocol described in `docs/protocol.md`. It goes up only
/// when a change breaks apps or Sols that speak an older one.
pub const PROTOCOL: u32 = 2;

/// Header an app sends with every request: the protocol it speaks.
pub const PROTOCOL_HEADER: &str = "sol-protocol";
/// Header an app sends with every request: its own version, e.g. `0.2.0`.
pub const VERSION_HEADER: &str = "sol-app-version";

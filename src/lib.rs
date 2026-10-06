//! Orbit: the protocol between Sol and its worlds.
//!
//! Sol is the one server. It keeps every world's data, the person's settings
//! and the connections between worlds. Each world is an app of its own (a
//! desktop app, later a phone app) that pairs with Sol and works with its data
//! there, the way a music player works with a Navidrome server.
//!
//! This crate holds what both sides share: the shapes on the wire
//! ([`world`], [`doc`], [`event`], [`device`], [`widget`]), the SQLite actor
//! ([`db`]) and logging ([`telemetry`]). Features add the app side:
//! `client` the HTTP client ([`client::Sol`]), `keystore` the system keyring,
//! and `app` both plus [`link::Link`], the whole engine a world's app runs on.
//! `docs/protocol.md` describes the protocol.

pub mod db;
pub mod device;
pub mod doc;
pub mod error;
pub mod event;
pub mod telemetry;
pub mod widget;
pub mod world;

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "keystore")]
pub mod keystore;
#[cfg(feature = "app")]
pub mod link;

pub use db::Actor;
pub use doc::Doc;
pub use error::ErrorBody;
pub use event::Envelope;
pub use widget::WidgetView;
pub use world::WorldManifest;

/// Version of the protocol described in `docs/protocol.md`.
pub const PROTOCOL: u32 = 2;

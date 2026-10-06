//! Pairing a world's app with Sol, and what a paired device can ask.
//!
//! 1. The app calls `POST /api/v1/pair` with its world and a device name and
//!    shows the person the code it gets back.
//! 2. In Sol, under Devices, the person approves the request with that code.
//! 3. The app polls `POST /api/v1/pair/claim` and receives a token for that
//!    world alone. It sends it as `Authorization: Bearer …` from then on.
//!
//! Nobody types a password into a world's app, and every device can be
//! revoked on its own.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::event::Envelope;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PairRequest {
    /// The world the app is, e.g. `terra`.
    pub world: String,
    /// How the device appears in Sol, e.g. `Desktop` or `Lukas's phone`.
    pub device: String,
    /// `linux`, `windows`, `macos`, `android` or `ios`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PairStarted {
    pub id: String,
    /// Show this; the person approves the request that carries it.
    pub code: String,
    /// Proves the claim comes from the app that started the pairing.
    pub secret: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PairClaim {
    pub id: String,
    pub secret: String,
}

/// Answer to a claim once the person has approved it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Paired {
    /// Keep it safe, e.g. in the system keyring; it is shown only once.
    pub token: String,
    pub device_id: String,
    pub world: String,
    pub user: Person,
}

/// Answer to a claim that is still waiting for the person.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Pending {
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Person {
    pub id: String,
    pub username: String,
    /// IANA time zone; "today" in a world means today here.
    pub tz: String,
}

/// `GET /api/v1/me`: who this device is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DeviceMe {
    pub device_id: String,
    pub world: String,
    pub user: Person,
}

/// Something another world asked this one to do, through a connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Delivery {
    /// Inbox cursor.
    pub n: i64,
    /// One of the world's `actions`, or `widget.toggle` from Sol's dashboard.
    pub action: String,
    pub params: serde_json::Value,
    /// The event that set it off, when there was one.
    pub event: Option<Envelope>,
    pub connection: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// `GET /api/v1/inbox?after=N`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct InboxPage {
    pub deliveries: Vec<Delivery>,
    pub next: i64,
}

/// `PUT /api/v1/widgets/{id}`: the latest look of one of the world's widgets
/// on Sol's dashboard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct WidgetPush {
    pub title: String,
    pub view: crate::widget::WidgetView,
}

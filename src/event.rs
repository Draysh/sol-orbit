//! Events: what happened in a world, in one sentence for people and a little
//! data for other worlds.
//!
//! A world app posts an [`Emit`] to Sol; Sol stores it as an [`Envelope`],
//! shows it in its activity feed, streams it to open screens and runs the
//! connections that listen for its type.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// An event as Sol stored it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Envelope {
    /// UUIDv7; the same id posted twice is stored once.
    pub id: Uuid,
    /// `<world>.<thing>.<past-tense verb>`, for example `terra.habit.checked`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Version of `data`'s shape for this type.
    pub v: u32,
    /// The world it came from; Sol sets it from the device's pairing.
    pub source: String,
    /// Position in Sol's event log.
    pub seq: i64,
    pub time: DateTime<Utc>,
    /// The person it belongs to; `None` for events about Sol itself.
    pub user: Option<String>,
    /// What it is about, as `<thing>/<id>`.
    pub subject: Option<String>,
    /// One sentence for people: `Did “Walk outside”, 4 days in a row`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub data: serde_json::Value,
}

/// What a world app posts to `POST /api/v1/events`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Emit {
    /// Optional UUIDv7, so a retried post is stored once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default = "one")]
    pub v: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub data: serde_json::Value,
}

fn one() -> u32 {
    1
}

impl Emit {
    pub fn new(kind: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            id: Some(Uuid::now_v7()),
            kind: kind.into(),
            v: 1,
            subject: None,
            summary: Some(summary.into()),
            data: serde_json::Value::Null,
        }
    }

    pub fn data(mut self, data: serde_json::Value) -> Self {
        self.data = data;
        self
    }

    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }
}

/// True when `kind` is a well-formed type in `world`'s namespace:
/// `<world>.<thing>.<verb>`, lowercase letters, digits, `-` and `_`.
pub fn valid_type(world: &str, kind: &str) -> bool {
    let mut parts = kind.split('.');
    parts.next() == Some(world)
        && kind.split('.').count() >= 3
        && parts.all(|p| {
            !p.is_empty()
                && p.len() <= 40
                && p.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        })
}

/// `terra.habit.*` matches every `terra.habit.` type; `*` matches all.
pub fn matches(pattern: &str, kind: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => kind.starts_with(prefix),
        None => pattern == kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_stay_in_their_world() {
        assert!(valid_type("terra", "terra.habit.checked"));
        assert!(valid_type("terra", "terra.check-in.logged"));
        assert!(!valid_type("terra", "neptune.track.played"));
        assert!(!valid_type("terra", "terra.checked"));
        assert!(!valid_type("terra", "terra.Habit.checked"));
        assert!(!valid_type("terra", "terra..checked"));
    }

    #[test]
    fn patterns() {
        assert!(matches("terra.habit.*", "terra.habit.checked"));
        assert!(matches("*", "neptune.track.played"));
        assert!(matches("terra.habit.checked", "terra.habit.checked"));
        assert!(!matches("terra.habit.checked", "terra.habit.created"));
    }

    #[test]
    fn emit_defaults() {
        let e: Emit = serde_json::from_str(r#"{"type":"terra.habit.checked"}"#).unwrap();
        assert_eq!(e.v, 1);
        assert!(e.data.is_null() && e.id.is_none());
    }
}

//! Documents: how a world keeps its data in Sol.
//!
//! A world stores JSON documents in named collections (`habits`, `checks`,
//! `ratings`). Every write gets the next version number in that world's
//! database, so a device stays in sync by asking for the changes after the
//! last version it saw. Deletes leave a tombstone that travels the same way.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The largest document Sol accepts, serialised.
pub const MAX_DOC_BYTES: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Doc {
    pub collection: String,
    pub id: String,
    /// Increases with every write in this world; the sync cursor.
    pub version: i64,
    pub updated_at: DateTime<Utc>,
    /// A tombstone: the document was deleted at `version`.
    #[serde(default)]
    pub deleted: bool,
    /// `null` for tombstones.
    pub data: serde_json::Value,
}

/// `PUT /api/v1/docs/{collection}/{id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Put {
    pub data: serde_json::Value,
    /// Write only if the document is still at this version (`0`: only if it
    /// doesn't exist yet). Leave out to always write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_version: Option<i64>,
}

/// `GET /api/v1/changes?after=N`: every write after version `N`, oldest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ChangePage {
    pub changes: Vec<Doc>,
    /// Send as `after` next time.
    pub next: i64,
    /// More changes are waiting; ask again straight away.
    pub more: bool,
    /// The id of Sol's database, made when Sol first started and again when
    /// it is restored from a backup. Cursors (`next` here and in the inbox)
    /// belong to one id: when it changes, start again from `0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
}

/// The `409` body when `if_version` didn't match (`error` is `conflict`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Conflict {
    pub error: String,
    pub message: String,
    /// The document as it is now; `None` if it doesn't exist.
    pub current: Option<Doc>,
}

/// The schema version of a stored shape written before it carried one. A
/// world's stored type keeps its version in a `v` field, so a later shape can
/// tell old rows from new: `#[serde(default = "orbit::doc::first_shape")] v: u32`.
pub fn first_shape() -> u32 {
    1
}

/// Collection names: lowercase letters, digits, `-` and `_`, at most 64.
pub fn valid_collection(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// A value as JSON for a document or an event's data, through JSON text: an
/// `f32` is kept as written (7.3), where `serde_json::to_value` widens it
/// (7.300000190734863) and Sol would store and show the noise.
pub fn value_of<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<serde_json::Value> {
    serde_json::from_str(&serde_json::to_string(value)?)
}

/// Document ids: letters, digits, `-` and `_`, at most 128 (a UUID fits).
pub fn valid_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_collection("habits") && valid_collection("check_ins"));
        assert!(!valid_collection("Habits") && !valid_collection("") && !valid_collection("1x"));
        assert!(valid_id("0192f1c4-7a1b-7c3d-9e0f-123456789abc"));
        assert!(!valid_id("a/b") && !valid_id(""));
    }

    #[test]
    fn values_keep_floats_as_written() {
        #[derive(Serialize)]
        struct Rated {
            rating: f32,
        }
        assert_eq!(
            value_of(&Rated { rating: 7.3 }).unwrap(),
            serde_json::json!({ "rating": 7.3 })
        );
    }
}

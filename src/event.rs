//! Events leave an app through a transactional outbox.
//!
//! An app writes its event in the same SQLite transaction as the change it
//! describes, so both commit or neither does. Sol long-polls
//! `/_sol/outbox?after=<seq>` and stores its own cursor, which makes delivery
//! at-least-once; consumers deduplicate on the event `id`.

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use utoipa::ToSchema;
use uuid::Uuid;

/// The envelope every event travels in, from app outbox to Sol to the browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Envelope {
    /// UUIDv7 chosen by the producing app; consumers deduplicate on it.
    pub id: Uuid,
    /// `<app>.<entity>.<past-tense verb>`, for example `terra.habit.checked`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Version of `data`'s shape for this type.
    pub v: u32,
    /// The app that produced it.
    pub source: String,
    /// Position in the producing app's outbox.
    pub seq: i64,
    pub time: DateTime<Utc>,
    /// The user the event belongs to; `None` for system-wide events.
    pub user: Option<String>,
    /// What it is about, as `<entity>/<id>`.
    pub subject: Option<String>,
    /// One sentence for people, written by the app: `Did “Walk outside”, 4 days in a row`.
    /// Sol's activity feed and notifications show it as is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub data: serde_json::Value,
}

/// One page of an app's outbox.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct OutboxPage {
    pub events: Vec<Envelope>,
    /// The cursor to send as `after` next time.
    pub next: i64,
}

/// What an app passes to [`append`].
pub struct NewEvent<'a, T: Serialize> {
    pub kind: &'a str,
    pub v: u32,
    pub user: Option<&'a str>,
    pub subject: Option<String>,
    pub summary: Option<String>,
    pub data: &'a T,
}

pub(crate) fn ensure_outbox(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _orbit_outbox (
            seq     INTEGER PRIMARY KEY AUTOINCREMENT,
            id      TEXT NOT NULL UNIQUE,
            type    TEXT NOT NULL,
            v       INTEGER NOT NULL,
            time    TEXT NOT NULL,
            user_id TEXT,
            subject TEXT,
            summary TEXT,
            data    TEXT NOT NULL
        );",
    )?;
    // Outboxes created before `summary` existed.
    let has_summary = conn
        .prepare("SELECT 1 FROM pragma_table_info('_orbit_outbox') WHERE name = 'summary'")?
        .exists([])?;
    if !has_summary {
        conn.execute_batch("ALTER TABLE _orbit_outbox ADD COLUMN summary TEXT;")?;
    }
    Ok(())
}

/// Records an event inside the caller's transaction. Call [`Outbox::notify`]
/// after the transaction commits to wake Sol's long-poll.
pub fn append<T: Serialize>(conn: &Connection, event: NewEvent<'_, T>) -> rusqlite::Result<i64> {
    let data = serde_json::to_string(event.data)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    conn.execute(
        "INSERT INTO _orbit_outbox (id, type, v, time, user_id, subject, summary, data)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            Uuid::now_v7().to_string(),
            event.kind,
            event.v,
            Utc::now().to_rfc3339(),
            event.user,
            event.subject,
            event.summary,
            data,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Reads up to `limit` events after `after`, oldest first.
pub fn read_after(
    conn: &Connection,
    source: &str,
    after: i64,
    limit: u32,
) -> rusqlite::Result<OutboxPage> {
    let mut stmt = conn.prepare_cached(
        "SELECT seq, id, type, v, time, user_id, subject, summary, data
         FROM _orbit_outbox WHERE seq > ?1 ORDER BY seq LIMIT ?2",
    )?;
    let events = stmt
        .query_map(params![after, limit], |row| {
            let id: String = row.get(1)?;
            let time: String = row.get(4)?;
            let data: String = row.get(8)?;
            Ok(Envelope {
                seq: row.get(0)?,
                id: id.parse().unwrap_or_default(),
                kind: row.get(2)?,
                v: row.get(3)?,
                source: source.to_owned(),
                time: DateTime::parse_from_rfc3339(&time)
                    .map(|t| t.with_timezone(&Utc))
                    .unwrap_or_default(),
                user: row.get(5)?,
                subject: row.get(6)?,
                summary: row.get(7)?,
                data: serde_json::from_str(&data).unwrap_or_default(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next = events.last().map_or(after, |e| e.seq);
    Ok(OutboxPage { events, next })
}

/// Wakes outbox long-polls when an app commits new events.
#[derive(Clone)]
pub struct Outbox {
    pub(crate) source: &'static str,
    pub(crate) notify: Arc<Notify>,
}

impl Outbox {
    pub fn new(source: &'static str) -> Self {
        Self {
            source,
            notify: Arc::new(Notify::new()),
        }
    }

    /// Call after committing a transaction that appended events.
    pub fn notify(&self) {
        self.notify.notify_waiters();
    }
}

/// Longest wait a caller may ask for; keeps requests under proxy timeouts.
pub const MAX_WAIT: Duration = Duration::from_secs(25);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_then_read_after_cursor() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_outbox(&conn).unwrap();
        for n in 0..3 {
            append(
                &conn,
                NewEvent {
                    kind: "test.thing.happened",
                    v: 1,
                    user: Some("u1"),
                    subject: None,
                    summary: Some(format!("Thing {n} happened")),
                    data: &n,
                },
            )
            .unwrap();
        }
        let first = read_after(&conn, "test", 0, 2).unwrap();
        assert_eq!(first.events.len(), 2);
        assert_eq!(first.events[0].source, "test");
        assert_eq!(first.events[1].data, serde_json::json!(1));
        assert_eq!(first.events[1].summary.as_deref(), Some("Thing 1 happened"));

        let rest = read_after(&conn, "test", first.next, 10).unwrap();
        assert_eq!(rest.events.len(), 1);
        assert_eq!(rest.next, 3);
        assert!(
            read_after(&conn, "test", rest.next, 10)
                .unwrap()
                .events
                .is_empty()
        );
    }
}

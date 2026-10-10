//! The local copy: documents, writes waiting for Sol, and a few facts
//! about the pairing, in SQLite (the tables are in `cache.sql`).

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};
use rusqlite_migration::M;

use crate::{Doc, device::Person};

pub(super) const MIGRATIONS: &[M<'static>] = &[M::up(include_str!("cache.sql"))];

pub(super) const COLUMNS: &str = "collection, id, version, updated_at, deleted, data";

pub(super) fn doc(row: &Row) -> rusqlite::Result<Doc> {
    let data: String = row.get(5)?;
    Ok(Doc {
        collection: row.get(0)?,
        id: row.get(1)?,
        version: row.get(2)?,
        updated_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(3)?)
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_default(),
        deleted: row.get(4)?,
        data: serde_json::from_str(&data).unwrap_or_default(),
    })
}

pub(super) fn local(c: &Connection, collection: &str, id: &str) -> rusqlite::Result<Option<Doc>> {
    c.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM docs WHERE collection = ?1 AND id = ?2"
    ))?
    .query_row(params![collection, id], doc)
    .optional()
}

pub(super) fn store(c: &Connection, d: &Doc) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO docs (collection, id, version, updated_at, deleted, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (collection, id) DO UPDATE SET version = excluded.version,
           updated_at = excluded.updated_at, deleted = excluded.deleted, data = excluded.data",
        params![
            d.collection,
            d.id,
            d.version,
            d.updated_at.to_rfc3339(),
            d.deleted,
            d.data.to_string()
        ],
    )?;
    Ok(())
}

pub(super) fn waiting(c: &Connection, collection: &str, id: &str) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT EXISTS (SELECT 1 FROM pending WHERE collection = ?1 AND id = ?2)",
        params![collection, id],
        |r| r.get(0),
    )
}

pub(super) fn meta(c: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()
}

pub(super) fn set_meta(c: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub(super) fn cursor(c: &mut Connection, name: &str) -> rusqlite::Result<i64> {
    Ok(meta(c, &format!("cursor:{name}"))?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0))
}

pub(super) struct Meta {
    pub(super) server: Option<String>,
    pub(super) device: Option<String>,
    pub(super) user: Option<Person>,
}

pub(super) fn read_meta(c: &mut Connection) -> rusqlite::Result<Meta> {
    Ok(Meta {
        server: meta(c, "server")?,
        device: meta(c, "device")?,
        user: meta(c, "user")?.and_then(|u| serde_json::from_str(&u).ok()),
    })
}

pub(super) fn now() -> String {
    Utc::now().to_rfc3339()
}

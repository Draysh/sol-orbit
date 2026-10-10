//! Writes kept aside instead of lost: a document another device changed
//! first (a conflict), a write Sol turned down, and writes that were still
//! waiting when the device was unpaired. The screens show them through
//! [`Kept`] and let the person choose; nothing here is dropped without them.

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{
    Link, Update,
    cache::{local, now, store},
};
use crate::Doc;

/// Why a write was kept aside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Why {
    /// The document changed on another device first; Sol's copy stays in
    /// place until the person picks this one ([`Link::keep_mine`]).
    Conflict,
    /// Sol turned the write down (too large, a name it doesn't take).
    Rejected,
    /// Waiting when this device was unpaired; sent once it pairs again with
    /// the same person, kept here otherwise.
    Earlier,
}

impl Why {
    fn as_str(self) -> &'static str {
        match self {
            Why::Conflict => "conflict",
            Why::Rejected => "rejected",
            Why::Earlier => "earlier",
        }
    }
}

/// How many writes are kept aside, by why; all zero almost always.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Kept {
    pub conflicts: u32,
    pub rejected: u32,
    pub earlier: u32,
}

impl Kept {
    pub fn is_empty(&self) -> bool {
        self.conflicts + self.rejected + self.earlier == 0
    }
}

/// One write kept aside.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KeptWrite {
    /// What [`Link::keep_mine`] and [`Link::drop_kept`] take.
    pub seq: i64,
    pub why: Why,
    /// `put`, `delete`, `event`, `widget` or `unwidget`.
    pub kind: String,
    pub collection: Option<String>,
    pub id: Option<String>,
    /// This device's version: the document (`null` for a delete), the event
    /// or the widget.
    pub data: serde_json::Value,
    /// For a conflict, Sol's copy as it is (`None`: Sol has none).
    pub theirs: Option<Doc>,
    /// For a rejected write, what Sol said.
    pub message: Option<String>,
    pub at: DateTime<Utc>,
}

impl Link {
    /// How many writes are kept aside.
    pub async fn kept(&self) -> anyhow::Result<Kept> {
        Ok(self.0.db.call(|c| count(c)).await??)
    }

    /// Every write kept aside, oldest first.
    pub async fn kept_writes(&self) -> anyhow::Result<Vec<KeptWrite>> {
        Ok(self
            .0
            .db
            .call(|c| -> rusqlite::Result<Vec<KeptWrite>> {
                c.prepare(
                    "SELECT seq, why, kind, collection, id, body, theirs, message, at FROM held ORDER BY seq",
                )?
                .query_map([], |r| {
                    let why = match r.get::<_, String>(1)?.as_str() {
                        "conflict" => Why::Conflict,
                        "rejected" => Why::Rejected,
                        _ => Why::Earlier,
                    };
                    let body: String = r.get(5)?;
                    let theirs: Option<String> = r.get(6)?;
                    Ok(KeptWrite {
                        seq: r.get(0)?,
                        why,
                        kind: r.get(2)?,
                        collection: r.get(3)?,
                        id: r.get(4)?,
                        data: serde_json::from_str(&body).unwrap_or_default(),
                        theirs: theirs.and_then(|t| serde_json::from_str(&t).ok()),
                        message: r.get(7)?,
                        at: DateTime::parse_from_rfc3339(&r.get::<_, String>(8)?)
                            .map(|t| t.with_timezone(&Utc))
                            .unwrap_or_default(),
                    })
                })?
                .collect()
            })
            .await??)
    }

    /// Settles a conflict for this device's version: it goes to Sol over
    /// the other one, and shows here at once.
    pub async fn keep_mine(&self, seq: i64) -> anyhow::Result<()> {
        let collection = self
            .0
            .db
            .call(move |c| -> anyhow::Result<String> {
                let tx = c.transaction()?;
                let row = tx
                    .query_row(
                        "SELECT kind, collection, id, body, base FROM held WHERE seq = ?1 AND why = 'conflict'",
                        [seq],
                        |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, String>(2)?,
                                r.get::<_, String>(3)?,
                                r.get::<_, Option<i64>>(4)?,
                            ))
                        },
                    )
                    .optional()?;
                let Some((kind, collection, id, body, base)) = row else {
                    anyhow::bail!("that conflict was settled already");
                };
                let version = local(&tx, &collection, &id)?.map_or(0, |d| d.version);
                if kind == "delete" {
                    tx.execute(
                        "UPDATE docs SET deleted = 1, data = 'null', updated_at = ?3
                         WHERE collection = ?1 AND id = ?2",
                        params![collection, id, now()],
                    )?;
                } else {
                    tx.execute(
                        "INSERT INTO docs (collection, id, version, updated_at, deleted, data)
                         VALUES (?1, ?2, ?3, ?4, 0, ?5)
                         ON CONFLICT (collection, id) DO UPDATE SET
                           updated_at = excluded.updated_at, deleted = 0, data = excluded.data",
                        params![collection, id, version, now(), body],
                    )?;
                }
                // On top of the copy the person saw, so a newer change from
                // yet another device still comes back as a conflict.
                tx.execute(
                    "INSERT INTO pending (kind, collection, id, body, base) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![kind, collection, id, body, base],
                )?;
                tx.execute("DELETE FROM held WHERE seq = ?1", [seq])?;
                tx.commit()?;
                Ok(collection)
            })
            .await??;
        self.wrote(&[collection]).await;
        self.announce_kept().await;
        Ok(())
    }

    /// Lets a kept write go: for a conflict, Sol's copy stays as it is; a
    /// rejected or earlier write is forgotten.
    pub async fn drop_kept(&self, seq: i64) -> anyhow::Result<()> {
        self.0
            .db
            .call(move |c| c.execute("DELETE FROM held WHERE seq = ?1", [seq]))
            .await??;
        self.announce_kept().await;
        Ok(())
    }

    /// Every kept write as one JSON document, for the person to save.
    pub async fn export_kept(&self) -> anyhow::Result<serde_json::Value> {
        Ok(serde_json::json!({
            "world": self.world(),
            "exported_at": Utc::now(),
            "writes": self.kept_writes().await?,
        }))
    }

    /// Tells the screens how many writes are kept aside now.
    pub(super) async fn announce_kept(&self) {
        if let Ok(kept) = self.kept().await {
            self.send(Update::Kept(kept));
        }
    }
}

fn count(c: &Connection) -> rusqlite::Result<Kept> {
    c.query_row(
        "SELECT count(*) FILTER (WHERE why = 'conflict'), count(*) FILTER (WHERE why = 'rejected'),
                count(*) FILTER (WHERE why = 'earlier') FROM held",
        [],
        |r| {
            Ok(Kept {
                conflicts: r.get(0)?,
                rejected: r.get(1)?,
                earlier: r.get(2)?,
            })
        },
    )
}

/// Moves every waiting write aside, for when the device is unpaired: they
/// go again if it pairs with `person` once more.
pub(super) fn set_aside(
    c: &Connection,
    person: Option<&str>,
    server: Option<&str>,
) -> rusqlite::Result<usize> {
    let moved = c.execute(
        "INSERT INTO held (why, kind, collection, id, body, base, person, server, at)
         SELECT 'earlier', kind, collection, id, body, base, ?1, ?2, ?3 FROM pending ORDER BY seq",
        params![person, server, now()],
    )?;
    c.execute("DELETE FROM pending", [])?;
    Ok(moved)
}

/// After pairing with `person`: their writes from an earlier pairing wait
/// to be sent again, in order, and show here at once.
pub(super) fn replay_earlier(c: &Connection, person: &str) -> rusqlite::Result<usize> {
    type Row = (
        i64,
        String,
        Option<String>,
        Option<String>,
        String,
        Option<i64>,
    );
    let rows: Vec<Row> = c
        .prepare(
            "SELECT seq, kind, collection, id, body, base FROM held
             WHERE why = 'earlier' AND person = ?1 ORDER BY seq",
        )?
        .query_map([person], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (seq, kind, collection, id, body, base) in &rows {
        c.execute(
            "INSERT INTO pending (kind, collection, id, body, base) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![kind, collection, id, body, base],
        )?;
        if let (Some(collection), Some(id)) = (collection, id) {
            match kind.as_str() {
                "put" => {
                    c.execute(
                        "INSERT INTO docs (collection, id, version, updated_at, deleted, data)
                         VALUES (?1, ?2, ?3, ?4, 0, ?5)
                         ON CONFLICT (collection, id) DO UPDATE SET
                           updated_at = excluded.updated_at, deleted = 0, data = excluded.data",
                        params![collection, id, base.unwrap_or(0), now(), body],
                    )?;
                }
                "delete" => {
                    c.execute(
                        "UPDATE docs SET deleted = 1, data = 'null', updated_at = ?3
                         WHERE collection = ?1 AND id = ?2",
                        params![collection, id, now()],
                    )?;
                }
                _ => {}
            }
        }
        c.execute("DELETE FROM held WHERE seq = ?1", [seq])?;
    }
    Ok(rows.len())
}

/// Sol answered a write with `409`: its copy (`current`) goes in here, and
/// this device's latest version of the document is kept as a conflict,
/// together with any later writes to it still waiting (only the latest
/// counts). Returns false when both copies say the same thing (a write that
/// went through though its answer was lost): then nothing is kept.
pub(super) fn hold_conflict(
    c: &Connection,
    collection: &str,
    id: &str,
    current: Option<&Doc>,
) -> rusqlite::Result<bool> {
    let waiting: Vec<(i64, String, String)> = c
        .prepare(
            "SELECT seq, kind, body FROM pending
             WHERE collection = ?1 AND id = ?2 AND kind IN ('put', 'delete') ORDER BY seq",
        )?
        .query_map(params![collection, id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    c.execute(
        "DELETE FROM pending WHERE collection = ?1 AND id = ?2 AND kind IN ('put', 'delete')",
        params![collection, id],
    )?;
    match current {
        Some(doc) => store(c, doc)?,
        None => {
            c.execute(
                "DELETE FROM docs WHERE collection = ?1 AND id = ?2",
                params![collection, id],
            )?;
        }
    }
    let Some((_, kind, body)) = waiting.last() else {
        return Ok(false);
    };
    let mine: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let same = match current {
        Some(doc) if doc.deleted => kind == "delete",
        Some(doc) => kind == "put" && doc.data == mine,
        None => kind == "delete",
    };
    if same {
        return Ok(false);
    }
    c.execute(
        "INSERT INTO held (why, kind, collection, id, body, base, theirs, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            Why::Conflict.as_str(),
            kind,
            collection,
            id,
            body,
            current.map_or(0, |d| d.version),
            current.and_then(|d| serde_json::to_string(d).ok()),
            now()
        ],
    )?;
    Ok(true)
}

/// Sol turned the waiting write `seq` down: it is kept with Sol's reason.
pub(super) fn hold_rejected(c: &Connection, seq: i64, message: &str) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO held (why, kind, collection, id, body, base, message, at)
         SELECT ?2, kind, collection, id, body, base, ?3, ?4 FROM pending WHERE seq = ?1",
        params![seq, Why::Rejected.as_str(), message, now()],
    )?;
    c.execute("DELETE FROM pending WHERE seq = ?1", [seq])?;
    Ok(())
}

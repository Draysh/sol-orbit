//! The world's documents, read from and written to the local copy.

use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
};

use rusqlite::params;
use serde::{Serialize, de::DeserializeOwned};

use super::{
    Link,
    cache::{COLUMNS, doc, local, now},
};
use crate::{
    Doc,
    doc::{valid_collection, valid_id},
};

/// How many documents of another shape each typed listing left out last,
/// by world, collection and type, so a change is logged once.
static UNREADABLE: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);

/// A collection read as `T` by [`Link::list_counted`].
#[derive(Debug, Clone, PartialEq)]
pub struct Listed<T> {
    /// The documents that are `T`, by id, each with its id.
    pub rows: Vec<(String, T)>,
    /// How many live documents had another shape and were left out.
    pub unreadable: usize,
}

impl Link {
    /// Every live document in a collection, from the local copy.
    pub async fn list(&self, collection: &str) -> anyhow::Result<Vec<Doc>> {
        let collection = collection.to_owned();
        Ok(self
            .0
            .db
            .call(move |c| {
                c.prepare_cached(&format!(
                    "SELECT {COLUMNS} FROM docs WHERE collection = ?1 AND deleted = 0 ORDER BY id"
                ))?
                .query_map([collection], doc)?
                .collect::<rusqlite::Result<Vec<_>>>()
            })
            .await??)
    }

    /// Every live document in a collection whose id starts with `prefix`
    /// (a title's episodes, `<key>_`), by id. It reads only those rows, by
    /// the local copy's index, so it stays quick however large the
    /// collection grows.
    pub async fn list_prefix(&self, collection: &str, prefix: &str) -> anyhow::Result<Vec<Doc>> {
        let (collection, from, to) = (collection.to_owned(), prefix.to_owned(), past(prefix));
        Ok(self
            .0
            .db
            .call(move |c| {
                c.prepare_cached(&format!(
                    "SELECT {COLUMNS} FROM docs WHERE collection = ?1 AND id >= ?2 AND id < ?3
                     AND deleted = 0 ORDER BY id"
                ))?
                .query_map(params![collection, from, to], doc)?
                .collect::<rusqlite::Result<Vec<_>>>()
            })
            .await??)
    }

    /// The same as `T`, each with its id; documents of another shape are
    /// left out (the whole collection's listing logs how many).
    pub async fn list_prefix_as<T: DeserializeOwned>(
        &self,
        collection: &str,
        prefix: &str,
    ) -> anyhow::Result<Vec<(String, T)>> {
        Ok(self
            .list_prefix(collection, prefix)
            .await?
            .into_iter()
            .filter_map(|d| Some((d.id, serde_json::from_value(d.data).ok()?)))
            .collect())
    }

    /// How many live documents a collection has whose id starts with
    /// `prefix` (`""` for all of them), counted in the local copy without
    /// reading them.
    pub async fn count(&self, collection: &str, prefix: &str) -> anyhow::Result<usize> {
        let (collection, from, to) = (collection.to_owned(), prefix.to_owned(), past(prefix));
        Ok(self
            .0
            .db
            .call(move |c| {
                c.prepare_cached(
                    "SELECT count(*) FROM docs WHERE collection = ?1 AND id >= ?2 AND id < ?3
                     AND deleted = 0",
                )?
                .query_row(params![collection, from, to], |r| r.get::<_, i64>(0))
            })
            .await?? as usize)
    }

    pub async fn get(&self, collection: &str, id: &str) -> anyhow::Result<Option<Doc>> {
        let (collection, id) = (collection.to_owned(), id.to_owned());
        Ok(self
            .0
            .db
            .call(move |c| local(c, &collection, &id))
            .await??
            .filter(|d| !d.deleted))
    }

    /// Every live document in a collection as `T`, by id; documents of
    /// another shape are left out (how many is logged, see
    /// [`Link::list_counted`]).
    pub async fn list_as<T: DeserializeOwned>(&self, collection: &str) -> anyhow::Result<Vec<T>> {
        Ok(self
            .list_counted(collection)
            .await?
            .rows
            .into_iter()
            .map(|(_, t)| t)
            .collect())
    }

    /// The same, each with its document's id.
    pub async fn list_with_ids<T: DeserializeOwned>(
        &self,
        collection: &str,
    ) -> anyhow::Result<Vec<(String, T)>> {
        Ok(self.list_counted(collection).await?.rows)
    }

    /// Every live document in a collection as `T`, each with its id, and how
    /// many of another shape were left out, for a page that says so. A count
    /// is logged when it changes (not on every read, so an app reading the
    /// same rows every minute doesn't write a log line every minute).
    pub async fn list_counted<T: DeserializeOwned>(
        &self,
        collection: &str,
    ) -> anyhow::Result<Listed<T>> {
        let mut rows = Vec::new();
        let mut unreadable = 0;
        let mut first = None;
        for d in self.list(collection).await? {
            match serde_json::from_value(d.data) {
                Ok(t) => rows.push((d.id, t)),
                Err(e) => {
                    unreadable += 1;
                    first.get_or_insert((d.id, e));
                }
            }
        }
        let key = format!(
            "{}/{collection}/{}",
            self.world(),
            std::any::type_name::<T>()
        );
        let before = UNREADABLE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(HashMap::new)
            .insert(key, unreadable)
            .unwrap_or(0);
        if unreadable != before {
            match first {
                Some((id, e)) => tracing::warn!(
                    "{unreadable} document(s) in {collection} couldn't be read and were left out \
                     (the first, {id:?}: {e})"
                ),
                None => tracing::info!("every document in {collection} can be read again"),
            }
        }
        Ok(Listed { rows, unreadable })
    }

    /// One document as `T`; nothing when there is none or it has another shape.
    pub async fn get_as<T: DeserializeOwned>(
        &self,
        collection: &str,
        id: &str,
    ) -> anyhow::Result<Option<T>> {
        Ok(self
            .get(collection, id)
            .await?
            .and_then(|d| serde_json::from_value(d.data).ok()))
    }

    /// Writes `value` as a document, with its `f32`s kept as written (see
    /// [`crate::doc::value_of`]).
    pub async fn put_as<T: Serialize + ?Sized>(
        &self,
        collection: &str,
        id: &str,
        value: &T,
    ) -> anyhow::Result<Doc> {
        self.put(collection, id, crate::doc::value_of(value)?).await
    }

    /// Writes a document here at once and sends it to Sol in the background.
    /// If another device changed it in Sol meanwhile, Sol's copy stays and
    /// this one is kept aside as a conflict (see [`Link::kept`]) for the
    /// person to choose between them.
    pub async fn put(
        &self,
        collection: &str,
        id: &str,
        data: serde_json::Value,
    ) -> anyhow::Result<Doc> {
        self.write(collection, id, data, true).await
    }

    /// Like [`Link::put`], but this write wins over whatever another device
    /// wrote meanwhile: for state that only matters as it is now (where
    /// playback stands, the last page open), never for what the person made.
    pub async fn overwrite(
        &self,
        collection: &str,
        id: &str,
        data: serde_json::Value,
    ) -> anyhow::Result<Doc> {
        self.write(collection, id, data, false).await
    }

    async fn write(
        &self,
        collection: &str,
        id: &str,
        data: serde_json::Value,
        conditional: bool,
    ) -> anyhow::Result<Doc> {
        check_names(collection, id)?;
        anyhow::ensure!(!data.is_null(), "use delete to remove a document");
        let (c2, i2) = (collection.to_owned(), id.to_owned());
        let written = self
            .0
            .db
            .call(move |c| -> rusqlite::Result<Doc> {
                let tx = c.transaction()?;
                let known = local(&tx, &c2, &i2)?;
                let version = known.as_ref().map_or(0, |d| d.version);
                // The copy this write was made on: Sol's version of it, 0 when
                // Sol has none yet, nothing while that's unknown (a restore).
                let base = (conditional && version >= 0).then_some(version);
                tx.execute(
                    "INSERT INTO docs (collection, id, version, updated_at, deleted, data)
                     VALUES (?1, ?2, ?3, ?4, 0, ?5)
                     ON CONFLICT (collection, id) DO UPDATE SET
                       updated_at = excluded.updated_at, deleted = 0, data = excluded.data",
                    params![c2, i2, version, now(), data.to_string()],
                )?;
                tx.execute(
                    "INSERT INTO pending (kind, collection, id, body, base) VALUES ('put', ?1, ?2, ?3, ?4)",
                    params![c2, i2, data.to_string(), base],
                )?;
                let written = local(&tx, &c2, &i2)?.expect("just written");
                tx.commit()?;
                Ok(written)
            })
            .await??;
        self.wrote(&[collection.to_owned()]).await;
        Ok(written)
    }

    /// Deletes a document here at once and in Sol in the background.
    pub async fn delete(&self, collection: &str, id: &str) -> anyhow::Result<()> {
        check_names(collection, id)?;
        let (c2, i2) = (collection.to_owned(), id.to_owned());
        self.0
            .db
            .call(move |c| -> rusqlite::Result<()> {
                let tx = c.transaction()?;
                tx.execute(
                    "UPDATE docs SET deleted = 1, data = 'null', updated_at = ?3
                     WHERE collection = ?1 AND id = ?2",
                    params![c2, i2, now()],
                )?;
                tx.execute(
                    "INSERT INTO pending (kind, collection, id, body) VALUES ('delete', ?1, ?2, 'null')",
                    params![c2, i2],
                )?;
                tx.commit()
            })
            .await??;
        self.wrote(&[collection.to_owned()]).await;
        Ok(())
    }
}

/// The first id past every id that starts with `prefix`: ids are ASCII,
/// and the last character of Unicode sorts after all of it.
fn past(prefix: &str) -> String {
    format!("{prefix}{}", char::MAX)
}

fn check_names(collection: &str, id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        valid_collection(collection),
        "a collection name is lowercase letters, digits, - and _"
    );
    anyhow::ensure!(valid_id(id), "a document id is letters, digits, - and _");
    Ok(())
}

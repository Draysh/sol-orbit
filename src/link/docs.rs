//! The world's documents, read from and written to the local copy.

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
    /// another shape are left out.
    pub async fn list_as<T: DeserializeOwned>(&self, collection: &str) -> anyhow::Result<Vec<T>> {
        Ok(self
            .list(collection)
            .await?
            .into_iter()
            .filter_map(|d| serde_json::from_value(d.data).ok())
            .collect())
    }

    /// The same, each with its document's id.
    pub async fn list_with_ids<T: DeserializeOwned>(
        &self,
        collection: &str,
    ) -> anyhow::Result<Vec<(String, T)>> {
        Ok(self
            .list(collection)
            .await?
            .into_iter()
            .filter_map(|d| Some((d.id, serde_json::from_value(d.data).ok()?)))
            .collect())
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
    pub async fn put(
        &self,
        collection: &str,
        id: &str,
        data: serde_json::Value,
    ) -> anyhow::Result<Doc> {
        check_names(collection, id)?;
        anyhow::ensure!(!data.is_null(), "use delete to remove a document");
        let (c2, i2) = (collection.to_owned(), id.to_owned());
        let written = self
            .0
            .db
            .call(move |c| -> rusqlite::Result<Doc> {
                let tx = c.transaction()?;
                let version = local(&tx, &c2, &i2)?.map_or(0, |d| d.version);
                tx.execute(
                    "INSERT INTO docs (collection, id, version, updated_at, deleted, data)
                     VALUES (?1, ?2, ?3, ?4, 0, ?5)
                     ON CONFLICT (collection, id) DO UPDATE SET
                       updated_at = excluded.updated_at, deleted = 0, data = excluded.data",
                    params![c2, i2, version, now(), data.to_string()],
                )?;
                tx.execute(
                    "INSERT INTO pending (kind, collection, id, body) VALUES ('put', ?1, ?2, ?3)",
                    params![c2, i2, data.to_string()],
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

fn check_names(collection: &str, id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        valid_collection(collection),
        "a collection name is lowercase letters, digits, - and _"
    );
    anyhow::ensure!(valid_id(id), "a document id is letters, digits, - and _");
    Ok(())
}

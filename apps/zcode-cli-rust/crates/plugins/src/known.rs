//! Writes of `known_marketplaces.json`, the authority of marketplace
//! snapshots (Node `upsertKnownMarketplace`, `persistMarketplaceRefreshFailure`,
//! `removeMarketplace`, `writeKnownMarketplaces`). Spec
//! rust-m10-4-plugin-sources §7.
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::path::Path;

pub async fn write(storage: &Path, records: &[Value]) -> Result<()> {
    let path = storage.join(crate::store::KNOWN_MARKETPLACES);
    crate::store::write_json(&path, &json!({"version":1,"marketplaces":records})).await?;
    Ok(())
}

/// An upserted record that can be rolled back until finalized.
pub struct Upsert {
    record: Value,
    previous: Option<Value>,
    settled: bool,
}

impl Upsert {
    pub fn finalize(&mut self) {
        self.settled = true;
    }

    /// Restores the previous record unless another writer changed it since.
    pub async fn rollback(&mut self, storage: &Path) -> Result<()> {
        if self.settled {
            return Ok(());
        }
        let mut current = crate::market::known(storage).await?;
        let id = &self.record["id"];
        let index = current.iter().position(|r| r["id"] == *id);
        let unchanged = index.is_some_and(|i| {
            current[i].get("lastUpdated") == self.record.get("lastUpdated")
                && current[i].get("cacheTransactionId") == self.record.get("cacheTransactionId")
        });
        let Some(index) = index.filter(|_| unchanged) else {
            bail!(
                "Cannot roll back marketplace authority after concurrent update: {}",
                id.as_str().unwrap_or_default()
            );
        };
        match &self.previous {
            Some(previous) => current[index] = previous.clone(),
            None => {
                current.remove(index);
            }
        }
        write(storage, &current).await?;
        self.settled = true;
        Ok(())
    }
}

/// Node `upsertKnownMarketplace`: a refreshed record keeps the previous
/// fields and `addedAt`, dropping its transaction id and refresh failure.
pub async fn upsert(storage: &Path, record: Value) -> Result<Upsert> {
    let mut known = crate::market::known(storage).await?;
    let index = known.iter().position(|r| r["id"] == record["id"]);
    let previous = index.map(|i| known[i].clone());
    match index {
        Some(index) => {
            let mut merged = known[index].clone();
            if let Some(fields) = merged.as_object_mut() {
                fields.remove("cacheTransactionId");
                fields.remove("lastRefreshFailure");
                let added = fields.get("addedAt").filter(|v| !v.is_null()).cloned();
                for (key, value) in record.as_object().into_iter().flatten() {
                    fields.insert(key.clone(), value.clone());
                }
                if let Some(added) = added {
                    fields.insert("addedAt".into(), added);
                }
            }
            known[index] = merged;
        }
        None => known.push(record.clone()),
    }
    write(storage, &known).await?;
    Ok(Upsert {
        record,
        previous,
        settled: false,
    })
}

/// Node `persistMarketplaceRefreshFailure`.
pub async fn persist_failure(storage: &Path, id: &str, failure: Value) -> Result<()> {
    let mut known = crate::market::known(storage).await?;
    let Some(record) = known.iter_mut().find(|r| r["id"] == id) else {
        return Ok(());
    };
    record["lastRefreshFailure"] = failure;
    write(storage, &known).await
}

/// Node `removeMarketplace`: the snapshot directory is kept.
pub async fn remove(storage: &Path, id: &str) -> Result<()> {
    let known: Vec<Value> = crate::market::known(storage)
        .await?
        .into_iter()
        .filter(|r| r["id"] != id)
        .collect();
    write(storage, &known).await
}

/// Node `toMarketplaceSummaryData` (without featured and count overrides).
pub fn summary(record: &Value) -> Value {
    let mut summary = json!({"id":record["id"],"name":record["name"],"source":record["source"],
        "pluginCount":record["pluginCount"],
        "isOfficial":record["id"] == crate::official::MARKETPLACE});
    for key in ["description", "lastUpdated"] {
        if record[key].as_str().is_some_and(|s| !s.is_empty()) {
            summary[key] = record[key].clone();
        }
    }
    if let Some(failure) = record.get("lastRefreshFailure").filter(|f| f.is_object()) {
        summary["refreshFailure"] = json!({"code":failure["code"],"failedAt":failure["failedAt"],"message":failure["message"]});
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn upserts_merge_previous_records_and_roll_back() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path();
        let old = json!({"id":"m","name":"m","source":{"source":"url","url":"u"},"pluginCount":1,
            "addedAt":"t0","extra":true,"cacheTransactionId":"x","lastRefreshFailure":{"code":"c"}});
        write(storage, std::slice::from_ref(&old)).await.unwrap();
        let record = json!({"id":"m","name":"m","source":{"source":"url","url":"v"},"pluginCount":2,
            "addedAt":"t1","lastUpdated":"t1","cacheTransactionId":"y"});
        let mut upsert = upsert(storage, record).await.unwrap();
        let known = crate::market::known(storage).await.unwrap();
        assert_eq!(known[0]["addedAt"], "t0");
        assert_eq!(known[0]["extra"], true);
        assert_eq!(known[0]["cacheTransactionId"], "y");
        assert!(known[0].get("lastRefreshFailure").is_none());
        upsert.rollback(storage).await.unwrap();
        assert_eq!(crate::market::known(storage).await.unwrap()[0], old);
        persist_failure(
            storage,
            "m",
            json!({"code":"e","failedAt":"t2","message":"boom"}),
        )
        .await
        .unwrap();
        let known = crate::market::known(storage).await.unwrap();
        assert_eq!(summary(&known[0])["refreshFailure"]["message"], "boom");
        remove(storage, "m").await.unwrap();
        assert!(crate::market::known(storage).await.unwrap().is_empty());
    }
}

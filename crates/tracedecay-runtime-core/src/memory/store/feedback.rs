//! Oplog operations for `MemoryStore`.

use crate::errors::Result;

use super::MemoryStore;

impl MemoryStore<'_> {
    /// Public oplog hook for mutation flows that live outside this store
    /// (e.g. dashboard curation apply).
    pub async fn record_oplog(
        &self,
        op: &str,
        fact_id: Option<i64>,
        detail: &serde_json::Value,
    ) -> Result<()> {
        self.log_oplog(op, fact_id, detail).await
    }
}

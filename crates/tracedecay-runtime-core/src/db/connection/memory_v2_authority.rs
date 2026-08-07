use tracedecay_domain::{FactOwnerV1, UtcMicros};

use crate::db::{DatabaseMemoryTransaction, memory_v2};
use crate::errors::Result;

use super::Database;

impl Database {
    /// Marks an owner-bound bank projection dirty inside an already-open
    /// authoritative writer transaction.
    pub(crate) async fn mark_memory_v2_bank_dirty_in_transaction(
        &self,
        transaction: &DatabaseMemoryTransaction<'_>,
        owner: &FactOwnerV1,
        bank_name: &str,
        updated_at: UtcMicros,
    ) -> Result<()> {
        self.require_active_write_scope("mark memory v2 bank dirty in writer transaction")?;
        memory_v2::mark_memory_v2_bank_dirty_in_transaction(
            transaction,
            owner,
            bank_name,
            updated_at,
        )
        .await
    }

    /// Replaces an owner-bound bank projection inside an already-open
    /// authoritative writer transaction.
    pub(crate) async fn upsert_memory_v2_bank_in_transaction(
        &self,
        transaction: &DatabaseMemoryTransaction<'_>,
        owner: &FactOwnerV1,
        bank_name: &str,
        vector: &[u8],
        fact_count: u64,
        updated_at: UtcMicros,
    ) -> Result<()> {
        self.require_active_write_scope("upsert memory v2 bank in writer transaction")?;
        memory_v2::upsert_memory_v2_bank_in_transaction(
            transaction,
            owner,
            bank_name,
            vector,
            fact_count,
            updated_at,
        )
        .await
    }

    /// Deletes an empty owner-bound bank projection inside an already-open
    /// authoritative writer transaction.
    pub(crate) async fn delete_memory_v2_bank_in_transaction(
        &self,
        transaction: &DatabaseMemoryTransaction<'_>,
        owner: &FactOwnerV1,
        bank_name: &str,
    ) -> Result<()> {
        self.require_active_write_scope("delete memory v2 bank in writer transaction")?;
        memory_v2::delete_memory_v2_bank_in_transaction(transaction, owner, bank_name).await
    }

    /// Clears an owner-bound dirty-bank generation only when it matches the
    /// generation the caller rebuilt in this writer transaction.
    pub(crate) async fn clear_memory_v2_bank_dirty_in_transaction(
        &self,
        transaction: &DatabaseMemoryTransaction<'_>,
        owner: &FactOwnerV1,
        bank_name: &str,
        expected_updated_at: UtcMicros,
    ) -> Result<bool> {
        self.require_active_write_scope("clear memory v2 bank dirty in writer transaction")?;
        memory_v2::clear_memory_v2_bank_dirty_in_transaction(
            transaction,
            owner,
            bank_name,
            expected_updated_at,
        )
        .await
    }
}

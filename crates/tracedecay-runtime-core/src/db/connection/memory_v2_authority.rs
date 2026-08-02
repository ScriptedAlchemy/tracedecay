use tracedecay_domain::{ActorId, FactEventId, FactId, FactOwnerV1, SourceStoreId, UtcMicros};

use crate::db::{DatabaseMemoryTransaction, MemoryV2LegacyPurgeReceipt, memory_v2};
use crate::errors::Result;

use super::Database;

impl Database {
    /// Marks an owner-bound V23 compatibility-bank projection dirty inside an
    /// already-open authoritative writer transaction.
    pub(crate) async fn mark_memory_v2_compatibility_bank_dirty_in_transaction(
        &self,
        transaction: &DatabaseMemoryTransaction<'_>,
        owner: &FactOwnerV1,
        source_store_id: &SourceStoreId,
        bank_name: &str,
        updated_at: UtcMicros,
    ) -> Result<()> {
        self.require_active_write_scope(
            "mark memory v2 compatibility bank dirty in writer transaction",
        )?;
        memory_v2::mark_memory_v2_compatibility_bank_dirty_in_transaction(
            transaction,
            owner,
            source_store_id,
            bank_name,
            updated_at,
        )
        .await
    }

    /// Replaces an owner-bound V23 compatibility-bank projection inside an
    /// already-open authoritative writer transaction.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn upsert_memory_v2_compatibility_bank_in_transaction(
        &self,
        transaction: &DatabaseMemoryTransaction<'_>,
        owner: &FactOwnerV1,
        source_store_id: &SourceStoreId,
        bank_name: &str,
        vector: &[u8],
        fact_count: u64,
        updated_at: UtcMicros,
    ) -> Result<()> {
        self.require_active_write_scope(
            "upsert memory v2 compatibility bank in writer transaction",
        )?;
        memory_v2::upsert_memory_v2_compatibility_bank_in_transaction(
            transaction,
            owner,
            source_store_id,
            bank_name,
            vector,
            fact_count,
            updated_at,
        )
        .await
    }

    /// Deletes an empty owner-bound V23 compatibility-bank projection inside
    /// an already-open authoritative writer transaction.
    pub(crate) async fn delete_memory_v2_compatibility_bank_in_transaction(
        &self,
        transaction: &DatabaseMemoryTransaction<'_>,
        owner: &FactOwnerV1,
        source_store_id: &SourceStoreId,
        bank_name: &str,
    ) -> Result<()> {
        self.require_active_write_scope(
            "delete memory v2 compatibility bank in writer transaction",
        )?;
        memory_v2::delete_memory_v2_compatibility_bank_in_transaction(
            transaction,
            owner,
            source_store_id,
            bank_name,
        )
        .await
    }

    /// Clears an owner-bound V23 dirty-bank generation only when it matches
    /// the generation the caller rebuilt in this writer transaction.
    pub(crate) async fn clear_memory_v2_compatibility_bank_dirty_in_transaction(
        &self,
        transaction: &DatabaseMemoryTransaction<'_>,
        owner: &FactOwnerV1,
        source_store_id: &SourceStoreId,
        bank_name: &str,
        expected_updated_at: UtcMicros,
    ) -> Result<bool> {
        self.require_active_write_scope(
            "clear memory v2 compatibility bank dirty in writer transaction",
        )?;
        memory_v2::clear_memory_v2_compatibility_bank_dirty_in_transaction(
            transaction,
            owner,
            source_store_id,
            bank_name,
            expected_updated_at,
        )
        .await
    }

    /// Applies the guarded migrated-V1 deletion path inside the compatibility
    /// command's authority transaction, preserving one atomic receipt.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn purge_memory_v2_legacy_fact_payload_in_transaction(
        &self,
        transaction: &DatabaseMemoryTransaction<'_>,
        owner: &FactOwnerV1,
        source_store_id: &SourceStoreId,
        fact_id: &FactId,
        expected_last_event_id: &FactEventId,
        actor: Option<&ActorId>,
        occurred_at: UtcMicros,
    ) -> Result<MemoryV2LegacyPurgeReceipt> {
        self.require_active_write_scope(
            "purge memory v2 legacy fact payload in writer transaction",
        )?;
        memory_v2::purge_memory_v2_fact_in_transaction(
            transaction,
            owner,
            source_store_id,
            fact_id,
            expected_last_event_id,
            actor,
            occurred_at,
        )
        .await
    }
}

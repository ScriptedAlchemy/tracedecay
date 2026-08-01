use tracedecay_domain::{ActorId, FactEventId, FactId, FactOwnerV1, SourceStoreId, UtcMicros};

use crate::db::{
    DatabaseMemoryTransaction, MemoryV2CutoverOutcome, MemoryV2CutoverReceipt,
    MemoryV2FeedbackHistoryRepairBatchOutcome, MemoryV2FeedbackHistoryRepairProgress,
    MemoryV2LegacyPurgeReceipt, memory_v2,
};
use crate::errors::Result;

use super::Database;

impl Database {
    /// Reads the daemon-owned V22 repair snapshot for legacy feedback already
    /// imported before the history/map projection existed.
    ///
    /// Standalone writer-connection reader retained for owner-bound repair
    /// tests; production reads progress inside a caller-owned authority
    /// transaction via `*_in_transaction`.
    #[cfg(test)]
    pub(crate) async fn feedback_history_repair_progress(
        &self,
        owner: &FactOwnerV1,
        source_store_id: &SourceStoreId,
    ) -> Result<Option<MemoryV2FeedbackHistoryRepairProgress>> {
        let writer = self
            .writer_connection("read memory v2 feedback history repair progress")
            .await?;
        memory_v2::feedback_history_repair_progress(&writer.conn, owner, source_store_id).await
    }

    /// Reads V22 repair progress from an already-open authoritative writer
    /// transaction without opening a second writer connection.
    pub(crate) async fn feedback_history_repair_progress_in_transaction(
        &self,
        transaction: &DatabaseMemoryTransaction<'_>,
        owner: &FactOwnerV1,
        source_store_id: &SourceStoreId,
    ) -> Result<Option<MemoryV2FeedbackHistoryRepairProgress>> {
        self.require_active_write_scope("read memory v2 feedback history in writer transaction")?;
        memory_v2::feedback_history_repair_progress(transaction, owner, source_store_id).await
    }

    /// Repairs one bounded V22 feedback-history batch inside an already-open
    /// authoritative writer transaction. The caller owns commit/rollback so a
    /// repair, V1 mirror work, and receipt share one atomic outcome.
    pub(crate) async fn repair_memory_v2_feedback_history_batch_in_transaction(
        &self,
        transaction: &DatabaseMemoryTransaction<'_>,
        owner: &FactOwnerV1,
        source_store_id: &SourceStoreId,
        batch_size: i64,
    ) -> Result<MemoryV2FeedbackHistoryRepairBatchOutcome> {
        self.require_active_write_scope("repair memory v2 feedback history in writer transaction")?;
        memory_v2::repair_memory_v2_feedback_history_batch_in_transaction(
            transaction,
            owner,
            source_store_id,
            batch_size,
        )
        .await
    }

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

    pub async fn finalize_memory_v2_cutover(
        &self,
        receipt: &MemoryV2CutoverReceipt,
    ) -> Result<MemoryV2CutoverOutcome> {
        let writer = self.writer_connection("finalize memory v2 cutover").await?;
        memory_v2::finalize_memory_v2_cutover(&writer.conn, receipt).await
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

    pub async fn reopen_memory_v2_cutover_for_legacy_union(
        &self,
        owner: &FactOwnerV1,
        source_store_id: &SourceStoreId,
    ) -> Result<bool> {
        let writer = self
            .writer_connection("reopen memory v2 cutover for branch union")
            .await?;
        memory_v2::reopen_memory_v2_cutover_for_legacy_union(&writer.conn, owner, source_store_id)
            .await
    }
}

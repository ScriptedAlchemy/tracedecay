//! Per-fact derived-vector maintenance for `MemoryStore`.
//!
//! Plan 39 Task 7 (owner decision 2026-08-07, second): the persisted category
//! bank projection (`memory_banks`/`memory_bank_dirty`) was write-only and is
//! deleted. Recall re-encodes candidate vectors from canonical fact content at
//! query time; the per-fact stored vectors below leave with `memory_facts` in
//! the Step 3 legacy-mirror removal.

use crate::db::engine::params;

use crate::errors::Result;
use crate::memory::encoding::HolographicEncoder;

use super::{HRR_ALGEBRA, MemoryStore, db_error, normalized_limit};

impl MemoryStore<'_> {
    pub async fn compute_missing_vectors(&self, limit: usize) -> Result<usize> {
        let limit = normalized_limit(limit);
        let mut rows = self
            .conn
            .query(
                "SELECT fact_id FROM memory_facts
                 WHERE hrr_vector IS NULL
                    OR hrr_algebra != ?1
                    OR hrr_dim != ?2
                    OR hrr_precision != ?3
                    OR length(hrr_vector) != ?4
                 ORDER BY updated_at DESC
                 LIMIT ?5",
                params![
                    HRR_ALGEBRA,
                    HolographicEncoder::DIMENSIONS as i64,
                    HolographicEncoder::HRR_PRECISION,
                    HolographicEncoder::SERIALIZED_F32_BYTES as i64,
                    limit as i64
                ],
            )
            .await
            .map_err(|e| db_error("compute_missing_vectors", e))?;

        let mut fact_ids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("compute_missing_vectors", e))?
        {
            fact_ids.push(
                row.get::<i64>(0)
                    .map_err(|e| db_error("compute_missing_vectors", e))?,
            );
        }

        for fact_id in &fact_ids {
            if let Some(fact) = self.get_fact(*fact_id).await? {
                let vector =
                    self.encode_vector(&fact.content, &fact.entities, "compute_missing_vectors")?;
                // hrr_* are derived fields; recomputing them must not touch updated_at,
                // which retrieval uses for temporal decay and tie-breaking. Bumping it here
                // would let a read-only memory_status repair silently promote stale facts.
                self.conn
                    .execute(
                        "UPDATE memory_facts
                         SET hrr_vector = ?1,
                             hrr_algebra = ?2,
                             hrr_dim = ?3,
                             hrr_precision = ?4
                         WHERE fact_id = ?5",
                        params![
                            vector,
                            HRR_ALGEBRA,
                            HolographicEncoder::DIMENSIONS as i64,
                            HolographicEncoder::HRR_PRECISION,
                            *fact_id,
                        ],
                    )
                    .await
                    .map_err(|e| db_error("compute_missing_vectors", e))?;
            }
        }

        Ok(fact_ids.len())
    }
}

//! Vector and category-bank maintenance operations for `MemoryStore`.

use crate::db::engine::params;

use crate::errors::Result;
use crate::memory::encoding::HolographicEncoder;
use crate::memory::types::MemoryCategory;
use crate::tracedecay::current_timestamp;

use super::{
    HRR_ALGEBRA, MemoryStore, average_vectors, db_error, db_message, normalize_bank_name,
    normalized_limit, parse_category,
};

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
                self.mark_fact_banks_dirty(fact.category).await?;
            }
        }

        Ok(fact_ids.len())
    }

    pub async fn rebuild_bank(
        &self,
        bank_name: &str,
        category: Option<MemoryCategory>,
    ) -> Result<usize> {
        let (fact_count, vectors) = self.load_bank_vectors(category).await?;
        if vectors.is_empty() {
            self.conn
                .execute(
                    "DELETE FROM memory_banks WHERE bank_name = ?1",
                    params![bank_name],
                )
                .await
                .map_err(|e| db_error("rebuild_bank", e))?;
            return Ok(0);
        }

        let averaged = average_vectors(&vectors);
        let vector_bytes = HolographicEncoder::serialize(&averaged).map_err(|e| {
            db_message(
                "rebuild_bank",
                format!("failed to serialize bank vector: {e}"),
            )
        })?;
        let normalized_name = normalize_bank_name(bank_name);
        let now = current_timestamp();

        self.conn
            .execute(
                "INSERT INTO memory_banks (
                    bank_name, vector, hrr_algebra, hrr_dim, fact_count, updated_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(bank_name) DO UPDATE SET
                    vector = excluded.vector,
                    hrr_algebra = excluded.hrr_algebra,
                    hrr_dim = excluded.hrr_dim,
                    fact_count = excluded.fact_count,
                    updated_at = excluded.updated_at",
                params![
                    normalized_name,
                    vector_bytes,
                    HRR_ALGEBRA,
                    HolographicEncoder::DIMENSIONS as i64,
                    fact_count as i64,
                    now,
                ],
            )
            .await
            .map_err(|e| db_error("rebuild_bank", e))?;

        Ok(fact_count)
    }

    pub async fn rebuild_all_banks(&self) -> Result<usize> {
        let mut categories = Vec::new();
        let mut rows = self
            .conn
            .query("SELECT DISTINCT category FROM memory_facts", ())
            .await
            .map_err(|e| db_error("rebuild_all_banks", e))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("rebuild_all_banks", e))?
        {
            let category = row
                .get::<String>(0)
                .map_err(|e| db_error("rebuild_all_banks", e))?;
            categories.push(parse_category(&category, "rebuild_all_banks")?);
        }

        let mut rebuilt = 0;
        self.rebuild_bank("all", None).await?;
        rebuilt += 1;
        for category in categories {
            self.rebuild_bank(category.as_str(), Some(category)).await?;
            rebuilt += 1;
        }
        Ok(rebuilt)
    }

    pub async fn rebuild_dirty_banks(&self) -> Result<usize> {
        let mut rows = self
            .conn
            .query(
                "SELECT bank_name, updated_at FROM memory_bank_dirty ORDER BY bank_name",
                (),
            )
            .await
            .map_err(|e| db_error("rebuild_dirty_banks", e))?;
        let mut dirty_banks = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("rebuild_dirty_banks", e))?
        {
            dirty_banks.push((
                row.get::<String>(0)
                    .map_err(|e| db_error("rebuild_dirty_banks", e))?,
                row.get::<i64>(1)
                    .map_err(|e| db_error("rebuild_dirty_banks", e))?,
            ));
        }

        let mut rebuilt = 0;
        for (bank_name, dirty_updated_at) in dirty_banks {
            if bank_name == "all" {
                self.rebuild_bank("all", None).await?;
            } else {
                let category = parse_category(&bank_name, "rebuild_dirty_banks")?;
                self.rebuild_bank(category.as_str(), Some(category)).await?;
            }
            self.conn
                .execute(
                    "DELETE FROM memory_bank_dirty
                     WHERE bank_name = ?1 AND updated_at = ?2",
                    params![bank_name, dirty_updated_at],
                )
                .await
                .map_err(|e| db_error("rebuild_dirty_banks", e))?;
            rebuilt += 1;
        }
        Ok(rebuilt)
    }

    pub async fn repair_fact_vector(&self, fact_id: i64) -> Result<bool> {
        self.with_immediate_tx("repair_fact_vector", move |store| {
            Box::pin(store.repair_fact_vector_inner(fact_id))
        })
        .await
    }

    pub async fn repair_fact_vector_inner(&self, fact_id: i64) -> Result<bool> {
        let Some(fact) = self.get_fact(fact_id).await? else {
            return Err(db_message(
                "repair_fact_vector",
                format!("fact {fact_id} not found"),
            ));
        };
        self.update_fact_vector(fact_id, &fact.content, &fact.entities, "repair_fact_vector")
            .await?;
        self.mark_fact_banks_dirty(fact.category).await?;
        Ok(true)
    }
}

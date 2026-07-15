//! Session-memory (holographic fact store) surface of [`TraceDecay`].

use crate::errors::{Result, TraceDecayError};
use crate::memory::encoding::HolographicEncoder;
use crate::memory::retrieval::FactRetriever;
use crate::memory::store::MemoryStore;
use crate::memory::trust::{DEFAULT_MIN_TRUST, DEFAULT_TRUST};
use crate::memory::types::{
    AddFactOutcome, AddFactRequest, ContradictionResult, FactRecord, FactSearchResult,
    FeedbackRequest, FeedbackResult, MemoryCategory, MemoryFeedbackFunnel, MemoryRepairStats,
    MemoryStatus, SearchFactsRequest, TrustHistoryEntry, UpdateFactRequest,
};

use super::TraceDecay;

const MAX_FACT_LIMIT: usize = 200;
const DEFAULT_FACT_LIMIT: usize = 20;

fn memory_database_error(operation: &str, message: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!("{operation} failed: {message}"),
        operation: operation.to_string(),
    }
}

fn fact_result_ids(results: &[FactSearchResult]) -> Vec<i64> {
    results.iter().map(|result| result.fact.fact_id).collect()
}

fn fact_ids(facts: &[FactRecord]) -> Vec<i64> {
    facts.iter().map(|fact| fact.fact_id).collect()
}

impl TraceDecay {
    /// Add a fact to the holographic memory store. The outcome carries the
    /// stored (or pre-existing) fact plus a write-time diff report
    /// (near-duplicate / possible-conflict / secret rejection).
    pub async fn add_fact(&self, request: AddFactRequest) -> Result<AddFactOutcome> {
        let writer = self.db.writer_connection("add fact").await?;
        writer.memory_store().add_fact(request, DEFAULT_TRUST).await
    }

    /// Search facts by lexical overlap, entity metadata, category, and trust.
    pub async fn search_facts(&self, request: SearchFactsRequest) -> Result<Vec<FactSearchResult>> {
        let writer = self.db.writer_connection("search facts").await?;
        let mut results = writer
            .fact_retriever()
            .search(
                &request.query,
                request.category,
                request.min_trust,
                request.limit.unwrap_or(DEFAULT_FACT_LIMIT),
            )
            .await?;
        if !request.include_why {
            for result in &mut results {
                result.why = None;
            }
        }
        writer
            .memory_store()
            .increment_retrieval_counts(&fact_result_ids(&results))
            .await?;
        Ok(results)
    }

    /// Search facts without updating recall/access counters. This is for
    /// background enrichment surfaces such as `tracedecay_context`, where a
    /// memory match is supporting context rather than an explicit recall.
    pub async fn search_facts_untracked(
        &self,
        request: SearchFactsRequest,
    ) -> Result<Vec<FactSearchResult>> {
        let db = self.open_project_store_db_read_only().await?;
        let mut results = FactRetriever::new(db.conn())
            .search_untracked(
                &request.query,
                request.category,
                request.min_trust,
                request.limit.unwrap_or(DEFAULT_FACT_LIMIT),
            )
            .await?;
        if !request.include_why {
            for result in &mut results {
                result.why = None;
            }
        }
        Ok(results)
    }

    pub async fn probe_entity(
        &self,
        entity: &str,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactSearchResult>> {
        let writer = self.db.writer_connection("probe facts").await?;
        let results = writer
            .fact_retriever()
            .probe(entity, category, min_trust, limit)
            .await?;
        writer
            .memory_store()
            .increment_retrieval_counts(&fact_result_ids(&results))
            .await?;
        Ok(results)
    }

    pub async fn related_facts(
        &self,
        entity: &str,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactSearchResult>> {
        let writer = self.db.writer_connection("related facts").await?;
        let retriever = writer.fact_retriever();
        let related_entities = retriever.related(entity, limit).await?;
        let mut seen = std::collections::HashSet::new();
        let mut results = Vec::new();
        for related in related_entities {
            for result in retriever
                .probe(&related.name, category, min_trust, limit.saturating_mul(2))
                .await?
            {
                if seen.insert(result.fact.fact_id) {
                    results.push(result);
                    if results.len() >= limit.clamp(1, MAX_FACT_LIMIT) {
                        break;
                    }
                }
            }
            if results.len() >= limit.clamp(1, MAX_FACT_LIMIT) {
                break;
            }
        }
        writer
            .memory_store()
            .increment_retrieval_counts(&fact_result_ids(&results))
            .await?;
        Ok(results)
    }

    pub async fn reason_facts(
        &self,
        entities: &[String],
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactSearchResult>> {
        let writer = self.db.writer_connection("reason facts").await?;
        let results = writer
            .fact_retriever()
            .reason(entities, category, min_trust, limit)
            .await?;
        writer
            .memory_store()
            .increment_retrieval_counts(&fact_result_ids(&results))
            .await?;
        Ok(results)
    }

    pub async fn contradict_facts(
        &self,
        category: Option<MemoryCategory>,
        threshold: f64,
        limit: usize,
    ) -> Result<Vec<ContradictionResult>> {
        let retriever = FactRetriever::new(self.db.conn());
        if let Some(category) = category {
            return retriever.contradict(category, threshold, limit).await;
        }

        let mut out = Vec::new();
        for category in [
            MemoryCategory::General,
            MemoryCategory::UserPref,
            MemoryCategory::Project,
            MemoryCategory::Tool,
            MemoryCategory::Decision,
            MemoryCategory::CodeArea,
        ] {
            out.extend(retriever.contradict(category, threshold, limit).await?);
            if out.len() >= limit.clamp(1, MAX_FACT_LIMIT) {
                out.truncate(limit.clamp(1, MAX_FACT_LIMIT));
                break;
            }
        }
        Ok(out)
    }

    pub async fn update_fact(&self, request: UpdateFactRequest) -> Result<FactRecord> {
        let writer = self.db.writer_connection("update fact").await?;
        writer.memory_store().update_fact(request).await
    }

    pub async fn remove_fact(&self, fact_id: i64) -> Result<bool> {
        let writer = self.db.writer_connection("remove fact").await?;
        writer.memory_store().remove_fact(fact_id).await
    }

    pub async fn list_facts(
        &self,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactRecord>> {
        let writer = self.db.writer_connection("list facts").await?;
        let facts = writer
            .memory_store()
            .list_facts(category, min_trust, limit)
            .await?;
        writer
            .memory_store()
            .increment_retrieval_counts(&fact_ids(&facts))
            .await?;
        Ok(facts)
    }

    pub async fn get_fact(&self, fact_id: i64) -> Result<Option<FactRecord>> {
        MemoryStore::new(self.db.conn()).get_fact(fact_id).await
    }

    pub async fn record_fact_feedback(&self, request: FeedbackRequest) -> Result<FeedbackResult> {
        let writer = self.db.writer_connection("record fact feedback").await?;
        writer.memory_store().record_feedback_event(request).await
    }

    pub async fn fact_trust_history(&self, fact_id: i64) -> Result<Vec<TrustHistoryEntry>> {
        MemoryStore::new(self.db.conn())
            .fact_trust_history(fact_id)
            .await
    }

    async fn repair_derived_memory(store: &MemoryStore<'_>) -> Result<MemoryRepairStats> {
        let mut missing_vectors_repaired = 0;
        loop {
            let repaired = store.compute_missing_vectors(500).await?;
            if repaired == 0 {
                break;
            }
            missing_vectors_repaired += repaired;
        }

        let banks_rebuilt = store.rebuild_dirty_banks().await?;

        Ok(MemoryRepairStats {
            missing_vectors_repaired,
            banks_rebuilt,
        })
    }

    pub(crate) async fn memory_status_for_db(db: &crate::db::Database) -> Result<MemoryStatus> {
        let writer = db.writer_connection("memory status repair").await?;
        let operation = "memory_status";
        let store = writer.memory_store();
        let repair = Self::repair_derived_memory(&store).await?;
        let hrr_dim = HolographicEncoder::DIMENSIONS;
        let mut fact_rows = writer
            .query("SELECT trust_score FROM memory_facts", ())
            .await
            .map_err(|e| memory_database_error(operation, e))?;
        let row_err = |e: libsql::Error| memory_database_error(operation, e);
        let mut trust_0_025_count = 0_usize;
        let mut trust_025_050_count = 0_usize;
        let mut trust_050_075_count = 0_usize;
        let mut trust_075_100_count = 0_usize;
        let mut below_default_recall_threshold_count = 0_usize;
        let mut fact_count = 0_usize;
        while let Some(row) = fact_rows.next().await.map_err(row_err)? {
            fact_count += 1;
            let trust_score = row.get::<f64>(0).map_err(row_err)?;
            if trust_score < DEFAULT_MIN_TRUST {
                below_default_recall_threshold_count += 1;
            }
            if trust_score < 0.25 {
                trust_0_025_count += 1;
            } else if trust_score < 0.50 {
                trust_025_050_count += 1;
            } else if trust_score < 0.75 {
                trust_050_075_count += 1;
            } else {
                trust_075_100_count += 1;
            }
        }
        let mut entity_rows = writer
            .query("SELECT COUNT(*) FROM memory_entities", ())
            .await
            .map_err(|e| memory_database_error(operation, e))?;
        let entity_count = entity_rows
            .next()
            .await
            .map_err(row_err)?
            .map_or(Ok(0_i64), |row| row.get(0).map_err(row_err))?;
        let mut bank_rows = writer
            .query("SELECT COUNT(*) FROM memory_banks", ())
            .await
            .map_err(|e| memory_database_error(operation, e))?;
        let bank_count = bank_rows
            .next()
            .await
            .map_err(row_err)?
            .map_or(Ok(0_i64), |row| row.get(0).map_err(row_err))?;
        let mut aggregate_rows = writer
            .query(
                "SELECT COALESCE(SUM(helpful_count), 0),
                        COALESCE(SUM(unhelpful_count), 0),
                        COALESCE(SUM(CASE
                            WHEN hrr_vector IS NULL
                              OR hrr_algebra != 'amari_fhrr'
                              OR hrr_dim != ?1
                            THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(retrieval_count), 0),
                        COALESCE(SUM(access_count), 0),
                        COALESCE(SUM(retrieval_count > 0), 0),
                        COALESCE(SUM(helpful_count + unhelpful_count > 0), 0)
                 FROM memory_facts",
                libsql::params![hrr_dim as i64],
            )
            .await
            .map_err(|e| memory_database_error(operation, e))?;
        let Some(aggregate_row) = aggregate_rows.next().await.map_err(row_err)? else {
            return Err(memory_database_error(
                operation,
                "memory aggregate query returned no rows",
            ));
        };
        let helpful_count = aggregate_row.get::<i64>(0).map_err(row_err)?;
        let unhelpful_count = aggregate_row.get::<i64>(1).map_err(row_err)?;
        let missing_vector_count = aggregate_row.get::<i64>(2).map_err(row_err)?;
        let retrieval_count_total = aggregate_row.get::<i64>(3).map_err(row_err)?;
        let access_count_total = aggregate_row.get::<i64>(4).map_err(row_err)?;
        let retrieved_fact_count = aggregate_row.get::<i64>(5).map_err(row_err)?;
        let rated_fact_count = aggregate_row.get::<i64>(6).map_err(row_err)?;
        let feedback_total = (helpful_count + unhelpful_count).max(0) as usize;
        let seen_total = retrieval_count_total + access_count_total;
        let seen_to_feedback_ratio = if feedback_total > 0 {
            Some(seen_total / feedback_total as i64)
        } else {
            None
        };
        let feedback_funnel = MemoryFeedbackFunnel {
            retrieval_count_total,
            access_count_total,
            retrieved_fact_count: retrieved_fact_count.max(0) as usize,
            rated_fact_count: rated_fact_count.max(0) as usize,
            feedback_total,
            seen_to_feedback_ratio,
        };
        let mut backfill_rows = writer
            .query(
                "SELECT COUNT(*) FROM memory_facts
                 WHERE json_extract(metadata, '$.holographic_memory_backfill_v1') = 1",
                (),
            )
            .await
            .map_err(|e| memory_database_error(operation, e))?;
        let backfilled_count = backfill_rows
            .next()
            .await
            .map_err(row_err)?
            .map_or(Ok(0_i64), |row| row.get(0).map_err(row_err))?;
        let estimated_capacity = (hrr_dim as f64 / (hrr_dim as f64).ln()).round() as usize;
        Ok(MemoryStatus {
            fact_count,
            entity_count: entity_count as usize,
            bank_count: bank_count as usize,
            algebra_name: "amari_fhrr".to_string(),
            hrr_dim,
            estimated_capacity,
            trust_0_025_count,
            trust_025_050_count,
            trust_050_075_count,
            trust_075_100_count,
            below_default_recall_threshold_count,
            helpful_count: helpful_count as usize,
            unhelpful_count: unhelpful_count as usize,
            missing_vector_count: missing_vector_count as usize,
            legacy_backfill_complete: backfilled_count > 0,
            repair,
            feedback_funnel,
        })
    }

    pub async fn memory_status(&self) -> Result<MemoryStatus> {
        Self::memory_status_for_db(&self.db).await
    }

    pub async fn project_memory_status(&self) -> Result<MemoryStatus> {
        let db = self.open_project_store_db().await?;
        Self::memory_status_for_db(&db).await
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::db::DatabaseAuthority;

    #[tokio::test]
    async fn separate_memory_handles_serialize_mutations() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("memory.db");
        let authority = DatabaseAuthority::acquire_test(&path, "memory writer lane").unwrap();
        let (first, _) = crate::db::Database::initialize(&path, &authority)
            .await
            .unwrap();
        let (second, _) = crate::db::Database::open(&path, &authority).await.unwrap();
        let first_writer = first
            .writer_connection("hold first memory writer")
            .await
            .unwrap();
        let mut second_write = Box::pin(async {
            let writer = second.writer_connection("second memory mutation").await?;
            writer
                .memory_store()
                .add_fact(
                    AddFactRequest {
                        content: "serialized memory mutation".to_string(),
                        category: MemoryCategory::Project,
                        source: None,
                        tags: Vec::new(),
                        entities: Vec::new(),
                        trust: None,
                        metadata: serde_json::Value::Null,
                    },
                    DEFAULT_TRUST,
                )
                .await
        });

        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut second_write)
                .await
                .is_err()
        );
        drop(first_writer);
        let outcome = tokio::time::timeout(Duration::from_secs(10), second_write)
            .await
            .expect("second writer remained blocked")
            .unwrap();
        assert!(outcome.fact.is_some());
    }
}

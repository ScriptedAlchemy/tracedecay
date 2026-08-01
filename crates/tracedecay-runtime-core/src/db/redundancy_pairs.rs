// Rust guideline compliant 2026-07-09
//! Freshness-validated cache of `tracedecay_redundancy` duplicate pairs.
//!
//! The redundancy handler computes duplicate function/method pairs on demand.
//! Persisting the ranked pairs here lets other surfaces — the diagnose
//! near-duplicate enrichment, the dashboard, future tools — read the
//! last-known duplicates by an indexed lookup instead of re-running the
//! token-window scan.
//!
//! Rows are stored in the canonical `(node_a_id, node_b_id)` orientation the
//! scan already emits (`a < b` by `(file_path, start_line, id)`) and carry the
//! `source_hash` of each side. A row is only *served* when both stored hashes
//! still match the current `node_fingerprints.source_hash` for their nodes —
//! see [`Database::fresh_redundancy_pairs_for_node`] for the staleness
//! contract. `ON DELETE CASCADE` on the schema reclaims rows when either node
//! is deleted, so orphaned rows never need an explicit sweep.

use crate::db::engine::{Error as EngineError, Row, params};

use super::connection::{Database, DatabaseWriteTransaction};
use crate::errors::{Result, TraceDecayError};

/// A duplicate pair to persist. Borrows from the caller's `RedundantPair`
/// slice so the writer stays decoupled from `crate::redundancy`.
#[derive(Debug, Clone, Copy)]
pub struct RedundancyPairWrite<'a> {
    pub node_a_id: &'a str,
    pub node_b_id: &'a str,
    pub source_hash_a: &'a str,
    pub source_hash_b: &'a str,
    pub ranking_score: f64,
    pub similarity: f64,
    pub vector_cosine: f64,
    pub overlap_kind: &'a str,
    pub severity: &'a str,
    pub generic_helper_downranked: bool,
    /// UNIX seconds the pair was computed; stamped by the caller.
    pub computed_at: i64,
}

/// A stored duplicate pair row served by the reader. `node_a_id` / `node_b_id`
/// keep the canonical orientation; callers pick the partner relative to the
/// node they queried.
#[derive(Debug, Clone)]
pub struct RedundancyPairRow {
    pub node_a_id: String,
    pub node_b_id: String,
    pub ranking_score: f64,
    pub similarity: f64,
    pub vector_cosine: f64,
    pub overlap_kind: String,
    pub severity: String,
    pub generic_helper_downranked: bool,
    pub computed_at: i64,
}

impl RedundancyPairRow {
    /// The id of the pair member that is *not* `node_id`. Assumes `node_id`
    /// is one of the two members (the reader only returns such rows).
    #[must_use]
    pub fn partner_of(&self, node_id: &str) -> &str {
        if self.node_a_id == node_id {
            &self.node_b_id
        } else {
            &self.node_a_id
        }
    }
}

impl Database {
    /// Upsert computed duplicate pairs, replacing any existing row for the
    /// same `(node_a_id, node_b_id)` key. Returns the number of rows written.
    ///
    /// Callers pass pairs in canonical orientation (`a < b`); the primary key
    /// makes a re-run idempotent. Empty input is a no-op.
    pub(crate) async fn upsert_redundancy_pairs(
        &self,
        pairs: &[RedundancyPairWrite<'_>],
    ) -> Result<usize> {
        self.publish_redundancy_cache(&[], pairs).await
    }

    /// Atomically publishes every fingerprint used by a redundancy scan and
    /// the pairs computed from that exact set. CPU parsing and pair scoring
    /// happen before this method acquires the writer lane.
    pub async fn publish_redundancy_cache(
        &self,
        fingerprints: &[(&str, &crate::redundancy::Fingerprint)],
        pairs: &[RedundancyPairWrite<'_>],
    ) -> Result<usize> {
        self.publish_redundancy_cache_with_pause(fingerprints, pairs, std::future::ready(()))
            .await
    }

    async fn publish_redundancy_cache_with_pause<F>(
        &self,
        fingerprints: &[(&str, &crate::redundancy::Fingerprint)],
        pairs: &[RedundancyPairWrite<'_>],
        before_pairs: F,
    ) -> Result<usize>
    where
        F: std::future::Future<Output = ()>,
    {
        if fingerprints.is_empty() && pairs.is_empty() {
            return Ok(0);
        }

        let transaction = self
            .begin_write_transaction("publish_redundancy_cache")
            .await?;
        for (node_id, fingerprint) in fingerprints {
            super::fingerprints::upsert_fingerprint_in_transaction(
                &transaction,
                node_id,
                fingerprint,
            )
            .await?;
        }
        before_pairs.await;
        for pair in pairs {
            upsert_pair_in_transaction(&transaction, pair).await?;
        }
        transaction
            .commit()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to commit redundancy cache publication: {error}"),
                operation: "publish_redundancy_cache".to_string(),
            })?;
        Ok(pairs.len())
    }

    /// Return the cached duplicate pairs that mention `node_id`, filtered to
    /// only the **fresh** rows.
    ///
    /// Staleness contract: a row is served only when the stored `source_hash`
    /// of *both* its members still equals the current
    /// `node_fingerprints.source_hash` for that node. Any row whose either
    /// side's fingerprint is missing or has a different hash (the node's body
    /// changed, or its fingerprint was never recomputed) is filtered out — the
    /// inner join to `node_fingerprints` on `(node_id, source_hash)` enforces
    /// this. Deleted nodes drop out via `ON DELETE CASCADE` on the table, so
    /// this reader never returns a pair pointing at a vanished node.
    ///
    /// Rows are ordered by `ranking_score` descending, then by ids for a
    /// deterministic tie-break independent of storage order.
    pub async fn fresh_redundancy_pairs_for_node(
        &self,
        node_id: &str,
    ) -> Result<Vec<RedundancyPairRow>> {
        let mut rows = self
            .engine_conn()
            .query(
                "SELECT rp.node_a_id, rp.node_b_id, rp.ranking_score, rp.similarity,
                        rp.vector_cosine, rp.overlap_kind, rp.severity,
                        rp.generic_helper_downranked, rp.computed_at
                   FROM redundancy_pairs rp
                   JOIN node_fingerprints fa
                     ON fa.node_id = rp.node_a_id AND fa.source_hash = rp.source_hash_a
                   JOIN node_fingerprints fb
                     ON fb.node_id = rp.node_b_id AND fb.source_hash = rp.source_hash_b
                  WHERE rp.node_a_id = ?1 OR rp.node_b_id = ?1
                  ORDER BY rp.ranking_score DESC, rp.node_a_id ASC, rp.node_b_id ASC",
                params![node_id],
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query fresh redundancy pairs: {e}"),
                operation: "fresh_redundancy_pairs_for_node".to_string(),
            })?;

        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read redundancy pair row: {e}"),
            operation: "fresh_redundancy_pairs_for_node".to_string(),
        })? {
            out.push(row_to_pair(&row)?);
        }
        Ok(out)
    }
}

async fn upsert_pair_in_transaction(
    transaction: &DatabaseWriteTransaction<'_>,
    pair: &RedundancyPairWrite<'_>,
) -> Result<()> {
    transaction
        .execute_engine(
            "INSERT OR REPLACE INTO redundancy_pairs
                 (node_a_id, node_b_id, source_hash_a, source_hash_b,
                  ranking_score, similarity, vector_cosine, overlap_kind,
                  severity, generic_helper_downranked, computed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                pair.node_a_id,
                pair.node_b_id,
                pair.source_hash_a,
                pair.source_hash_b,
                pair.ranking_score,
                pair.similarity,
                pair.vector_cosine,
                pair.overlap_kind,
                pair.severity,
                i64::from(pair.generic_helper_downranked),
                pair.computed_at,
            ],
        )
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to upsert redundancy pair: {error}"),
            operation: "publish_redundancy_cache".to_string(),
        })?;
    Ok(())
}

fn row_to_pair(row: &Row) -> Result<RedundancyPairRow> {
    let get_err = |field: &str, e: EngineError| TraceDecayError::Database {
        message: format!("failed to read redundancy pair {field}: {e}"),
        operation: "row_to_pair".to_string(),
    };
    let downranked: i64 = row
        .get(7)
        .map_err(|e| get_err("generic_helper_downranked", e))?;
    Ok(RedundancyPairRow {
        node_a_id: row.get(0).map_err(|e| get_err("node_a_id", e))?,
        node_b_id: row.get(1).map_err(|e| get_err("node_b_id", e))?,
        ranking_score: row.get(2).map_err(|e| get_err("ranking_score", e))?,
        similarity: row.get(3).map_err(|e| get_err("similarity", e))?,
        vector_cosine: row.get(4).map_err(|e| get_err("vector_cosine", e))?,
        overlap_kind: row.get(5).map_err(|e| get_err("overlap_kind", e))?,
        severity: row.get(6).map_err(|e| get_err("severity", e))?,
        generic_helper_downranked: downranked != 0,
        computed_at: row.get(8).map_err(|e| get_err("computed_at", e))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseAuthority, TestDatabaseRuntimeMode};
    use crate::redundancy::Fingerprint;

    fn fingerprint(source_hash: &str) -> Fingerprint {
        Fingerprint {
            ast_hash: format!("ast-{source_hash}"),
            cfg_hash: format!("cfg-{source_hash}"),
            call_seq_hash: format!("calls-{source_hash}"),
            shingles: vec![1, 2, 3],
            body_tokens: 3,
            source_hash: source_hash.to_string(),
        }
    }

    fn pair<'a>(
        source_hash_a: &'a str,
        source_hash_b: &'a str,
        score: f64,
    ) -> RedundancyPairWrite<'a> {
        RedundancyPairWrite {
            node_a_id: "node-a",
            node_b_id: "node-b",
            source_hash_a,
            source_hash_b,
            ranking_score: score,
            similarity: score,
            vector_cosine: score,
            overlap_kind: "structural",
            severity: "likely",
            generic_helper_downranked: false,
            computed_at: score as i64,
        }
    }

    async fn assert_generation(db: &Database, source_hash_a: &str, ranking_score: f64) {
        let fingerprint = db.get_fingerprint("node-a").await.unwrap().unwrap();
        assert_eq!(fingerprint.source_hash, source_hash_a);
        let pairs = db.fresh_redundancy_pairs_for_node("node-a").await.unwrap();
        assert_eq!(pairs.len(), 1);
        assert!((pairs[0].ranking_score - ranking_score).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn cancelled_publication_never_exposes_a_mixed_generation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "redundancy publication").unwrap();
        let (db, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        {
            db.writer_connection("seed redundancy publication")
                .await
                .unwrap()
                .execute_batch(
                    "INSERT INTO nodes
                         (id, kind, name, qualified_name, file_path, start_line, end_line,
                          start_column, end_column, updated_at)
                     VALUES
                         ('node-a', 'function', 'a', 'a', 'a.rs', 1, 2, 0, 1, 0),
                         ('node-b', 'function', 'b', 'b', 'b.rs', 1, 2, 0, 1, 0);",
                )
                .await
                .unwrap();
        }

        let old_a = fingerprint("old-a");
        let old_b = fingerprint("old-b");
        db.publish_redundancy_cache(
            &[("node-a", &old_a), ("node-b", &old_b)],
            &[pair("old-a", "old-b", 1.0)],
        )
        .await
        .unwrap();

        let task_db = db.clone();
        let (fingerprints_written, fingerprints_written_rx) = tokio::sync::oneshot::channel();
        let (resume, resume_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let new_a = fingerprint("new-a");
            let new_b = fingerprint("new-b");
            task_db
                .publish_redundancy_cache_with_pause(
                    &[("node-a", &new_a), ("node-b", &new_b)],
                    &[pair("new-a", "new-b", 2.0)],
                    async move {
                        let _ = fingerprints_written.send(());
                        let _ = resume_rx.await;
                    },
                )
                .await
        });
        fingerprints_written_rx.await.unwrap();

        assert_generation(&db, "old-a", 1.0).await;
        task.abort();
        let _ = task.await;
        drop(resume);
        assert_generation(&db, "old-a", 1.0).await;

        let new_a = fingerprint("new-a");
        let new_b = fingerprint("new-b");
        db.publish_redundancy_cache(
            &[("node-a", &new_a), ("node-b", &new_b)],
            &[pair("new-a", "new-b", 2.0)],
        )
        .await
        .unwrap();
        assert_generation(&db, "new-a", 2.0).await;
    }
}

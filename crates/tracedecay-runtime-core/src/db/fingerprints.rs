// Rust guideline compliant 2025-10-17
use crate::db::engine::{Row, params};

use super::connection::{Database, DatabaseWriteTransaction};
use crate::errors::{Result, TraceDecayError};

// ---------------------------------------------------------------------------
// Node fingerprints (issue #83 — tracedecay_redundancy)
// ---------------------------------------------------------------------------

/// A stored fingerprint row, paired with its node id.
#[derive(Debug, Clone)]
pub struct StoredFingerprint {
    pub node_id: String,
    pub ast_hash: String,
    pub cfg_hash: String,
    pub call_seq_hash: String,
    pub shingles: Vec<u32>,
    pub body_tokens: u32,
    pub source_hash: String,
}

impl From<StoredFingerprint> for crate::redundancy::Fingerprint {
    /// Rehydrate a stored row into the in-memory scoring shape, dropping the
    /// `node_id` (which the fingerprint itself does not carry).
    fn from(stored: StoredFingerprint) -> Self {
        Self {
            ast_hash: stored.ast_hash,
            cfg_hash: stored.cfg_hash,
            call_seq_hash: stored.call_seq_hash,
            shingles: stored.shingles,
            body_tokens: stored.body_tokens as usize,
            source_hash: stored.source_hash,
        }
    }
}

impl Database {
    /// Upsert a fingerprint for a node. Replaces any existing row.
    pub async fn upsert_fingerprint(
        &self,
        node_id: &str,
        fp: &crate::redundancy::Fingerprint,
    ) -> Result<()> {
        self.publish_redundancy_cache(&[(node_id, fp)], &[]).await?;
        Ok(())
    }

    /// Fetch a single fingerprint by node id, returning `None` if missing.
    pub async fn get_fingerprint(&self, node_id: &str) -> Result<Option<StoredFingerprint>> {
        let mut rows = self
            .engine_conn()
            .query(
                "SELECT node_id, ast_hash, cfg_hash, call_seq_hash, shingles, body_tokens, source_hash
                   FROM node_fingerprints WHERE node_id = ?1",
                params![node_id],
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query fingerprint: {e}"),
                operation: "get_fingerprint".to_string(),
            })?;
        match rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read fingerprint row: {e}"),
            operation: "get_fingerprint".to_string(),
        })? {
            Some(row) => Ok(Some(row_to_fingerprint(&row)?)),
            None => Ok(None),
        }
    }

    /// Fetch cached fingerprints for the requested node ids in one query.
    ///
    /// The JSON parameter keeps the query to one bound value regardless of
    /// candidate count, while the `json_each` subquery bounds returned rows to
    /// the requested ids instead of materializing the full fingerprint table.
    /// Missing ids are omitted and row order is unspecified.
    pub async fn get_fingerprints(&self, node_ids: &[String]) -> Result<Vec<StoredFingerprint>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }
        let node_ids_json =
            serde_json::to_string(node_ids).map_err(|e| TraceDecayError::Database {
                message: format!("failed to encode fingerprint node ids: {e}"),
                operation: "get_fingerprints".to_string(),
            })?;
        let mut rows = self
            .engine_conn()
            .query(
                "SELECT node_id, ast_hash, cfg_hash, call_seq_hash, shingles, body_tokens, source_hash
                   FROM node_fingerprints
                  WHERE node_id IN (SELECT value FROM json_each(?1))",
                params![node_ids_json],
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to bulk query fingerprints: {e}"),
                operation: "get_fingerprints".to_string(),
            })?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read bulk fingerprint row: {e}"),
            operation: "get_fingerprints".to_string(),
        })? {
            out.push(row_to_fingerprint(&row)?);
        }
        Ok(out)
    }

    /// Fetch cached fingerprint rows whose `body_tokens` fall inside the
    /// inclusive `[lo, hi]` window, capped at `limit` rows.
    ///
    /// The `body_tokens` range is filtered in SQL so a large cache never
    /// materializes fully in memory — callers pass a bounded `limit` (the
    /// diagnose near-duplicate lookup caps it) so a huge cache cannot blow up
    /// the call. Each row carries its `node_id`, so results map back to graph
    /// nodes; row order is unspecified.
    pub async fn fingerprints_in_token_window(
        &self,
        lo: u32,
        hi: u32,
        limit: usize,
    ) -> Result<Vec<StoredFingerprint>> {
        let mut rows = self
            .engine_conn()
            .query(
                "SELECT node_id, ast_hash, cfg_hash, call_seq_hash, shingles, body_tokens, source_hash
                   FROM node_fingerprints
                  WHERE body_tokens >= ?1 AND body_tokens <= ?2
                  LIMIT ?3",
                params![
                    i64::from(lo),
                    i64::from(hi),
                    i64::try_from(limit).unwrap_or(i64::MAX),
                ],
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query fingerprint window: {e}"),
                operation: "fingerprints_in_token_window".to_string(),
            })?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read fingerprint window row: {e}"),
            operation: "fingerprints_in_token_window".to_string(),
        })? {
            out.push(row_to_fingerprint(&row)?);
        }
        Ok(out)
    }
}

pub(super) async fn upsert_fingerprint_in_transaction(
    transaction: &DatabaseWriteTransaction<'_>,
    node_id: &str,
    fp: &crate::redundancy::Fingerprint,
) -> Result<()> {
    transaction
        .execute_engine(
            "INSERT OR REPLACE INTO node_fingerprints
             (node_id, ast_hash, cfg_hash, call_seq_hash, shingles, body_tokens, source_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                node_id,
                fp.ast_hash.as_str(),
                fp.cfg_hash.as_str(),
                fp.call_seq_hash.as_str(),
                fp.shingles_to_string(),
                i64::try_from(fp.body_tokens).unwrap_or(i64::MAX),
                fp.source_hash.as_str(),
            ],
        )
        .await
        .map_err(|e| TraceDecayError::Database {
            message: format!("failed to upsert fingerprint: {e}"),
            operation: "upsert_fingerprint".to_string(),
        })?;
    Ok(())
}

fn row_to_fingerprint(row: &Row) -> Result<StoredFingerprint> {
    let shingles_str: String = row.get(4).map_err(|e| TraceDecayError::Database {
        message: format!("failed to read shingles: {e}"),
        operation: "row_to_fingerprint".to_string(),
    })?;
    let body_tokens_i: i64 = row.get(5).map_err(|e| TraceDecayError::Database {
        message: format!("failed to read body_tokens: {e}"),
        operation: "row_to_fingerprint".to_string(),
    })?;
    Ok(StoredFingerprint {
        node_id: row.get(0).map_err(|e| TraceDecayError::Database {
            message: format!("failed to read node_id: {e}"),
            operation: "row_to_fingerprint".to_string(),
        })?,
        ast_hash: row.get(1).map_err(|e| TraceDecayError::Database {
            message: format!("failed to read ast_hash: {e}"),
            operation: "row_to_fingerprint".to_string(),
        })?,
        cfg_hash: row.get(2).map_err(|e| TraceDecayError::Database {
            message: format!("failed to read cfg_hash: {e}"),
            operation: "row_to_fingerprint".to_string(),
        })?,
        call_seq_hash: row.get(3).map_err(|e| TraceDecayError::Database {
            message: format!("failed to read call_seq_hash: {e}"),
            operation: "row_to_fingerprint".to_string(),
        })?,
        shingles: crate::redundancy::Fingerprint::shingles_from_string(&shingles_str),
        body_tokens: u32::try_from(body_tokens_i).unwrap_or(u32::MAX),
        source_hash: row.get(6).map_err(|e| TraceDecayError::Database {
            message: format!("failed to read source_hash: {e}"),
            operation: "row_to_fingerprint".to_string(),
        })?,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::Database;
    use crate::db::{DatabaseAuthority, StoredFingerprint, TestDatabaseRuntimeMode};
    use crate::redundancy::Fingerprint;
    use crate::types::{Node, NodeKind, Visibility};

    fn test_node(id: &str) -> Node {
        Node {
            id: id.to_string(),
            kind: NodeKind::Function,
            name: id.to_string(),
            qualified_name: id.to_string(),
            file_path: "src/lib.rs".to_string(),
            start_line: 0,
            attrs_start_line: 0,
            end_line: 8,
            start_column: 0,
            end_column: 1,
            signature: None,
            docstring: None,
            visibility: Visibility::default(),
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            updated_at: 0,
            parent_id: None,
        }
    }

    fn test_fingerprint(id: &str) -> Fingerprint {
        Fingerprint {
            ast_hash: format!("ast-{id}"),
            cfg_hash: format!("cfg-{id}"),
            call_seq_hash: format!("call-{id}"),
            shingles: vec![1, 2, 3],
            body_tokens: 42,
            source_hash: format!("source-{id}"),
        }
    }

    async fn seeded_database() -> (tempfile::TempDir, Database) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority =
            DatabaseAuthority::acquire_test(&path, "fingerprint bulk read tests").unwrap();
        let (db, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        for id in ["alpha", "beta", "gamma"] {
            db.insert_node(&test_node(id)).await.unwrap();
            db.upsert_fingerprint(id, &test_fingerprint(id))
                .await
                .unwrap();
        }
        (temp, db)
    }

    fn by_id(rows: Vec<StoredFingerprint>) -> std::collections::HashMap<String, StoredFingerprint> {
        rows.into_iter()
            .map(|fingerprint| (fingerprint.node_id.clone(), fingerprint))
            .collect()
    }

    #[tokio::test]
    async fn bulk_read_returns_only_requested_fingerprints() {
        let (_temp, db) = seeded_database().await;
        let requested = vec![
            "beta".to_string(),
            "missing".to_string(),
            "alpha".to_string(),
        ];

        let rows = by_id(db.get_fingerprints(&requested).await.unwrap());

        assert_eq!(rows.len(), 2);
        assert_eq!(rows["alpha"].source_hash, "source-alpha");
        assert_eq!(rows["beta"].source_hash, "source-beta");
        assert!(!rows.contains_key("gamma"));
    }

    #[tokio::test]
    async fn bulk_read_short_circuits_an_empty_request() {
        let (_temp, db) = seeded_database().await;

        assert!(db.get_fingerprints(&[]).await.unwrap().is_empty());
    }
}

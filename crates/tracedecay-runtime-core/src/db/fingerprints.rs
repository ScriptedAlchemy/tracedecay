// Rust guideline compliant 2025-10-17
use libsql::params;

use super::connection::Database;
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
        self.conn()
            .execute(
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

    /// Fetch a single fingerprint by node id, returning `None` if missing.
    pub async fn get_fingerprint(&self, node_id: &str) -> Result<Option<StoredFingerprint>> {
        let mut rows = self
            .conn()
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
            .conn()
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

fn row_to_fingerprint(row: &libsql::Row) -> Result<StoredFingerprint> {
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

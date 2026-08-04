// Rust guideline compliant 2025-10-17
use libsql::params;

use super::connection::Database;
use super::rows::row_to_edge;
use super::sql::collect_rows;
use crate::errors::{Result, TraceDecayError};
use crate::types::*;

impl Database {
    /// Inserts a single edge, skipping silently if either endpoint is missing.
    pub async fn insert_edge(&self, edge: &Edge) -> Result<()> {
        // Contains is denormalized to nodes.parent_id since v9. Fold the
        // edge into an UPDATE rather than writing a row to the edges table.
        if edge.kind == EdgeKind::Contains {
            self.conn()
                .execute(
                    "UPDATE nodes SET parent_id = ?1 WHERE id = ?2",
                    params![edge.source.as_str(), edge.target.as_str()],
                )
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to set parent_id: {e}"),
                    operation: "insert_edge".to_string(),
                })?;
            return Ok(());
        }
        self.conn()
            .execute(
                "INSERT OR IGNORE INTO edges (source, target, kind, line) \
                 SELECT ?1, ?2, ?3, ?4 \
                 WHERE EXISTS (SELECT 1 FROM nodes WHERE id = ?1) \
                   AND EXISTS (SELECT 1 FROM nodes WHERE id = ?2)",
                params![
                    edge.source.as_str(),
                    edge.target.as_str(),
                    edge.kind.as_str(),
                    edge.line.map(i64::from)
                ],
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to insert edge: {e}"),
                operation: "insert_edge".to_string(),
            })?;
        Ok(())
    }

    /// Inserts a batch of edges inside a single transaction.
    ///
    /// Edges whose source or target node does not yet exist are silently
    /// skipped (#58). They will be picked up on a future sync once the
    /// referenced file is indexed. `Contains` edges are denormalized into
    /// `nodes.parent_id` via UPDATE; they do not produce edge rows.
    pub async fn insert_edges(&self, edges: &[Edge]) -> Result<()> {
        if edges.is_empty() {
            return Ok(());
        }

        self.with_batch_transaction("insert_edges", async {
            // Conditional INSERT: only insert when both endpoints exist in
            // `nodes`. This avoids FK violations during incremental sync
            // when an edge references a node from a not-yet-indexed file.
            let stmt = self
                .conn()
                .prepare(
                    "INSERT OR IGNORE INTO edges (source, target, kind, line) \
                     SELECT ?1, ?2, ?3, ?4 \
                     WHERE EXISTS (SELECT 1 FROM nodes WHERE id = ?1) \
                       AND EXISTS (SELECT 1 FROM nodes WHERE id = ?2)",
                )
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to prepare: {e}"),
                    operation: "insert_edges".to_string(),
                })?;

            let parent_stmt = self
                .conn()
                .prepare("UPDATE nodes SET parent_id = ?1 WHERE id = ?2")
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to prepare parent update: {e}"),
                    operation: "insert_edges".to_string(),
                })?;

            for edge in edges {
                if edge.kind == EdgeKind::Contains {
                    if let Err(e) = parent_stmt
                        .execute(params![edge.source.as_str(), edge.target.as_str()])
                        .await
                    {
                        parent_stmt.reset();
                        return Err(TraceDecayError::Database {
                            message: format!("failed to set parent_id: {e}"),
                            operation: "insert_edges".to_string(),
                        });
                    }
                    parent_stmt.reset();
                    continue;
                }
                if let Err(e) = stmt
                    .execute(params![
                        edge.source.as_str(),
                        edge.target.as_str(),
                        edge.kind.as_str(),
                        edge.line.map(i64::from),
                    ])
                    .await
                {
                    stmt.reset();
                    return Err(TraceDecayError::Database {
                        message: format!("failed to insert edge: {e}"),
                        operation: "insert_edges".to_string(),
                    });
                }
                stmt.reset();
            }

            Ok(())
        })
        .await
    }

    /// Returns outgoing edges from a source node, optionally filtered by edge kinds.
    ///
    /// If `kinds` is empty, all outgoing edges are returned.
    pub async fn get_outgoing_edges(
        &self,
        source_id: &str,
        kinds: &[EdgeKind],
    ) -> Result<Vec<Edge>> {
        if kinds.is_empty() {
            let mut rows = self
                .conn()
                .query(
                    "SELECT source, target, kind, line FROM edges WHERE source = ?1",
                    params![source_id],
                )
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to query outgoing edges: {e}"),
                    operation: "get_outgoing_edges".to_string(),
                })?;

            collect_rows(&mut rows, row_to_edge, "get_outgoing_edges").await
        } else {
            let placeholders: Vec<String> = kinds
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", i + 2))
                .collect();
            let sql = format!(
                "SELECT source, target, kind, line FROM edges WHERE source = ?1 AND kind IN ({})",
                placeholders.join(", ")
            );

            let mut param_values: Vec<libsql::Value> = Vec::new();
            param_values.push(libsql::Value::Text(source_id.to_string()));
            for k in kinds {
                param_values.push(libsql::Value::Text(k.as_str().to_string()));
            }

            let mut rows = self
                .conn()
                .query(&sql, libsql::params_from_iter(param_values))
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to query outgoing edges: {e}"),
                    operation: "get_outgoing_edges".to_string(),
                })?;

            collect_rows(&mut rows, row_to_edge, "get_outgoing_edges").await
        }
    }

    /// Returns incoming edges to a target node, optionally filtered by edge kinds.
    ///
    /// If `kinds` is empty, all incoming edges are returned.
    pub async fn get_incoming_edges(
        &self,
        target_id: &str,
        kinds: &[EdgeKind],
    ) -> Result<Vec<Edge>> {
        if kinds.is_empty() {
            let mut rows = self
                .conn()
                .query(
                    "SELECT source, target, kind, line FROM edges WHERE target = ?1",
                    params![target_id],
                )
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to query incoming edges: {e}"),
                    operation: "get_incoming_edges".to_string(),
                })?;

            collect_rows(&mut rows, row_to_edge, "get_incoming_edges").await
        } else {
            let placeholders: Vec<String> = kinds
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", i + 2))
                .collect();
            let sql = format!(
                "SELECT source, target, kind, line FROM edges WHERE target = ?1 AND kind IN ({})",
                placeholders.join(", ")
            );

            let mut param_values: Vec<libsql::Value> = Vec::new();
            param_values.push(libsql::Value::Text(target_id.to_string()));
            for k in kinds {
                param_values.push(libsql::Value::Text(k.as_str().to_string()));
            }

            let mut rows = self
                .conn()
                .query(&sql, libsql::params_from_iter(param_values))
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to query incoming edges: {e}"),
                    operation: "get_incoming_edges".to_string(),
                })?;

            collect_rows(&mut rows, row_to_edge, "get_incoming_edges").await
        }
    }

    /// Returns all incoming edges for many target nodes in a single query.
    ///
    /// Used by the bulk `callers_for` MCP tool: clients pass a list of item
    /// IDs and get back, for each id, the set of nodes pointing at it via
    /// the requested edge kinds. One round-trip replaces N round-trips
    /// through `get_incoming_edges`.
    ///
    /// When `kinds` is empty, all edge kinds are returned.
    pub async fn get_incoming_edges_bulk(
        &self,
        target_ids: &[String],
        kinds: &[EdgeKind],
    ) -> Result<Vec<Edge>> {
        if target_ids.is_empty() {
            return Ok(Vec::new());
        }

        let target_placeholders: Vec<String> =
            (1..=target_ids.len()).map(|i| format!("?{i}")).collect();
        let mut param_values: Vec<libsql::Value> = target_ids
            .iter()
            .map(|id| libsql::Value::Text(id.clone()))
            .collect();

        let sql = if kinds.is_empty() {
            format!(
                "SELECT source, target, kind, line FROM edges WHERE target IN ({})",
                target_placeholders.join(", ")
            )
        } else {
            let kind_placeholders: Vec<String> = (1..=kinds.len())
                .map(|i| format!("?{}", target_ids.len() + i))
                .collect();
            for k in kinds {
                param_values.push(libsql::Value::Text(k.as_str().to_string()));
            }
            format!(
                "SELECT source, target, kind, line FROM edges \
                 WHERE target IN ({}) AND kind IN ({})",
                target_placeholders.join(", "),
                kind_placeholders.join(", ")
            )
        };

        let mut rows = self
            .conn()
            .query(&sql, libsql::params_from_iter(param_values))
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query bulk incoming edges: {e}"),
                operation: "get_incoming_edges_bulk".to_string(),
            })?;

        collect_rows(&mut rows, row_to_edge, "get_incoming_edges_bulk").await
    }

    /// Returns every edge in the database.
    pub async fn get_all_edges(&self) -> Result<Vec<Edge>> {
        let mut rows = self
            .conn()
            .query("SELECT source, target, kind, line FROM edges", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query all edges: {e}"),
                operation: "get_all_edges".to_string(),
            })?;

        collect_rows(&mut rows, row_to_edge, "get_all_edges").await
    }

    /// Deletes all edges originating from a given source node.
    pub async fn delete_edges_by_source(&self, source_id: &str) -> Result<()> {
        self.conn()
            .execute("DELETE FROM edges WHERE source = ?1", params![source_id])
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to delete edges by source: {e}"),
                operation: "delete_edges_by_source".to_string(),
            })?;
        Ok(())
    }

    /// Returns edges where both source and target are in the given node ID set.
    ///
    /// Batches queries in groups of 500 IDs to avoid SQL parameter limits.
    pub async fn get_internal_edges(&self, node_ids: &[String]) -> Result<Vec<Edge>> {
        const BATCH_SIZE: usize = 500;
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Build a set of IDs for filtering targets in memory, then query
        // edges from each batch of sources.
        let id_set: std::collections::HashSet<&str> =
            node_ids.iter().map(std::string::String::as_str).collect();
        let mut all_edges = Vec::new();
        let mut offset = 0;
        while offset < node_ids.len() {
            let end = (offset + BATCH_SIZE).min(node_ids.len());
            let batch = &node_ids[offset..end];

            let placeholders: Vec<String> = batch
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", i + 1))
                .collect();
            let sql = format!(
                "SELECT source, target, kind, line FROM edges WHERE source IN ({})",
                placeholders.join(", ")
            );

            let param_values: Vec<libsql::Value> = batch
                .iter()
                .map(|id| libsql::Value::Text(id.clone()))
                .collect();

            let mut rows = self
                .conn()
                .query(&sql, libsql::params_from_iter(param_values))
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to query internal edges: {e}"),
                    operation: "get_internal_edges".to_string(),
                })?;

            let batch_edges: Vec<Edge> =
                collect_rows(&mut rows, row_to_edge, "get_internal_edges").await?;

            // Keep only edges whose target is also in the node set.
            for edge in batch_edges {
                if id_set.contains(edge.target.as_str()) {
                    all_edges.push(edge);
                }
            }

            offset = end;
        }

        Ok(all_edges)
    }
}

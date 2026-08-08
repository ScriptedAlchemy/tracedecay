// Rust guideline compliant 2025-10-17
use crate::db::engine::{Value, params, params_from_iter};

use super::connection::{Database, DatabaseEngineReadSnapshot, DatabaseWriteTransaction};
use super::rows::row_to_edge;
use super::sql::{collect_rowid_pages, collect_rowid_pages_with_controlled, collect_rows};
use crate::errors::{Result, TraceDecayError};
use crate::types::*;

/// Columns `row_to_edge` reads, and therefore the index of the trailing
/// `rowid` cursor column in a paged edge scan.
pub(super) const EDGE_COLUMNS: i32 = 4;

/// The only two columns an edge traversal can use as its endpoint.
///
/// Keeping the column and its composite index together prevents a dynamic SQL
/// caller from changing the query shape independently of its index hint.
#[derive(Clone, Copy, Debug)]
pub(super) enum EdgeEndpoint {
    Source,
    Target,
}

impl EdgeEndpoint {
    const fn column(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Target => "target",
        }
    }

    const fn index(self) -> &'static str {
        match self {
            Self::Source => "idx_edges_source_kind",
            Self::Target => "idx_edges_target_kind",
        }
    }
}

pub(super) fn single_edges_by_endpoint_page_sql(
    endpoint: EdgeEndpoint,
    kind_count: usize,
) -> String {
    let kind_clause = if kind_count == 0 {
        String::new()
    } else {
        let placeholders: Vec<String> = (0..kind_count)
            .map(|index| format!("?{}", index + 2))
            .collect();
        format!(" AND kind IN ({})", placeholders.join(", "))
    };
    let cursor_param = kind_count + 2;
    format!(
        "SELECT source, target, kind, line, rowid FROM edges \
         WHERE {} = ?1{kind_clause} AND rowid > ?{cursor_param} \
         ORDER BY rowid LIMIT ?{}",
        endpoint.column(),
        cursor_param + 1
    )
}

/// Builds the bulk endpoint query with all endpoint ids carried in one JSON
/// array parameter. Only the small, caller-selected edge-kind set contributes
/// SQL bind parameters.
pub(super) fn bulk_edges_by_endpoint_page_sql(endpoint: EdgeEndpoint, kind_count: usize) -> String {
    let kind_clause = if kind_count == 0 {
        String::new()
    } else {
        let placeholders: Vec<String> = (0..kind_count)
            .map(|index| format!("?{}", index + 2))
            .collect();
        format!(" AND kind IN ({})", placeholders.join(", "))
    };
    let cursor_param = kind_count + 2;
    format!(
        "SELECT source, target, kind, line, rowid FROM edges INDEXED BY {} \
         WHERE {} IN (SELECT value FROM json_each(?1)){kind_clause} \
           AND rowid > ?{cursor_param} ORDER BY rowid LIMIT ?{}",
        endpoint.index(),
        endpoint.column(),
        cursor_param + 1
    )
}

#[derive(Clone, Debug)]
pub struct EdgesByEndpointPage {
    pub edges: Vec<Edge>,
    pub has_more: bool,
    pub rows_read: usize,
}

pub(super) async fn read_edges_by_endpoint_page_controlled<C, F>(
    conn: &C,
    endpoint: EdgeEndpoint,
    node_ids: &[String],
    kinds: &[EdgeKind],
    limit: usize,
    mut checkpoint: F,
) -> Result<EdgesByEndpointPage>
where
    C: crate::db::engine::QueryExecutor + ?Sized,
    F: FnMut() -> Result<()>,
{
    if node_ids.is_empty() || limit == 0 {
        return Ok(EdgesByEndpointPage {
            edges: Vec::new(),
            has_more: false,
            rows_read: 0,
        });
    }
    let query_limit = limit
        .checked_add(1)
        .ok_or_else(|| TraceDecayError::Database {
            message: "edge page limit overflowed".to_owned(),
            operation: "get_incoming_edges_bulk_page".to_owned(),
        })?;
    let (mut values, sql) = if let [node_id] = node_ids {
        (
            vec![Value::Text(node_id.clone())],
            single_edges_by_endpoint_page_sql(endpoint, kinds.len()),
        )
    } else {
        let encoded =
            serde_json::to_string(node_ids).map_err(|error| TraceDecayError::Database {
                message: format!("failed to encode bounded edge endpoints: {error}"),
                operation: "get_incoming_edges_bulk_page".to_owned(),
            })?;
        (
            vec![Value::Text(encoded)],
            bulk_edges_by_endpoint_page_sql(endpoint, kinds.len()),
        )
    };
    values.extend(
        kinds
            .iter()
            .map(|kind| Value::Text(kind.as_str().to_owned())),
    );
    values.push(Value::Integer(i64::MIN));
    values.push(Value::Integer(i64::try_from(query_limit).map_err(
        |error| TraceDecayError::Database {
            message: format!("invalid edge page limit: {error}"),
            operation: "get_incoming_edges_bulk_page".to_owned(),
        },
    )?));
    checkpoint()?;
    let mut rows = conn
        .query(&sql, params_from_iter(values))
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to query bounded incoming edges: {error}"),
            operation: "get_incoming_edges_bulk_page".to_owned(),
        })?;
    let mut edges = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to read bounded incoming edge: {error}"),
            operation: "get_incoming_edges_bulk_page".to_owned(),
        })?
    {
        if !edges.is_empty() && edges.len() % 64 == 0 {
            checkpoint()?;
        }
        edges.push(
            row_to_edge(&row).map_err(|error| TraceDecayError::Database {
                message: format!("failed to map bounded incoming edge: {error}"),
                operation: "get_incoming_edges_bulk_page".to_owned(),
            })?,
        );
    }
    checkpoint()?;
    let rows_read = edges.len();
    let has_more = rows_read > limit;
    edges.truncate(limit);
    Ok(EdgesByEndpointPage {
        edges,
        has_more,
        rows_read,
    })
}

pub(super) async fn read_edges_by_endpoint_controlled<C, F>(
    conn: &C,
    endpoint: EdgeEndpoint,
    node_ids: &[String],
    kinds: &[EdgeKind],
    operation: &'static str,
    checkpoint: F,
) -> Result<Vec<Edge>>
where
    C: crate::db::engine::QueryExecutor + ?Sized,
    F: FnMut() -> Result<()>,
{
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }
    let (mut leading, sql) = if let [node_id] = node_ids {
        (
            vec![Value::Text(node_id.clone())],
            single_edges_by_endpoint_page_sql(endpoint, kinds.len()),
        )
    } else {
        let encoded =
            serde_json::to_string(node_ids).map_err(|error| TraceDecayError::Database {
                message: format!("failed to encode bulk edge endpoints: {error}"),
                operation: operation.to_string(),
            })?;
        (
            vec![Value::Text(encoded)],
            bulk_edges_by_endpoint_page_sql(endpoint, kinds.len()),
        )
    };
    for kind in kinds {
        leading.push(Value::Text(kind.as_str().to_string()));
    }
    collect_rowid_pages_with_controlled(
        conn,
        &sql,
        &leading,
        EDGE_COLUMNS,
        row_to_edge,
        operation,
        checkpoint,
    )
    .await
}

impl DatabaseEngineReadSnapshot {
    pub async fn get_incoming_edges_bulk_page_controlled<F>(
        &self,
        target_ids: &[String],
        kinds: &[EdgeKind],
        limit: usize,
        checkpoint: F,
    ) -> Result<EdgesByEndpointPage>
    where
        F: FnMut() -> Result<()>,
    {
        read_edges_by_endpoint_page_controlled(
            self,
            EdgeEndpoint::Target,
            target_ids,
            kinds,
            limit,
            checkpoint,
        )
        .await
    }

    pub async fn get_incoming_edges_bulk_controlled<F>(
        &self,
        target_ids: &[String],
        kinds: &[EdgeKind],
        checkpoint: F,
    ) -> Result<Vec<Edge>>
    where
        F: FnMut() -> Result<()>,
    {
        read_edges_by_endpoint_controlled(
            self,
            EdgeEndpoint::Target,
            target_ids,
            kinds,
            "get_incoming_edges_bulk",
            checkpoint,
        )
        .await
    }
}

impl Database {
    /// Inserts a single edge, skipping silently if either endpoint is missing.
    pub async fn insert_edge(&self, edge: &Edge) -> Result<()> {
        let transaction = self.begin_write_transaction("insert_edge").await?;
        self.insert_edge_unguarded(&transaction, edge).await?;
        transaction.commit().await
    }

    pub(crate) async fn insert_edge_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
        edge: &Edge,
    ) -> Result<()> {
        // Contains is denormalized to nodes.parent_id since v9. Fold the
        // edge into an UPDATE rather than writing a row to the edges table.
        if edge.kind == EdgeKind::Contains {
            transaction
                .execute_engine(
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
        transaction
            .execute_engine(
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

        let transaction = self.begin_write_transaction("insert_edges").await?;
        self.insert_edges_unguarded(&transaction, edges).await?;
        transaction.commit().await
    }

    pub async fn insert_edges_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
        edges: &[Edge],
    ) -> Result<()> {
        if edges.is_empty() {
            return Ok(());
        }

        // Conditional INSERT: only insert when both endpoints exist in
        // `nodes`. This avoids FK violations during incremental sync
        // when an edge references a node from a not-yet-indexed file.
        let stmt = transaction
            .prepare_engine(
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

        let parent_stmt = transaction
            .prepare_engine("UPDATE nodes SET parent_id = ?1 WHERE id = ?2")
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

        drop(parent_stmt);
        drop(stmt);
        Ok(())
    }

    /// Returns outgoing edges from a source node, optionally filtered by edge kinds.
    ///
    /// If `kinds` is empty, all outgoing edges are returned.
    ///
    /// Read through `rowid` keyset pages. One endpoint is not a bound: a hub
    /// symbol on a real repository carries more edges than the `SQLite` runtime
    /// will materialize for one query, and the runtime refuses an oversized
    /// query outright rather than truncating it.
    pub async fn get_outgoing_edges(
        &self,
        source_id: &str,
        kinds: &[EdgeKind],
    ) -> Result<Vec<Edge>> {
        self.edges_by_endpoint(
            EdgeEndpoint::Source,
            &[source_id.to_string()],
            kinds,
            "get_outgoing_edges",
        )
        .await
    }

    /// Returns incoming edges to a target node, optionally filtered by edge kinds.
    ///
    /// If `kinds` is empty, all incoming edges are returned.
    ///
    /// Paged for the same reason as [`Database::get_outgoing_edges`].
    pub async fn get_incoming_edges(
        &self,
        target_id: &str,
        kinds: &[EdgeKind],
    ) -> Result<Vec<Edge>> {
        self.edges_by_endpoint(
            EdgeEndpoint::Target,
            &[target_id.to_string()],
            kinds,
            "get_incoming_edges",
        )
        .await
    }

    /// Shared keyset-paged read of every edge touching any of `node_ids`
    /// through the caller-selected endpoint.
    async fn edges_by_endpoint(
        &self,
        endpoint: EdgeEndpoint,
        node_ids: &[String],
        kinds: &[EdgeKind],
        operation: &'static str,
    ) -> Result<Vec<Edge>> {
        self.edges_by_endpoint_controlled(endpoint, node_ids, kinds, operation, || Ok(()))
            .await
    }

    async fn edges_by_endpoint_controlled<F>(
        &self,
        endpoint: EdgeEndpoint,
        node_ids: &[String],
        kinds: &[EdgeKind],
        operation: &'static str,
        checkpoint: F,
    ) -> Result<Vec<Edge>>
    where
        F: FnMut() -> Result<()>,
    {
        read_edges_by_endpoint_controlled(
            &self.engine_conn(),
            endpoint,
            node_ids,
            kinds,
            operation,
            checkpoint,
        )
        .await
    }

    /// Returns all outgoing edges for many source nodes in a single query.
    ///
    /// Mirrors `get_incoming_edges_bulk` but keys off `source` instead of
    /// `target`: callers that would otherwise loop `get_outgoing_edges` once
    /// per source node (e.g. `TraceDecay::get_impls`) can batch them into one
    /// round-trip.
    ///
    /// When `kinds` is empty, all edge kinds are returned.
    ///
    /// Paged for the same reason as [`Database::get_outgoing_edges`], and more
    /// so: a bulk frontier multiplies one node's fan-out by the number of ids.
    pub async fn get_outgoing_edges_bulk(
        &self,
        source_ids: &[String],
        kinds: &[EdgeKind],
    ) -> Result<Vec<Edge>> {
        self.edges_by_endpoint(
            EdgeEndpoint::Source,
            source_ids,
            kinds,
            "get_outgoing_edges_bulk",
        )
        .await
    }

    /// Returns all incoming edges for many target nodes in a single query.
    ///
    /// Used by the bulk `callers_for` MCP tool: clients pass a list of item
    /// IDs and get back, for each id, the set of nodes pointing at it via
    /// the requested edge kinds. One round-trip replaces N round-trips
    /// through `get_incoming_edges`.
    ///
    /// When `kinds` is empty, all edge kinds are returned.
    ///
    /// Paged for the same reason as [`Database::get_outgoing_edges_bulk`].
    pub async fn get_incoming_edges_bulk(
        &self,
        target_ids: &[String],
        kinds: &[EdgeKind],
    ) -> Result<Vec<Edge>> {
        self.edges_by_endpoint(
            EdgeEndpoint::Target,
            target_ids,
            kinds,
            "get_incoming_edges_bulk",
        )
        .await
    }

    /// [`Database::get_incoming_edges_bulk`] with cooperative page checkpoints.
    pub async fn get_incoming_edges_bulk_controlled<F>(
        &self,
        target_ids: &[String],
        kinds: &[EdgeKind],
        checkpoint: F,
    ) -> Result<Vec<Edge>>
    where
        F: FnMut() -> Result<()>,
    {
        self.edges_by_endpoint_controlled(
            EdgeEndpoint::Target,
            target_ids,
            kinds,
            "get_incoming_edges_bulk",
            checkpoint,
        )
        .await
    }

    /// Returns every edge in the database.
    /// Read through `rowid` keyset pages: whole-table reads on a real project
    /// exceed what the `SQLite` runtime will materialize for one query.
    pub async fn get_all_edges(&self) -> Result<Vec<Edge>> {
        collect_rowid_pages(
            &self.engine_conn(),
            "SELECT source, target, kind, line, rowid FROM edges
             WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
            EDGE_COLUMNS,
            row_to_edge,
            "get_all_edges",
        )
        .await
    }

    /// Deletes all edges originating from a given source node.
    pub async fn delete_edges_by_source(&self, source_id: &str) -> Result<()> {
        let transaction = self
            .begin_write_transaction("delete_edges_by_source")
            .await?;
        self.delete_edges_by_source_unguarded(&transaction, source_id)
            .await?;
        transaction.commit().await
    }

    pub(crate) async fn delete_edges_by_source_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
        source_id: &str,
    ) -> Result<()> {
        transaction
            .execute_engine("DELETE FROM edges WHERE source = ?1", params![source_id])
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

            let param_values: Vec<Value> = batch.iter().map(|id| Value::Text(id.clone())).collect();

            let mut rows = self
                .engine_conn()
                .query(&sql, params_from_iter(param_values))
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

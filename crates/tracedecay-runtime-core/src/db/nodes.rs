// Rust guideline compliant 2025-10-17
use crate::db::engine::{Value, params, params_from_iter};

use super::connection::{Database, DatabaseWriteTransaction};
use super::rows::{NODE_COLUMNS, NODE_SELECT_COLUMNS, node_select_columns, row_to_node};
use super::sql::{
    build_qmark_placeholders, collect_rowid_pages, collect_rowid_pages_with, collect_rows, opt_str,
    push_int, push_opt_quoted, push_quoted,
};
use crate::errors::{Result, TraceDecayError};
use crate::types::*;

/// One `rowid` keyset page of the nodes declared by a single file.
pub(super) const NODES_BY_FILE_PAGE_SQL: &str = concat!(
    "SELECT ",
    node_select_columns!(),
    ", rowid FROM nodes WHERE file_path = ?1 AND rowid > ?2 ORDER BY rowid LIMIT ?3"
);

/// [`NODES_BY_FILE_PAGE_SQL`] narrowed to just the node ids.
pub(super) const NODE_IDS_BY_FILE_PAGE_SQL: &str =
    "SELECT id, rowid FROM nodes WHERE file_path = ?1 AND rowid > ?2 ORDER BY rowid LIMIT ?3";

/// One `rowid` keyset page of a single node kind.
pub(super) const NODES_BY_KIND_PAGE_SQL: &str = concat!(
    "SELECT ",
    node_select_columns!(),
    ", rowid FROM nodes WHERE kind = ?1 AND rowid > ?2 ORDER BY rowid LIMIT ?3"
);

impl Database {
    /// Inserts or replaces a single node.
    pub async fn insert_node(&self, node: &Node) -> Result<()> {
        let transaction = self.begin_write_transaction("insert_node").await?;
        self.insert_node_unguarded(&transaction, node).await?;
        transaction.commit().await
    }

    pub(crate) async fn insert_node_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
        node: &Node,
    ) -> Result<()> {
        transaction
            .execute_engine(
                "INSERT OR REPLACE INTO nodes
                (id, kind, name, qualified_name, file_path,
                 start_line, end_line, start_column, end_column,
                 docstring, signature, visibility, is_async,
                 branches, loops, returns, max_nesting,
                 unsafe_blocks, unchecked_calls, assertions, updated_at, attrs_start_line, parent_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
                params![
                    node.id.as_str(),
                    node.kind.as_str(),
                    node.name.as_str(),
                    node.qualified_name.as_str(),
                    node.file_path.as_str(),
                    i64::from(node.start_line),
                    i64::from(node.end_line),
                    i64::from(node.start_column),
                    i64::from(node.end_column),
                    opt_str(node.docstring.as_deref()),
                    opt_str(node.signature.as_deref()),
                    node.visibility.as_str(),
                    i64::from(node.is_async),
                    i64::from(node.branches),
                    i64::from(node.loops),
                    i64::from(node.returns),
                    i64::from(node.max_nesting),
                    i64::from(node.unsafe_blocks),
                    i64::from(node.unchecked_calls),
                    i64::from(node.assertions),
                    node.updated_at as i64,
                    i64::from(node.attrs_start_line),
                    opt_str(node.parent_id.as_deref()),
                ],
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to insert node: {e}"),
                operation: "insert_node".to_string(),
            })?;
        Ok(())
    }

    /// Inserts all nodes, edges, and file records in a single `execute_batch` call.
    /// This minimizes transaction overhead by combining everything into one SQL string.
    ///
    /// `Contains` edges are denormalized at insert time: their `(source, target)`
    /// pair is folded into the target node's `parent_id` column, and the edge
    /// itself is not persisted. Extractors keep emitting `Contains` edges as
    /// before; the conversion happens here, in one place.
    pub async fn insert_all(
        &self,
        nodes: &[Node],
        edges: &[Edge],
        files: &[FileRecord],
    ) -> Result<()> {
        // Pull every Contains edge out: build target_id -> parent_id map, then
        // filter the surviving edges list. When a node has multiple incoming
        // Contains rows (extractor anomaly), the first one wins — matching
        // the migration's `LIMIT 1` backfill behavior.
        let mut parent_map: std::collections::HashMap<&str, &str> =
            std::collections::HashMap::new();
        let mut surviving_edges: Vec<&Edge> = Vec::with_capacity(edges.len());
        for edge in edges {
            if edge.kind == crate::types::EdgeKind::Contains {
                parent_map
                    .entry(edge.target.as_str())
                    .or_insert(edge.source.as_str());
            } else {
                surviving_edges.push(edge);
            }
        }
        // Apply the hoisted parents to the node slice without cloning every
        // node: we materialize only when parent_map has something to say.
        let nodes_owned: Vec<Node>;
        let nodes_ref: &[Node] = if parent_map.is_empty() {
            nodes
        } else {
            nodes_owned = nodes
                .iter()
                .map(|n| {
                    if let Some(parent) = parent_map.get(n.id.as_str()) {
                        let mut copy = n.clone();
                        copy.parent_id = Some((*parent).to_string());
                        copy
                    } else {
                        n.clone()
                    }
                })
                .collect();
            &nodes_owned
        };

        let mut sql = String::with_capacity(
            nodes_ref.len() * 400 + surviving_edges.len() * 120 + files.len() * 120,
        );
        // Nodes
        for chunk in nodes_ref.chunks(200) {
            sql.push_str(
                "INSERT OR REPLACE INTO nodes \
                 (id,kind,name,qualified_name,file_path,\
                 start_line,end_line,start_column,end_column,\
                 docstring,signature,visibility,is_async,\
                 branches,loops,returns,max_nesting,\
                 unsafe_blocks,unchecked_calls,assertions,updated_at,attrs_start_line,parent_id) VALUES ",
            );
            for (i, node) in chunk.iter().enumerate() {
                if i > 0 {
                    sql.push(',');
                }
                sql.push('(');
                push_quoted(&mut sql, &node.id);
                sql.push(',');
                push_quoted(&mut sql, node.kind.as_str());
                sql.push(',');
                push_quoted(&mut sql, &node.name);
                sql.push(',');
                push_quoted(&mut sql, &node.qualified_name);
                sql.push(',');
                push_quoted(&mut sql, &node.file_path);
                sql.push(',');
                push_int(&mut sql, i64::from(node.start_line));
                sql.push(',');
                push_int(&mut sql, i64::from(node.end_line));
                sql.push(',');
                push_int(&mut sql, i64::from(node.start_column));
                sql.push(',');
                push_int(&mut sql, i64::from(node.end_column));
                sql.push(',');
                push_opt_quoted(&mut sql, node.docstring.as_deref());
                sql.push(',');
                push_opt_quoted(&mut sql, node.signature.as_deref());
                sql.push(',');
                push_quoted(&mut sql, node.visibility.as_str());
                sql.push(',');
                push_int(&mut sql, i64::from(node.is_async));
                sql.push(',');
                push_int(&mut sql, i64::from(node.branches));
                sql.push(',');
                push_int(&mut sql, i64::from(node.loops));
                sql.push(',');
                push_int(&mut sql, i64::from(node.returns));
                sql.push(',');
                push_int(&mut sql, i64::from(node.max_nesting));
                sql.push(',');
                push_int(&mut sql, i64::from(node.unsafe_blocks));
                sql.push(',');
                push_int(&mut sql, i64::from(node.unchecked_calls));
                sql.push(',');
                push_int(&mut sql, i64::from(node.assertions));
                sql.push(',');
                push_int(&mut sql, node.updated_at as i64);
                sql.push(',');
                push_int(&mut sql, i64::from(node.attrs_start_line));
                sql.push(',');
                push_opt_quoted(&mut sql, node.parent_id.as_deref());
                sql.push(')');
            }
            sql.push_str(";\n");
        }

        // Edges (Contains has already been hoisted out into parent_id)
        for chunk in surviving_edges.chunks(500) {
            sql.push_str("INSERT OR IGNORE INTO edges (source,target,kind,line) VALUES ");
            for (i, edge) in chunk.iter().enumerate() {
                if i > 0 {
                    sql.push(',');
                }
                sql.push('(');
                push_quoted(&mut sql, &edge.source);
                sql.push(',');
                push_quoted(&mut sql, &edge.target);
                sql.push(',');
                push_quoted(&mut sql, edge.kind.as_str());
                sql.push(',');
                match edge.line {
                    Some(l) => push_int(&mut sql, i64::from(l)),
                    None => sql.push_str("NULL"),
                }
                sql.push(')');
            }
            sql.push_str(";\n");
        }

        // Files
        for chunk in files.chunks(500) {
            sql.push_str(
                "INSERT OR REPLACE INTO files \
                 (path,content_hash,size,modified_at,indexed_at,node_count) VALUES ",
            );
            for (i, file) in chunk.iter().enumerate() {
                if i > 0 {
                    sql.push(',');
                }
                sql.push('(');
                push_quoted(&mut sql, &file.path);
                sql.push(',');
                push_quoted(&mut sql, &file.content_hash);
                sql.push(',');
                push_int(&mut sql, file.size as i64);
                sql.push(',');
                push_int(&mut sql, file.modified_at);
                sql.push(',');
                push_int(&mut sql, file.indexed_at);
                sql.push(',');
                push_int(&mut sql, i64::from(file.node_count));
                sql.push(')');
            }
            sql.push_str(";\n");
        }

        let transaction = self.begin_write_transaction("insert_all").await?;
        self.insert_all_sql_unguarded(&transaction, &sql).await?;
        transaction.commit().await
    }

    async fn insert_all_sql_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
        sql: &str,
    ) -> Result<()> {
        transaction
            .execute_batch_engine(sql)
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to bulk insert: {e}"),
                operation: "insert_all".to_string(),
            })?;
        Ok(())
    }

    /// Inserts nodes using a prepared statement: parse SQL once, then
    /// bind+execute+reset for each row — zero SQL parsing after the first call.
    pub async fn insert_nodes(&self, nodes: &[Node]) -> Result<()> {
        if nodes.is_empty() {
            return Ok(());
        }

        let transaction = self.begin_write_transaction("insert_nodes").await?;
        self.insert_nodes_unguarded(&transaction, nodes).await?;
        transaction.commit().await
    }

    pub async fn insert_nodes_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
        nodes: &[Node],
    ) -> Result<()> {
        if nodes.is_empty() {
            return Ok(());
        }

        // Keep each statement below SQLite's conservative 999-parameter
        // floor: 32 rows × 23 columns = 736 parameters. This avoids one async
        // runtime request per node while preserving the surrounding atomic
        // full-index replacement.
        const ROWS_PER_INSERT: usize = 32;
        const COLUMNS: usize = NODE_COLUMNS as usize;
        for chunk in nodes.chunks(ROWS_PER_INSERT) {
            let values_clause = (0..chunk.len())
                .map(|row| {
                    let first = row * COLUMNS + 1;
                    let placeholders = (first..first + COLUMNS)
                        .map(|index| format!("?{index}"))
                        .collect::<Vec<_>>()
                        .join(",");
                    format!("({placeholders})")
                })
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "INSERT OR REPLACE INTO nodes \
                 (id,kind,name,qualified_name,file_path,\
                  start_line,end_line,start_column,end_column,\
                  docstring,signature,visibility,is_async,\
                  branches,loops,returns,max_nesting,\
                  unsafe_blocks,unchecked_calls,assertions,updated_at,attrs_start_line,parent_id) \
                 VALUES {values_clause}"
            );
            let mut values = Vec::with_capacity(chunk.len() * COLUMNS);
            for node in chunk {
                values.extend([
                    Value::Text(node.id.clone()),
                    Value::Text(node.kind.as_str().to_owned()),
                    Value::Text(node.name.clone()),
                    Value::Text(node.qualified_name.clone()),
                    Value::Text(node.file_path.clone()),
                    Value::Integer(i64::from(node.start_line)),
                    Value::Integer(i64::from(node.end_line)),
                    Value::Integer(i64::from(node.start_column)),
                    Value::Integer(i64::from(node.end_column)),
                    node.docstring.clone().map_or(Value::Null, Value::Text),
                    node.signature.clone().map_or(Value::Null, Value::Text),
                    Value::Text(node.visibility.as_str().to_owned()),
                    Value::Integer(i64::from(node.is_async)),
                    Value::Integer(i64::from(node.branches)),
                    Value::Integer(i64::from(node.loops)),
                    Value::Integer(i64::from(node.returns)),
                    Value::Integer(i64::from(node.max_nesting)),
                    Value::Integer(i64::from(node.unsafe_blocks)),
                    Value::Integer(i64::from(node.unchecked_calls)),
                    Value::Integer(i64::from(node.assertions)),
                    Value::Integer(node.updated_at as i64),
                    Value::Integer(i64::from(node.attrs_start_line)),
                    node.parent_id.clone().map_or(Value::Null, Value::Text),
                ]);
            }
            transaction
                .execute_engine(&sql, values)
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to insert node: {e}"),
                    operation: "insert_nodes".to_string(),
                })?;
        }
        Ok(())
    }

    /// Retrieves a node by its unique ID, returning `None` if not found.
    pub async fn get_node_by_id(&self, id: &str) -> Result<Option<Node>> {
        let mut rows = self
            .engine_conn()
            .query(
                concat!(
                    "SELECT ",
                    node_select_columns!(),
                    " FROM nodes WHERE id = ?1"
                ),
                params![id],
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query node by id: {e}"),
                operation: "get_node_by_id".to_string(),
            })?;

        match rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read node row: {e}"),
            operation: "get_node_by_id".to_string(),
        })? {
            Some(row) => {
                let node = row_to_node(&row).map_err(|e| TraceDecayError::Database {
                    message: format!("failed to map node row: {e}"),
                    operation: "get_node_by_id".to_string(),
                })?;
                Ok(Some(node))
            }
            None => Ok(None),
        }
    }

    /// Returns nodes by their IDs in a single batch query.
    /// IDs not found are silently omitted. Results are returned in arbitrary order.
    pub async fn get_nodes_by_ids(&self, ids: &[String]) -> Result<Vec<Node>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        // Build `?, ?, ?, …` in one allocation instead of `Vec<String>` of
        // `?1`/`?2`/`?N`. SQLite binds anonymous `?` parameters in order, so
        // dropping the numbered form changes nothing for the driver. Large
        // BFS frontiers (`traverse_bfs` calls this once per level) hit this
        // path often enough that the per-id `format!` allocations showed up
        // on profiles.
        let placeholders = build_qmark_placeholders(ids.len());
        let sql = format!("SELECT {NODE_SELECT_COLUMNS} FROM nodes WHERE id IN ({placeholders})");
        let param_values: Vec<Value> = ids.iter().map(|id| Value::Text(id.clone())).collect();
        let mut rows = self
            .engine_conn()
            .query(&sql, params_from_iter(param_values))
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to batch query nodes: {e}"),
                operation: "get_nodes_by_ids".to_string(),
            })?;
        collect_rows(&mut rows, row_to_node, "get_nodes_by_ids").await
    }

    /// Returns all nodes for a given file, ordered by start line.
    ///
    /// Read through `rowid` keyset pages. One file is not a bound: a generated
    /// or vendored source file can declare more symbols than the `SQLite`
    /// runtime will materialize for a single query, and the runtime refuses an
    /// oversized query outright rather than truncating it. The pages arrive in
    /// `rowid` order, so the `start_line` ordering is restored here.
    pub async fn get_nodes_by_file(&self, file_path: &str) -> Result<Vec<Node>> {
        let mut nodes = collect_rowid_pages_with(
            &self.engine_conn(),
            NODES_BY_FILE_PAGE_SQL,
            &[Value::Text(file_path.to_string())],
            NODE_COLUMNS,
            row_to_node,
            "get_nodes_by_file",
        )
        .await?;
        nodes.sort_by_key(|node| node.start_line);
        Ok(nodes)
    }

    /// Returns every node whose `parent_id` matches `parent_id`. Replaces
    /// the v8 pattern of querying outgoing `Contains` edges; after v9 the
    /// edges table no longer carries that information.
    pub async fn get_children_of(&self, parent_id: &str) -> Result<Vec<Node>> {
        let mut rows = self
            .engine_conn()
            .query(
                concat!(
                    "SELECT ",
                    node_select_columns!(),
                    " FROM nodes WHERE parent_id = ?1 ORDER BY start_line"
                ),
                params![parent_id],
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query children: {e}"),
                operation: "get_children_of".to_string(),
            })?;

        collect_rows(&mut rows, row_to_node, "get_children_of").await
    }

    /// Returns every node whose `parent_id` matches any of `parent_ids`, in
    /// one round-trip. Mirrors `get_children_of` but batched: callers that
    /// would otherwise loop `get_children_of` once per parent (e.g.
    /// `TraceDecay::get_trait_dispatch_targets` walking impl blocks) can
    /// batch them into a single query.
    ///
    /// Results are grouped by `parent_id` implicitly via the `parent_id`
    /// column in the returned rows (callers can bucket by it); within each
    /// parent, ordering follows `start_line` as in `get_children_of`.
    pub async fn get_children_of_bulk(&self, parent_ids: &[String]) -> Result<Vec<Node>> {
        if parent_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = build_qmark_placeholders(parent_ids.len());
        let sql = format!(
            concat!(
                "SELECT ",
                node_select_columns!(),
                " FROM nodes WHERE parent_id IN ({}) ORDER BY parent_id, start_line"
            ),
            placeholders
        );
        let param_values: Vec<Value> = parent_ids
            .iter()
            .map(|id| Value::Text(id.clone()))
            .collect();
        let mut rows = self
            .engine_conn()
            .query(&sql, params_from_iter(param_values))
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to batch query children: {e}"),
                operation: "get_children_of_bulk".to_string(),
            })?;
        collect_rows(&mut rows, row_to_node, "get_children_of_bulk").await
    }

    /// Returns the distinct file paths that hold at least one node of `kind`,
    /// in path order, starting after `after_path`.
    ///
    /// Whole-repository walks over one node kind (unused imports, for example)
    /// must not read the entire `nodes` table to find their candidate files.
    /// Path-ordered keyset paging also gives those walks a stable continuation
    /// cursor across calls.
    pub async fn file_paths_with_nodes_of_kind(
        &self,
        kind: NodeKind,
        after_path: Option<&str>,
        limit: usize,
    ) -> Result<Vec<String>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut rows = self
            .engine_conn()
            .query(
                "SELECT DISTINCT file_path
                 FROM nodes
                 WHERE kind = ?1 AND (?2 IS NULL OR file_path > ?2)
                 ORDER BY file_path
                 LIMIT ?3",
                params![kind.as_str(), opt_str(after_path), limit],
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query files by node kind: {e}"),
                operation: "file_paths_with_nodes_of_kind".to_string(),
            })?;
        collect_rows(
            &mut rows,
            |row| row.get::<String>(0),
            "file_paths_with_nodes_of_kind",
        )
        .await
    }

    /// Returns all nodes of a given kind.
    ///
    /// Read through `rowid` keyset pages. One node kind is a partition, not a
    /// bound: a real repository holds far more functions than the `SQLite`
    /// runtime will materialize for one query, and the runtime refuses an
    /// oversized query outright rather than truncating it.
    pub async fn get_nodes_by_kind(&self, kind: NodeKind) -> Result<Vec<Node>> {
        collect_rowid_pages_with(
            &self.engine_conn(),
            NODES_BY_KIND_PAGE_SQL,
            &[Value::Text(kind.as_str().to_string())],
            NODE_COLUMNS,
            row_to_node,
            "get_nodes_by_kind",
        )
        .await
    }

    /// Returns every node in the database.
    ///
    /// Read through `rowid` keyset pages: whole-table reads on a real project
    /// exceed what the `SQLite` runtime will materialize for one query.
    pub async fn get_all_nodes(&self) -> Result<Vec<Node>> {
        collect_rowid_pages(
            &self.engine_conn(),
            concat!(
                "SELECT ",
                node_select_columns!(),
                ", rowid FROM nodes WHERE rowid > ?1 ORDER BY rowid LIMIT ?2"
            ),
            NODE_COLUMNS,
            row_to_node,
            "get_all_nodes",
        )
        .await
    }

    /// Deletes all nodes (and cascading edges, unresolved refs, vectors) for a file.
    pub async fn delete_nodes_by_file(&self, file_path: &str) -> Result<()> {
        let transaction = self.begin_write_transaction("delete_nodes_by_file").await?;
        self.delete_nodes_by_file_unguarded(&transaction, file_path)
            .await?;
        transaction.commit().await
    }

    pub async fn delete_nodes_by_file_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
        file_path: &str,
    ) -> Result<()> {
        Self::delete_nodes_by_file_in_transaction(transaction, file_path).await?;
        Ok(())
    }

    pub(super) async fn delete_nodes_by_file_in_transaction(
        transaction: &DatabaseWriteTransaction<'_>,
        file_path: &str,
    ) -> Result<()> {
        debug_assert!(
            !file_path.is_empty(),
            "delete_nodes_by_file called with empty file_path"
        );
        debug_assert!(
            !file_path.starts_with('/'),
            "delete_nodes_by_file expects relative path, got absolute"
        );
        // Gather node IDs for the file first, through `rowid` keyset pages —
        // one file's symbol count is not a bound the runtime honours. See
        // [`Database::get_nodes_by_file`].
        let node_ids: Vec<String> = collect_rowid_pages_with(
            transaction,
            NODE_IDS_BY_FILE_PAGE_SQL,
            &[Value::Text(file_path.to_string())],
            1,
            |row| row.get::<String>(0),
            "delete_nodes_by_file",
        )
        .await?;

        if node_ids.is_empty() {
            return Ok(());
        }

        for id in &node_ids {
            transaction
                .execute_engine(
                    "DELETE FROM edges WHERE source = ?1 OR target = ?1",
                    params![id.as_str()],
                )
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to delete edges: {e}"),
                    operation: "delete_nodes_by_file".to_string(),
                })?;

            transaction
                .execute_engine(
                    "DELETE FROM unresolved_refs WHERE from_node_id = ?1",
                    params![id.as_str()],
                )
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to delete unresolved refs: {e}"),
                    operation: "delete_nodes_by_file".to_string(),
                })?;

            transaction
                .execute_engine(
                    "DELETE FROM vectors WHERE node_id = ?1",
                    params![id.as_str()],
                )
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to delete vectors: {e}"),
                    operation: "delete_nodes_by_file".to_string(),
                })?;
        }

        transaction
            .execute_engine("DELETE FROM nodes WHERE file_path = ?1", params![file_path])
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to delete nodes: {e}"),
                operation: "delete_nodes_by_file".to_string(),
            })?;
        Ok(())
    }
}

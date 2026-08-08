// Rust guideline compliant 2025-10-17
use std::collections::HashSet;

use crate::db::engine::{Value, params, params_from_iter};

use super::connection::{Database, DatabaseEngineReadSnapshot, DatabaseWriteTransaction};
use super::rows::{NODE_COLUMNS, node_select_columns, row_to_node};
use super::sql::{
    build_qmark_placeholders, collect_rowid_pages, collect_rowid_pages_with,
    collect_rowid_pages_with_controlled, collect_rows, opt_str, push_int, push_opt_quoted,
    push_quoted,
};
use crate::errors::{Result, TraceDecayError};
use crate::types::*;

const CONTROLLED_NODE_PAGE_CHECKPOINT_ROWS: usize = 64;

/// One canonical node row's binding to the symbol occurrence that a published
/// code-index generation minted for it.
///
/// Wire identity stays `nodes.id`; the occurrence is the internal join key
/// only, so nothing a client holds changes when a generation is republished
/// (ruling A1').
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolOccurrenceBinding {
    pub node_id: String,
    pub symbol_occurrence_id: String,
}

/// What a rebind actually wrote, against what it was asked to write.
///
/// `bound < requested` means the published generation named symbols the
/// relational index does not carry. The count is reported rather than
/// swallowed so the publish path can refuse instead of leaving a partially
/// bridged generation that reads cannot tell apart from a complete one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SymbolOccurrenceBindOutcome {
    pub requested: usize,
    pub bound: usize,
}

impl SymbolOccurrenceBindOutcome {
    /// Every requested binding reached a canonical node row.
    pub fn is_complete(&self) -> bool {
        self.bound == self.requested
    }
}

/// Drops every stored binding, so a rebind cannot leave a row carrying an
/// occurrence minted against a generation that is no longer published.
pub(super) const CLEAR_SYMBOL_OCCURRENCE_BINDINGS_SQL: &str =
    "UPDATE nodes SET symbol_occurrence_id = NULL WHERE symbol_occurrence_id IS NOT NULL";

/// Rows bound per statement. Three bound parameters per row (the `CASE` key,
/// the `CASE` value, and the `IN` key) keeps a full chunk at 768 parameters,
/// below `SQLite`'s conservative 999-parameter floor.
pub(super) const SYMBOL_OCCURRENCE_ROWS_PER_BIND: usize = 256;

/// Occurrences resolved per statement, against the same parameter floor.
pub(super) const SYMBOL_OCCURRENCES_PER_READ: usize = 512;

/// Rejects a binding set that a single `CASE` statement could only apply by
/// silently picking a winner: an empty identity, one node claimed by two
/// occurrences, or one occurrence claimed by two nodes.
pub(super) fn validate_symbol_occurrence_bindings(
    bindings: &[SymbolOccurrenceBinding],
) -> Result<()> {
    let mut seen_nodes = HashSet::with_capacity(bindings.len());
    let mut seen_occurrences = HashSet::with_capacity(bindings.len());
    for binding in bindings {
        if binding.node_id.is_empty() || binding.symbol_occurrence_id.is_empty() {
            return Err(TraceDecayError::Database {
                message: "symbol occurrence binding carries an empty identity".to_owned(),
                operation: "bind_symbol_occurrence_ids".to_owned(),
            });
        }
        if !seen_nodes.insert(binding.node_id.as_str()) {
            return Err(TraceDecayError::Database {
                message: format!(
                    "node {} is claimed by more than one symbol occurrence binding",
                    binding.node_id
                ),
                operation: "bind_symbol_occurrence_ids".to_owned(),
            });
        }
        if !seen_occurrences.insert(binding.symbol_occurrence_id.as_str()) {
            return Err(TraceDecayError::Database {
                message: format!(
                    "symbol occurrence {} is claimed by more than one node",
                    binding.symbol_occurrence_id
                ),
                operation: "bind_symbol_occurrence_ids".to_owned(),
            });
        }
    }
    Ok(())
}

/// The rebind statement for one chunk of `rows` bindings.
pub(super) fn symbol_occurrence_bind_sql(rows: usize) -> String {
    let cases = (0..rows)
        .map(|_| "WHEN ? THEN ?")
        .collect::<Vec<_>>()
        .join(" ");
    let placeholders = build_qmark_placeholders(rows);
    format!(
        "UPDATE nodes SET symbol_occurrence_id = CASE id {cases} END WHERE id IN ({placeholders})"
    )
}

/// Parameters for [`symbol_occurrence_bind_sql`], in statement order: the
/// `CASE` key/value pairs first, then the `IN` keys.
pub(super) fn symbol_occurrence_bind_params(chunk: &[SymbolOccurrenceBinding]) -> Vec<Value> {
    let mut values = Vec::with_capacity(chunk.len().saturating_mul(3));
    for binding in chunk {
        values.push(Value::Text(binding.node_id.clone()));
        values.push(Value::Text(binding.symbol_occurrence_id.clone()));
    }
    for binding in chunk {
        values.push(Value::Text(binding.node_id.clone()));
    }
    values
}

/// The occurrence-keyed resolution statement for `occurrences` bound ids.
pub(super) fn node_ids_by_symbol_occurrence_sql(occurrences: usize) -> String {
    let placeholders = build_qmark_placeholders(occurrences);
    format!(
        "SELECT symbol_occurrence_id, id FROM nodes WHERE symbol_occurrence_id IN ({placeholders})"
    )
}

/// One `rowid` keyset page of the nodes declared by a single file.
pub(super) const NODES_BY_FILE_PAGE_SQL: &str = concat!(
    "SELECT ",
    node_select_columns!(),
    ", rowid FROM nodes WHERE file_path = ?1 AND rowid > ?2 ORDER BY rowid LIMIT ?3"
);

/// One stable, bounded symbol page across a set of files.
///
/// The keyset predicate is applied before `LIMIT`, so every continuation
/// reads at most the caller's page budget plus one lookahead row.
pub(super) const NODES_BY_FILES_SYMBOL_PAGE_SQL: &str = concat!(
    "SELECT ",
    node_select_columns!(),
    ", n.rowid \
     FROM nodes AS n INDEXED BY idx_nodes_file_path_start_line \
     WHERE n.file_path IN (SELECT value FROM json_each(?1)) \
       AND (?2 IS NULL \
            OR n.file_path > ?2 \
            OR (n.file_path = ?2 AND n.start_line > ?3) \
            OR (n.file_path = ?2 AND n.start_line = ?3 AND n.rowid > ?4)) \
     ORDER BY n.file_path, n.start_line, n.rowid \
     LIMIT ?5"
);

/// One page of nodes selected by an arbitrary-size JSON-array id bind.
pub(super) const NODES_BY_IDS_PAGE_SQL: &str = concat!(
    "SELECT ",
    node_select_columns!(),
    ", rowid FROM nodes \
     WHERE id IN (SELECT value FROM json_each(?1)) \
       AND rowid > ?2 ORDER BY rowid LIMIT ?3"
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

const ALL_NODES_PAGE_SQL: &str = concat!(
    "SELECT ",
    node_select_columns!(),
    ", rowid FROM nodes WHERE rowid > ?1 ORDER BY rowid LIMIT ?2"
);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodesByFilesPageKey {
    pub file_path: String,
    pub start_line: u32,
    pub rowid: i64,
}

#[derive(Clone, Debug)]
pub struct NodesByFilesPageEntry {
    pub node: Option<Node>,
    pub file_path: String,
    pub start_line: u32,
    pub rowid: i64,
    pub is_config_summary: bool,
}

#[derive(Clone, Debug)]
pub struct NodesByFilesPage {
    pub entries: Vec<NodesByFilesPageEntry>,
    pub has_more: bool,
    pub rows_read: usize,
}

pub(super) async fn read_nodes_by_files_page_controlled<C, F>(
    conn: &C,
    file_paths: &[String],
    config_paths: &[String],
    after: Option<&NodesByFilesPageKey>,
    limit: usize,
    mut checkpoint: F,
) -> Result<NodesByFilesPage>
where
    C: crate::db::engine::QueryExecutor + ?Sized,
    F: FnMut() -> Result<()>,
{
    if file_paths.is_empty() {
        return Ok(NodesByFilesPage {
            entries: Vec::new(),
            has_more: false,
            rows_read: 0,
        });
    }
    let encode = |values: &[String], field: &'static str| {
        serde_json::to_string(values).map_err(|error| TraceDecayError::Database {
            message: format!("failed to encode {field}: {error}"),
            operation: "get_nodes_by_files_page".to_owned(),
        })
    };
    let admitted_limit = limit;
    let query_limit = limit
        .checked_add(1)
        .ok_or_else(|| TraceDecayError::Database {
            message: "node page limit overflowed".to_owned(),
            operation: "get_nodes_by_files_page".to_owned(),
        })?;
    let query_limit_i64 = i64::try_from(query_limit)
        .ok()
        .filter(|limit| *limit > 0)
        .ok_or_else(|| TraceDecayError::Database {
            message: "node page limit must be positive".to_owned(),
            operation: "get_nodes_by_files_page".to_owned(),
        })?;
    let config_path_set: HashSet<&str> = config_paths.iter().map(String::as_str).collect();
    let source_paths: Vec<String> = file_paths
        .iter()
        .filter(|path| !config_path_set.contains(path.as_str()))
        .cloned()
        .collect();
    let is_after = |file_path: &str, start_line: u32, rowid: i64| {
        after.is_none_or(|key| {
            (file_path, start_line, rowid) > (key.file_path.as_str(), key.start_line, key.rowid)
        })
    };
    let mut sorted_config_paths = config_paths.to_vec();
    sorted_config_paths.sort();
    sorted_config_paths.dedup();
    let mut entries: Vec<NodesByFilesPageEntry> = sorted_config_paths
        .into_iter()
        .filter(|path| is_after(path, 0, 0))
        .take(query_limit)
        .map(|path| NodesByFilesPageEntry {
            node: None,
            file_path: path,
            start_line: 0,
            rowid: 0,
            is_config_summary: true,
        })
        .collect();
    if source_paths.is_empty() {
        let rows_read = entries.len();
        let has_more = rows_read > admitted_limit;
        entries.truncate(admitted_limit);
        return Ok(NodesByFilesPage {
            entries,
            has_more,
            rows_read,
        });
    }
    let (after_path, after_line, after_rowid) =
        after.map_or((Value::Null, Value::Null, Value::Null), |key| {
            (
                Value::Text(key.file_path.clone()),
                Value::Integer(i64::from(key.start_line)),
                Value::Integer(key.rowid),
            )
        });
    checkpoint()?;
    let mut rows = conn
        .query(
            NODES_BY_FILES_SYMBOL_PAGE_SQL,
            params_from_iter([
                Value::Text(encode(&source_paths, "file paths")?),
                after_path,
                after_line,
                after_rowid,
                Value::Integer(query_limit_i64),
            ]),
        )
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to query bounded nodes by files: {error}"),
            operation: "get_nodes_by_files_page".to_owned(),
        })?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| TraceDecayError::Database {
            message: format!("failed to read bounded node page: {error}"),
            operation: "get_nodes_by_files_page".to_owned(),
        })?
    {
        if !entries.is_empty()
            && entries
                .len()
                .is_multiple_of(CONTROLLED_NODE_PAGE_CHECKPOINT_ROWS)
        {
            checkpoint()?;
        }
        let node = row_to_node(&row).map_err(|error| TraceDecayError::Database {
            message: format!("failed to map bounded node page: {error}"),
            operation: "get_nodes_by_files_page".to_owned(),
        })?;
        let rowid = row
            .get::<i64>(23)
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to read bounded node cursor: {error}"),
                operation: "get_nodes_by_files_page".to_owned(),
            })?;
        entries.push(NodesByFilesPageEntry {
            file_path: node.file_path.clone(),
            start_line: node.start_line,
            node: Some(node),
            rowid,
            is_config_summary: false,
        });
    }
    checkpoint()?;
    entries.sort_by(|left, right| {
        (&left.file_path, left.start_line, left.rowid).cmp(&(
            &right.file_path,
            right.start_line,
            right.rowid,
        ))
    });
    entries.truncate(query_limit);
    let rows_read = entries.len();
    let has_more = rows_read > admitted_limit;
    entries.truncate(admitted_limit);
    Ok(NodesByFilesPage {
        entries,
        has_more,
        rows_read,
    })
}

async fn read_nodes_by_ids_controlled<C, F>(
    conn: &C,
    ids: &[String],
    checkpoint: F,
) -> Result<Vec<Node>>
where
    C: crate::db::engine::QueryExecutor + ?Sized,
    F: FnMut() -> Result<()>,
{
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let encoded = serde_json::to_string(ids).map_err(|error| TraceDecayError::Database {
        message: format!("failed to encode node ids: {error}"),
        operation: "get_nodes_by_ids".to_string(),
    })?;
    collect_rowid_pages_with_controlled(
        conn,
        NODES_BY_IDS_PAGE_SQL,
        &[Value::Text(encoded)],
        NODE_COLUMNS,
        row_to_node,
        "get_nodes_by_ids",
        checkpoint,
    )
    .await
}

impl DatabaseEngineReadSnapshot {
    pub async fn get_nodes_by_files_page_controlled<F>(
        &self,
        file_paths: &[String],
        config_paths: &[String],
        after: Option<&NodesByFilesPageKey>,
        limit: usize,
        checkpoint: F,
    ) -> Result<NodesByFilesPage>
    where
        F: FnMut() -> Result<()>,
    {
        read_nodes_by_files_page_controlled(
            self,
            file_paths,
            config_paths,
            after,
            limit,
            checkpoint,
        )
        .await
    }

    pub async fn get_nodes_by_ids_controlled<F>(
        &self,
        ids: &[String],
        checkpoint: F,
    ) -> Result<Vec<Node>>
    where
        F: FnMut() -> Result<()>,
    {
        read_nodes_by_ids_controlled(self, ids, checkpoint).await
    }
}

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
    ///
    /// `symbol_occurrence_id` is deliberately absent from the column list. It
    /// is a per-generation binding minted by the published code index, not an
    /// extraction fact, so `INSERT OR REPLACE` clearing it is the intended
    /// behavior: a re-indexed node must not keep an occurrence minted against
    /// a generation it no longer belongs to. See
    /// [`Self::bind_symbol_occurrence_ids`].
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

    /// Replaces every `nodes.symbol_occurrence_id` binding with the bindings a
    /// single published code-index generation minted.
    ///
    /// The occurrence digest takes the generation id as an input, so a binding
    /// is meaningful with exactly one generation. This write therefore clears
    /// the whole column before applying `bindings`: a row the new generation
    /// does not mention ends up NULL rather than keeping an occurrence from a
    /// generation that is no longer published. Reads must treat NULL as a
    /// typed staleness refusal, never as "no such symbol".
    ///
    /// Returns [`SymbolOccurrenceBindOutcome`] so the caller can compare what
    /// it asked for against what the relational index could actually carry; a
    /// shortfall means the two pipelines disagree about which nodes exist and
    /// is the caller's to refuse, not this writer's to hide.
    pub async fn bind_symbol_occurrence_ids(
        &self,
        bindings: &[SymbolOccurrenceBinding],
    ) -> Result<SymbolOccurrenceBindOutcome> {
        let transaction = self
            .begin_write_transaction("bind_symbol_occurrence_ids")
            .await?;
        let outcome = self
            .bind_symbol_occurrence_ids_unguarded(&transaction, bindings)
            .await?;
        transaction.commit().await?;
        Ok(outcome)
    }

    /// [`Self::bind_symbol_occurrence_ids`] inside a caller-owned transaction,
    /// so a publish path can rebind in the same unit of work that stamps the
    /// generation watermark.
    pub async fn bind_symbol_occurrence_ids_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
        bindings: &[SymbolOccurrenceBinding],
    ) -> Result<SymbolOccurrenceBindOutcome> {
        validate_symbol_occurrence_bindings(bindings)?;

        transaction
            .execute_engine(CLEAR_SYMBOL_OCCURRENCE_BINDINGS_SQL, ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to clear prior symbol occurrence bindings: {e}"),
                operation: "bind_symbol_occurrence_ids".to_owned(),
            })?;

        let mut bound = 0_usize;
        for chunk in bindings.chunks(SYMBOL_OCCURRENCE_ROWS_PER_BIND) {
            let sql = symbol_occurrence_bind_sql(chunk.len());
            let affected = transaction
                .execute_engine(&sql, params_from_iter(symbol_occurrence_bind_params(chunk)))
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to bind symbol occurrence ids: {e}"),
                    operation: "bind_symbol_occurrence_ids".to_owned(),
                })?;
            bound = bound.saturating_add(usize::try_from(affected).unwrap_or(usize::MAX));
        }

        Ok(SymbolOccurrenceBindOutcome {
            requested: bindings.len(),
            bound,
        })
    }

    /// Resolves published symbol occurrences to the canonical node ids bound to
    /// them, for the generation whose bindings are currently stored.
    ///
    /// An occurrence absent from the returned map has no binding in the stored
    /// generation. That is never a silent "not found": the caller holds the
    /// published-generation watermark and must answer a typed staleness or
    /// corruption refusal, per ruling A1'.
    pub async fn node_ids_by_symbol_occurrence_ids(
        &self,
        occurrences: &[String],
    ) -> Result<std::collections::BTreeMap<String, String>> {
        let mut resolved = std::collections::BTreeMap::new();
        if occurrences.is_empty() {
            return Ok(resolved);
        }
        for chunk in occurrences.chunks(SYMBOL_OCCURRENCES_PER_READ) {
            let sql = node_ids_by_symbol_occurrence_sql(chunk.len());
            let values: Vec<Value> = chunk.iter().cloned().map(Value::Text).collect();
            let mut rows = self
                .engine_conn()
                .query(&sql, params_from_iter(values))
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to resolve symbol occurrence bindings: {e}"),
                    operation: "node_ids_by_symbol_occurrence_ids".to_owned(),
                })?;
            let page = collect_rows(
                &mut rows,
                |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?)),
                "node_ids_by_symbol_occurrence_ids",
            )
            .await?;
            resolved.extend(page);
        }
        Ok(resolved)
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
        self.get_nodes_by_ids_controlled(ids, || Ok(())).await
    }

    /// [`Database::get_nodes_by_ids`] with cooperative page checkpoints.
    pub async fn get_nodes_by_ids_controlled<F>(
        &self,
        ids: &[String],
        checkpoint: F,
    ) -> Result<Vec<Node>>
    where
        F: FnMut() -> Result<()>,
    {
        read_nodes_by_ids_controlled(&self.engine_conn(), ids, checkpoint).await
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

    /// Returns one stable, bounded symbol page across `file_paths`.
    pub async fn get_nodes_by_files_page_controlled<F>(
        &self,
        file_paths: &[String],
        config_paths: &[String],
        after: Option<&NodesByFilesPageKey>,
        limit: usize,
        checkpoint: F,
    ) -> Result<NodesByFilesPage>
    where
        F: FnMut() -> Result<()>,
    {
        read_nodes_by_files_page_controlled(
            &self.engine_conn(),
            file_paths,
            config_paths,
            after,
            limit,
            checkpoint,
        )
        .await
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
            ALL_NODES_PAGE_SQL,
            NODE_COLUMNS,
            row_to_node,
            "get_all_nodes",
        )
        .await
    }

    /// Returns every node visible inside an existing write transaction.
    pub async fn get_all_nodes_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
    ) -> Result<Vec<Node>> {
        collect_rowid_pages(
            transaction,
            ALL_NODES_PAGE_SQL,
            NODE_COLUMNS,
            row_to_node,
            "get_all_nodes_unguarded",
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

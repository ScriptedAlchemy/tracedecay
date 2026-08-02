// Rust guideline compliant 2025-10-17
use crate::db::engine::{Value, params, params_from_iter};

use super::connection::Database;
use super::rows::{NODE_COLUMNS, NODE_SELECT_COLUMNS, node_select_columns, row_to_node};
use super::sql::{
    collect_rowid_pages, collect_rowid_pages_with, collect_rows, path_prefix_like_value,
};
use crate::errors::{Result, TraceDecayError};
use crate::types::*;

/// One `rowid` keyset page of the whole `calls` partition.
pub(super) const CALL_EDGE_PAGE_SQL: &str = "SELECT source, target, rowid FROM edges
                 WHERE kind = 'calls' AND rowid > ?1 ORDER BY rowid LIMIT ?2";

/// [`CALL_EDGE_PAGE_SQL`] narrowed to one source-file prefix, bound as `?1`.
pub(super) const CALL_EDGE_PREFIXED_PAGE_SQL: &str =
    "SELECT e.source, e.target, e.rowid FROM edges e
                 JOIN nodes n ON e.source = n.id
                 WHERE e.kind = 'calls' AND n.file_path LIKE ?1
                   AND e.rowid > ?2 ORDER BY e.rowid LIMIT ?3";

/// [`CALL_EDGE_PAGE_SQL`] plus the call site's line.
pub(super) const CALL_EDGE_LINE_PAGE_SQL: &str = "SELECT source, target, line, rowid FROM edges
                 WHERE kind = 'calls' AND rowid > ?1 ORDER BY rowid LIMIT ?2";

/// [`CALL_EDGE_PREFIXED_PAGE_SQL`] plus the call site's line.
pub(super) const CALL_EDGE_LINE_PREFIXED_PAGE_SQL: &str =
    "SELECT e.source, e.target, e.line, e.rowid FROM edges e
                 JOIN nodes n ON e.source = n.id
                 WHERE e.kind = 'calls' AND n.file_path LIKE ?1
                   AND e.rowid > ?2 ORDER BY e.rowid LIMIT ?3";

/// Builds one `rowid` keyset page of the nodes under a path prefix (bound as
/// `?1`) whose kind is one of `kind_count` kinds (bound as `?2..`), followed by
/// the `rowid` cursor and the page row budget.
pub(super) fn nodes_by_dir_page_sql(kind_count: usize) -> String {
    let kind_placeholders: Vec<String> = (0..kind_count).map(|i| format!("?{}", i + 2)).collect();
    let cursor_param = kind_count + 2;
    format!(
        "SELECT {NODE_SELECT_COLUMNS}, rowid
             FROM nodes
             WHERE file_path LIKE ?1 || '%' AND kind IN ({})
               AND rowid > ?{cursor_param}
             ORDER BY rowid LIMIT ?{}",
        kind_placeholders.join(", "),
        cursor_param + 1
    )
}

/// Canonicalizes a caller-supplied qualified symbol name against stored graph
/// names without turning a qualified request into a bare-name fallback.
#[derive(Debug)]
struct CanonicalQualifiedName {
    normalized: String,
    without_crate_prefix: String,
    terminal_name: String,
}

impl CanonicalQualifiedName {
    fn new(value: &str) -> Self {
        let normalized = normalize_qualified_name(value);
        let without_crate_prefix = strip_crate_prefix(&normalized).to_string();
        let terminal_name = without_crate_prefix
            .rsplit("::")
            .next()
            .unwrap_or_default()
            .to_string();
        Self {
            normalized,
            without_crate_prefix,
            terminal_name,
        }
    }

    fn is_qualified(&self) -> bool {
        self.normalized.contains("::")
    }

    fn matches(&self, node: &Node) -> bool {
        let stored = normalize_qualified_name(&node.qualified_name);
        let stored_without_crate_prefix = strip_crate_prefix(&stored);
        if stored_without_crate_prefix == self.without_crate_prefix
            || qualified_suffix_matches(stored_without_crate_prefix, &self.without_crate_prefix)
        {
            return true;
        }

        rust_module_qualified_name(node)
            .is_some_and(|module_name| module_name == self.without_crate_prefix)
    }

    fn exactly_matches(&self, node: &Node) -> bool {
        normalize_qualified_name(&node.qualified_name) == self.normalized
    }
}

fn normalize_qualified_name(value: &str) -> String {
    let normalized = value.trim().replace('\\', "/");
    normalized
        .strip_prefix("./")
        .unwrap_or(&normalized)
        .to_string()
}

fn strip_crate_prefix(value: &str) -> &str {
    value.strip_prefix("crate::").unwrap_or(value)
}

fn qualified_suffix_matches(value: &str, suffix: &str) -> bool {
    !suffix.is_empty()
        && value
            .strip_suffix(suffix)
            .is_some_and(|prefix| prefix.is_empty() || prefix.ends_with("::"))
}

fn rust_module_qualified_name(node: &Node) -> Option<String> {
    let module = rust_module_path(&node.file_path)?;
    let file_path = normalize_qualified_name(&node.file_path);
    let stored = normalize_qualified_name(&node.qualified_name);
    let suffix = stored.strip_prefix(&file_path)?.strip_prefix("::")?;
    (!suffix.is_empty()).then(|| format!("{module}::{suffix}"))
}

fn rust_module_path(file_path: &str) -> Option<String> {
    let file_path = normalize_qualified_name(file_path);
    let source_relative = file_path
        .rsplit_once("/src/")
        .map(|(_, suffix)| suffix)
        .or_else(|| file_path.strip_prefix("src/"))?;
    let rust_relative = source_relative.strip_suffix(".rs")?;
    let mut segments: Vec<&str> = rust_relative
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();

    match segments.last().copied() {
        Some("mod") => {
            segments.pop();
        }
        Some("lib" | "main") if segments.len() == 1 => return None,
        _ => {}
    }

    (!segments.is_empty()).then(|| segments.join("::"))
}

impl Database {
    /// Returns all nodes whose `name` column matches the given bare identifier.
    ///
    /// Pure index lookup against `idx_nodes_name` — O(log n) with no BM25
    /// scoring, no fuzzy match, no fallback. Use this when you already know
    /// the exact symbol name and don't want the relevance-ranked behavior of
    /// `search`. Multiple nodes can share a name (overloads, same-named items
    /// across modules); `LIMIT 200` caps pathological cases.
    pub async fn get_nodes_by_name(&self, name: &str) -> Result<Vec<Node>> {
        let sql = concat!(
            "SELECT ",
            node_select_columns!(),
            " FROM nodes
              WHERE name = ?1
              LIMIT 200"
        );
        let mut rows = self
            .engine_conn()
            .query(sql, params![name])
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query by name: {e}"),
                operation: "get_nodes_by_name".to_string(),
            })?;
        collect_rows(&mut rows, row_to_node, "get_nodes_by_name").await
    }

    /// Returns all nodes whose `qualified_name` matches the given string.
    ///
    /// Multiple rows can share a qualified name (overloads, generic
    /// specialisations, separate `impl Trait for T` blocks). Uses the
    /// `idx_nodes_qualified_name` index for cross-run lookups by name,
    /// independent of content-hash IDs that change on edits.
    pub async fn get_nodes_by_qualified_name(&self, qname: &str) -> Result<Vec<Node>> {
        let lookup = CanonicalQualifiedName::new(qname);
        let snapshot = self
            .begin_engine_read_snapshot("get_nodes_by_qualified_name")
            .await?;

        // Bare names retain find_exact_symbol's indexed name-lookup behavior.
        // Qualified requests are handled below and never fall back to this path.
        if !lookup.is_qualified() {
            let bare_sql = concat!(
                "SELECT ",
                node_select_columns!(),
                " FROM nodes
                  WHERE name = ?1
                  LIMIT 200"
            );
            let mut rows = snapshot
                .query(bare_sql, params![lookup.terminal_name.as_str()])
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to query bare symbol name: {e}"),
                    operation: "get_nodes_by_qualified_name".to_string(),
                })?;
            let nodes = collect_rows(&mut rows, row_to_node, "get_nodes_by_qualified_name").await?;
            drop(rows);
            super::tx::commit(snapshot, "get_nodes_by_qualified_name").await?;
            return Ok(nodes);
        }

        // Prefer a stored fully-qualified name exactly as before. This keeps
        // cross-run callers that persist a graph-qualified name deterministic.
        let exact_sql = concat!(
            "SELECT ",
            node_select_columns!(),
            " FROM nodes
              WHERE qualified_name = ?1"
        );
        let mut rows = snapshot
            .query(exact_sql, params![lookup.normalized.as_str()])
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query by qualified_name: {e}"),
                operation: "get_nodes_by_qualified_name".to_string(),
            })?;

        let exact: Vec<Node> =
            collect_rows(&mut rows, row_to_node, "get_nodes_by_qualified_name").await?;
        drop(rows);
        if !exact.is_empty() {
            super::tx::commit(snapshot, "get_nodes_by_qualified_name").await?;
            return Ok(exact);
        }

        // Module spellings (for example `worktree::git_worktree_root`) do not
        // literally occur in Rust's file-backed stored names
        // (`src/worktree.rs::git_worktree_root`). Fetch only nodes with the
        // requested terminal name, then match canonical module, crate, path,
        // and suffix forms in memory. A wrong module therefore remains empty
        // instead of silently selecting a same-named callable elsewhere.
        let candidates_sql = concat!(
            "SELECT ",
            node_select_columns!(),
            " FROM nodes
              WHERE name = ?1
              LIMIT 200"
        );
        let mut candidate_rows = snapshot
            .query(candidates_sql, params![lookup.terminal_name.as_str()])
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query qualified-name candidates: {e}"),
                operation: "get_nodes_by_qualified_name".to_string(),
            })?;
        let mut matches = collect_rows(
            &mut candidate_rows,
            row_to_node,
            "get_nodes_by_qualified_name",
        )
        .await?
        .into_iter()
        .filter(|node| lookup.matches(node))
        .collect::<Vec<_>>();
        drop(candidate_rows);
        if matches.iter().any(|node| lookup.exactly_matches(node)) {
            matches.retain(|node| lookup.exactly_matches(node));
        }
        super::tx::commit(snapshot, "get_nodes_by_qualified_name").await?;
        Ok(matches)
    }

    /// Returns nodes ranked by edge count for a given edge kind and direction,
    /// optionally filtered by node kind.
    ///
    /// When `incoming` is true, ranks target nodes by incoming edge count
    /// (e.g. "most implemented interface"). When false, ranks source nodes
    /// by outgoing edge count (e.g. "class that implements the most interfaces").
    ///
    /// The query is performed entirely in SQL for efficiency — no need to load
    /// all edges into memory. Results are ordered by count descending.
    pub async fn get_ranked_nodes_by_edge_kind(
        &self,
        edge_kind: &EdgeKind,
        node_kind: Option<&NodeKind>,
        incoming: bool,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u64)>> {
        debug_assert!(
            limit > 0,
            "get_ranked_nodes_by_edge_kind limit must be positive"
        );
        debug_assert!(
            !edge_kind.as_str().is_empty(),
            "edge_kind must not be empty"
        );
        let (join_col, group_col) = if incoming {
            ("e.target", "e.target")
        } else {
            ("e.source", "e.source")
        };

        let mut conditions = vec!["e.kind = ?1".to_string()];
        let mut param_values: Vec<Value> = vec![Value::Text(edge_kind.as_str().to_string())];
        let mut param_idx = 2;

        if let Some(nk) = node_kind {
            conditions.push(format!("n.kind = ?{param_idx}"));
            param_values.push(Value::Text(nk.as_str().to_string()));
            param_idx += 1;
        }
        if let Some(prefix) = path_prefix {
            conditions.push(format!("n.file_path LIKE ?{param_idx}"));
            param_values.push(path_prefix_like_value(prefix));
            param_idx += 1;
        }

        let where_clause = conditions.join(" AND ");
        let sql = format!(
            "SELECT n.id, n.kind, n.name, n.qualified_name, n.file_path,
                    n.start_line, n.end_line, n.start_column, n.end_column,
                    n.docstring, n.signature, n.visibility, n.is_async, n.branches, n.loops, n.returns, n.max_nesting, n.unsafe_blocks, n.unchecked_calls, n.assertions, n.updated_at, n.attrs_start_line, n.parent_id,
                    COUNT(*) AS cnt
             FROM edges e
             JOIN nodes n ON {join_col} = n.id
             WHERE {where_clause}
             GROUP BY {group_col}
             ORDER BY cnt DESC
             LIMIT ?{param_idx}"
        );
        param_values.push(Value::Integer(limit as i64));

        let op = "get_ranked_nodes_by_edge_kind";
        let mut rows = self
            .engine_conn()
            .query(&sql, params_from_iter(param_values))
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query ranked nodes: {e}"),
                operation: op.to_string(),
            })?;

        let mut items = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read row: {e}"),
            operation: op.to_string(),
        })? {
            let node = row_to_node(&row).map_err(|e| TraceDecayError::Database {
                message: format!("failed to map row: {e}"),
                operation: op.to_string(),
            })?;
            let count = row.get::<u64>(23).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read count column: {e}"),
                operation: op.to_string(),
            })?;
            items.push((node, count));
        }

        Ok(items)
    }

    /// Returns nodes ranked by total incoming and outgoing connectivity.
    ///
    /// Aggregation stays inside `SQLite` so large graphs never materialize the
    /// complete edge table in the MCP process. The optional path filter is
    /// applied before `LIMIT`, unlike post-filtering an already truncated list.
    pub async fn get_hotspot_nodes(
        &self,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u64, u64)>> {
        debug_assert!(limit > 0, "get_hotspot_nodes limit must be positive");
        let mut params = Vec::new();
        let path_filter = if let Some(prefix) = path_prefix {
            params.push(path_prefix_like_value(prefix));
            "WHERE n.file_path LIKE ?1"
        } else {
            ""
        };
        let limit_index = params.len() + 1;
        params.push(Value::Integer(limit as i64));
        let sql = format!(
            "WITH connectivity AS (
                SELECT node_id,
                       SUM(incoming) AS incoming,
                       SUM(outgoing) AS outgoing
                FROM (
                    SELECT target AS node_id, COUNT(*) AS incoming, 0 AS outgoing
                    FROM edges GROUP BY target
                    UNION ALL
                    SELECT source AS node_id, 0 AS incoming, COUNT(*) AS outgoing
                    FROM edges GROUP BY source
                )
                GROUP BY node_id
             )
             SELECT n.id, n.kind, n.name, n.qualified_name, n.file_path,
                    n.start_line, n.end_line, n.start_column, n.end_column,
                    n.docstring, n.signature, n.visibility, n.is_async,
                    n.branches, n.loops, n.returns, n.max_nesting,
                    n.unsafe_blocks, n.unchecked_calls, n.assertions,
                    n.updated_at, n.attrs_start_line, n.parent_id,
                    connectivity.incoming, connectivity.outgoing
             FROM connectivity
             JOIN nodes n ON n.id = connectivity.node_id
             {path_filter}
             ORDER BY connectivity.incoming + connectivity.outgoing DESC,
                      n.id ASC
             LIMIT ?{limit_index}"
        );
        let operation = "get_hotspot_nodes";
        let mut rows = self
            .engine_conn()
            .query(&sql, params_from_iter(params))
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to query hotspot nodes: {error}"),
                operation: operation.to_owned(),
            })?;
        let mut items = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to read hotspot row: {error}"),
                operation: operation.to_owned(),
            })?
        {
            let node = row_to_node(&row).map_err(|error| TraceDecayError::Database {
                message: format!("failed to map hotspot row: {error}"),
                operation: operation.to_owned(),
            })?;
            let incoming = row
                .get::<u64>(23)
                .map_err(|error| TraceDecayError::Database {
                    message: format!("failed to read hotspot incoming count: {error}"),
                    operation: operation.to_owned(),
                })?;
            let outgoing = row
                .get::<u64>(24)
                .map_err(|error| TraceDecayError::Database {
                    message: format!("failed to read hotspot outgoing count: {error}"),
                    operation: operation.to_owned(),
                })?;
            items.push((node, incoming, outgoing));
        }
        Ok(items)
    }

    /// Returns nodes ranked by line span (`end_line` - `start_line` + 1), optionally
    /// filtered by node kind. Results are ordered by size descending.
    pub async fn get_largest_nodes(
        &self,
        node_kind: Option<&NodeKind>,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u32)>> {
        let mut conditions: Vec<String> = Vec::new();
        let mut param_values: Vec<Value> = Vec::new();
        let mut param_idx = 1;

        if let Some(nk) = node_kind {
            conditions.push(format!("kind = ?{param_idx}"));
            param_values.push(Value::Text(nk.as_str().to_string()));
            param_idx += 1;
        }
        if let Some(prefix) = path_prefix {
            conditions.push(format!("file_path LIKE ?{param_idx}"));
            param_values.push(path_prefix_like_value(prefix));
            param_idx += 1;
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let sql = format!(
            "SELECT {NODE_SELECT_COLUMNS},
                    (end_line - start_line + 1) AS lines
             FROM nodes
             {where_clause}
             ORDER BY lines DESC
             LIMIT ?{param_idx}"
        );
        param_values.push(Value::Integer(limit as i64));

        let op = "get_largest_nodes";
        let mut rows = self
            .engine_conn()
            .query(&sql, params_from_iter(param_values))
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query largest nodes: {e}"),
                operation: op.to_string(),
            })?;

        let mut items = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read row: {e}"),
            operation: op.to_string(),
        })? {
            let node = row_to_node(&row).map_err(|e| TraceDecayError::Database {
                message: format!("failed to map row: {e}"),
                operation: op.to_string(),
            })?;
            let lines = row.get::<u32>(23).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read lines column: {e}"),
                operation: op.to_string(),
            })?;
            items.push((node, lines));
        }

        Ok(items)
    }

    /// Returns files ranked by coupling (number of distinct other files connected
    /// via cross-file edges). `fan_in` mode counts how many files depend on each
    /// file; `fan_out` counts how many files each file depends on.
    ///
    /// Only `calls`, `uses`, `implements`, and `extends` edges are considered.
    pub async fn get_file_coupling(
        &self,
        fan_in: bool,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, u64)>> {
        let (group_alias, count_alias) = if fan_in {
            ("n_tgt", "n_src")
        } else {
            ("n_src", "n_tgt")
        };

        let path_filter = match path_prefix {
            Some(_) => format!("AND {group_alias}.file_path LIKE ?2"),
            None => String::new(),
        };

        let mut param_values = vec![Value::Integer(limit as i64)];
        if let Some(prefix) = path_prefix {
            param_values.push(path_prefix_like_value(prefix));
        }

        let sql = format!(
            "SELECT {group_alias}.file_path, COUNT(DISTINCT {count_alias}.file_path) AS coupling
             FROM edges e
             JOIN nodes n_src ON e.source = n_src.id
             JOIN nodes n_tgt ON e.target = n_tgt.id
             WHERE e.kind IN ('calls', 'uses', 'implements', 'extends')
               AND n_src.file_path != n_tgt.file_path
               {path_filter}
             GROUP BY {group_alias}.file_path
             ORDER BY coupling DESC
             LIMIT ?1"
        );

        let op = "get_file_coupling";
        let mut rows = self
            .engine_conn()
            .query(&sql, params_from_iter(param_values))
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query file coupling: {e}"),
                operation: op.to_string(),
            })?;

        let mut items = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read row: {e}"),
            operation: op.to_string(),
        })? {
            let file_path = row
                .get::<String>(0)
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to read file_path: {e}"),
                    operation: op.to_string(),
                })?;
            let count = row.get::<u64>(1).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read coupling count: {e}"),
                operation: op.to_string(),
            })?;
            items.push((file_path, count));
        }

        Ok(items)
    }

    /// Returns the maximum inheritance depth for classes/interfaces reachable
    /// via `extends` edges. Uses a recursive CTE to walk the hierarchy.
    ///
    /// Each result is a (`leaf_node`, depth) pair where depth is the number of
    /// `extends` hops from the leaf to the root of its hierarchy.
    pub async fn get_inheritance_depth(
        &self,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u64)>> {
        let path_filter = match path_prefix {
            Some(_) => "WHERE n.file_path LIKE ?2".to_string(),
            None => String::new(),
        };

        let mut param_values = vec![Value::Integer(limit as i64)];
        if let Some(prefix) = path_prefix {
            param_values.push(path_prefix_like_value(prefix));
        }

        // Track visited node IDs in `path` to avoid blowing up on cycles in the
        // `extends` graph. Without this guard, a cycle (or trait bound that
        // points back to itself through generics, common in Rust workspaces
        // like polkadot-sdk) makes the CTE explore the cycle up to the depth
        // bound, multiplied by every entry point — `get_inheritance_depth` then
        // takes >60s on polkadot vs 0.3s with cycle detection.
        //
        // Note the predicate order in the recursive step: `h.depth < 50` is a
        // cheap integer compare and is evaluated before the path `instr`
        // string-scan, so cycles still under the depth bound short-circuit
        // without paying for the substring search. Reducing the hierarchy to
        // `(leaf_id, max_depth)` in an inner subquery before joining `nodes`
        // means the `LIKE` path filter only runs against distinct leaves,
        // not against the (potentially huge) full hierarchy table.
        let sql = format!(
            "WITH RECURSIVE hierarchy(leaf_id, current_id, depth, path) AS (
                 SELECT e.source, e.target, 1,
                        ',' || e.source || ',' || e.target || ','
                 FROM edges e
                 WHERE e.kind = 'extends'
                 UNION ALL
                 SELECT h.leaf_id, e.target, h.depth + 1,
                        h.path || e.target || ','
                 FROM hierarchy h
                 JOIN edges e ON e.source = h.current_id AND e.kind = 'extends'
                 WHERE h.depth < 50
                   AND instr(h.path, ',' || e.target || ',') = 0
             ),
             leaf_depths AS (
                 SELECT leaf_id, MAX(depth) AS max_depth
                 FROM hierarchy
                 GROUP BY leaf_id
             )
             SELECT n.id, n.kind, n.name, n.qualified_name, n.file_path,
                    n.start_line, n.end_line, n.start_column, n.end_column,
                    n.docstring, n.signature, n.visibility, n.is_async, n.branches, n.loops, n.returns, n.max_nesting, n.unsafe_blocks, n.unchecked_calls, n.assertions, n.updated_at, n.attrs_start_line, n.parent_id,
                    ld.max_depth
             FROM leaf_depths ld
             JOIN nodes n ON ld.leaf_id = n.id
             {path_filter}
             ORDER BY ld.max_depth DESC
             LIMIT ?1"
        );

        let op = "get_inheritance_depth";
        let mut rows = self
            .engine_conn()
            .query(&sql, params_from_iter(param_values))
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query inheritance depth: {e}"),
                operation: op.to_string(),
            })?;

        let mut items = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read row: {e}"),
            operation: op.to_string(),
        })? {
            let node = row_to_node(&row).map_err(|e| TraceDecayError::Database {
                message: format!("failed to map row: {e}"),
                operation: op.to_string(),
            })?;
            let depth = row.get::<u64>(23).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read depth column: {e}"),
                operation: op.to_string(),
            })?;
            items.push((node, depth));
        }

        Ok(items)
    }

    /// Returns node kind counts grouped by file or directory prefix.
    ///
    /// If `path_prefix` is provided, only files under that path are included.
    /// Results are grouped by (`file_path`, kind) and ordered by file then count.
    pub async fn get_node_distribution(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<Vec<(String, String, u64)>> {
        let (sql, param_values): (&str, Vec<Value>) = match path_prefix {
            Some(prefix) => (
                "SELECT file_path, kind, COUNT(*) AS cnt
                 FROM nodes
                 WHERE file_path LIKE ?1
                 GROUP BY file_path, kind
                 ORDER BY file_path, cnt DESC",
                vec![path_prefix_like_value(prefix)],
            ),
            None => (
                "SELECT file_path, kind, COUNT(*) AS cnt
                 FROM nodes
                 GROUP BY file_path, kind
                 ORDER BY file_path, cnt DESC",
                vec![],
            ),
        };

        let op = "get_node_distribution";
        let mut rows = self
            .engine_conn()
            .query(sql, params_from_iter(param_values))
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query node distribution: {e}"),
                operation: op.to_string(),
            })?;

        let mut items = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read row: {e}"),
            operation: op.to_string(),
        })? {
            let file_path = row
                .get::<String>(0)
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to read file_path: {e}"),
                    operation: op.to_string(),
                })?;
            let kind = row
                .get::<String>(1)
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to read kind: {e}"),
                    operation: op.to_string(),
                })?;
            let count = row.get::<u64>(2).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read count: {e}"),
                operation: op.to_string(),
            })?;
            items.push((file_path, kind, count));
        }

        Ok(items)
    }

    /// Returns all `calls` edges for cycle detection in the call graph.
    ///
    /// Returns `(source_id, target_id)` pairs for every `calls` edge.
    ///
    /// Read through `rowid` keyset pages over `edges`. `kind = 'calls'` is the
    /// largest partition of the largest table, not a bound: a real repository
    /// records far more call edges than the `SQLite` runtime will materialize
    /// for one query, and the runtime refuses an oversized query outright
    /// rather than truncating it.
    pub async fn get_call_edges(&self, path_prefix: Option<&str>) -> Result<Vec<(String, String)>> {
        let (sql, leading) = match path_prefix {
            Some(prefix) => (
                CALL_EDGE_PREFIXED_PAGE_SQL,
                vec![path_prefix_like_value(prefix)],
            ),
            None => (CALL_EDGE_PAGE_SQL, vec![]),
        };
        collect_rowid_pages_with(
            &self.engine_conn(),
            sql,
            &leading,
            2,
            |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?)),
            "get_call_edges",
        )
        .await
    }

    /// Returns all `calls` edges with their source line for cycle detection.
    ///
    /// Returns `(source_id, target_id, line)` tuples for every `calls` edge.
    ///
    /// Paged for the same reason as [`Database::get_call_edges`].
    pub async fn get_call_edges_with_lines(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<Vec<(String, String, Option<u32>)>> {
        let (sql, leading) = match path_prefix {
            Some(prefix) => (
                CALL_EDGE_LINE_PREFIXED_PAGE_SQL,
                vec![path_prefix_like_value(prefix)],
            ),
            None => (CALL_EDGE_LINE_PAGE_SQL, vec![]),
        };
        collect_rowid_pages_with(
            &self.engine_conn(),
            sql,
            &leading,
            3,
            |row| {
                Ok((
                    row.get::<String>(0)?,
                    row.get::<String>(1)?,
                    row.get::<u32>(2).ok(),
                ))
            },
            "get_call_edges_with_lines",
        )
        .await
    }

    /// Returns functions/methods ranked by a composite complexity score.
    ///
    /// Complexity = `line_count` + (`call_fan_out` * 3) + `call_fan_in`.
    /// Line count reflects size, fan-out reflects cognitive load, fan-in
    /// reflects coupling. Results are ordered by score descending.
    pub async fn get_complexity_ranked(
        &self,
        node_kind: Option<&NodeKind>,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u32, u64, u64, u64)>> {
        debug_assert!(limit > 0, "get_complexity_ranked limit must be positive");
        let mut conditions: Vec<String> = Vec::new();
        let mut param_values: Vec<Value> = Vec::new();
        let mut param_idx = 1;

        match node_kind {
            Some(nk) => {
                conditions.push(format!("n.kind = ?{param_idx}"));
                param_values.push(Value::Text(nk.as_str().to_string()));
                param_idx += 1;
            }
            None => {
                conditions.push("n.kind IN ('function', 'method')".to_string());
            }
        }
        if let Some(prefix) = path_prefix {
            conditions.push(format!("n.file_path LIKE ?{param_idx}"));
            param_values.push(path_prefix_like_value(prefix));
            param_idx += 1;
        }

        let where_clause = conditions.join(" AND ");
        let sql = format!(
            "SELECT n.id, n.kind, n.name, n.qualified_name, n.file_path,
                    n.start_line, n.end_line, n.start_column, n.end_column,
                    n.docstring, n.signature, n.visibility, n.is_async, n.branches, n.loops, n.returns, n.max_nesting, n.unsafe_blocks, n.unchecked_calls, n.assertions, n.updated_at, n.attrs_start_line, n.parent_id,
                    (n.end_line - n.start_line + 1) AS lines,
                    COALESCE(out_calls.cnt, 0) AS fan_out,
                    COALESCE(in_calls.cnt, 0) AS fan_in,
                    ((n.end_line - n.start_line + 1) + COALESCE(out_calls.cnt, 0) * 3 + COALESCE(in_calls.cnt, 0)) AS score
             FROM nodes n
             LEFT JOIN (SELECT source, COUNT(*) AS cnt FROM edges WHERE kind = 'calls' GROUP BY source) out_calls ON out_calls.source = n.id
             LEFT JOIN (SELECT target, COUNT(*) AS cnt FROM edges WHERE kind = 'calls' GROUP BY target) in_calls ON in_calls.target = n.id
             WHERE {where_clause}
             ORDER BY score DESC
             LIMIT ?{param_idx}"
        );
        param_values.push(Value::Integer(limit as i64));

        let op = "get_complexity_ranked";
        let mut rows = self
            .engine_conn()
            .query(&sql, params_from_iter(param_values))
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query complexity ranking: {e}"),
                operation: op.to_string(),
            })?;

        let mut items = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read row: {e}"),
            operation: op.to_string(),
        })? {
            let node = row_to_node(&row).map_err(|e| TraceDecayError::Database {
                message: format!("failed to map row: {e}"),
                operation: op.to_string(),
            })?;
            let lines = row.get::<u32>(23).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read lines: {e}"),
                operation: op.to_string(),
            })?;
            let fan_out = row.get::<u64>(24).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read fan_out: {e}"),
                operation: op.to_string(),
            })?;
            let fan_in = row.get::<u64>(25).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read fan_in: {e}"),
                operation: op.to_string(),
            })?;
            let score = row.get::<u64>(26).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read score: {e}"),
                operation: op.to_string(),
            })?;
            items.push((node, lines, fan_out, fan_in, score));
        }

        Ok(items)
    }

    /// Returns public symbols that are missing docstrings.
    ///
    /// Filters to kinds that conventionally carry per-declaration docs
    /// (functions, methods, types, fields, variants, constants, modules, …).
    /// Excludes `namespace` and `package` because they are aggregators that
    /// almost never carry their own doc — reporting them would drown
    /// actionable items in noise. Checks for `NULL` or empty docstring.
    pub async fn get_undocumented_public_symbols(
        &self,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Node>> {
        const DOC_COVERAGE_KINDS: &str = "'function', 'method', 'class', 'interface', 'trait', \
            'struct', 'enum', 'module', 'field', 'enum_variant', 'const', 'static', 'type_alias', \
            'property', 'csharp_property', 'record', 'data_class', 'sealed_class', 'object', \
            'case_class', 'kotlin_object', 'inner_class', 'abstract_method', 'constructor', \
            'struct_method', 'val', 'var', 'mixin', 'extension', 'union', 'typedef'";

        let (sql, param_values): (String, Vec<Value>) = match path_prefix {
            Some(prefix) => (
                format!(
                    "SELECT {NODE_SELECT_COLUMNS}
                     FROM nodes
                     WHERE visibility = 'public'
                       AND (docstring IS NULL OR docstring = '')
                       AND kind IN ({DOC_COVERAGE_KINDS})
                       AND file_path LIKE ?1
                     ORDER BY file_path, start_line
                     LIMIT ?2"
                ),
                vec![path_prefix_like_value(prefix), Value::Integer(limit as i64)],
            ),
            None => (
                format!(
                    "SELECT {NODE_SELECT_COLUMNS}
                     FROM nodes
                     WHERE visibility = 'public'
                       AND (docstring IS NULL OR docstring = '')
                       AND kind IN ({DOC_COVERAGE_KINDS})
                     ORDER BY file_path, start_line
                     LIMIT ?1"
                ),
                vec![Value::Integer(limit as i64)],
            ),
        };

        let op = "get_undocumented_public_symbols";
        let mut rows = self
            .engine_conn()
            .query(&sql, params_from_iter(param_values))
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query undocumented symbols: {e}"),
                operation: op.to_string(),
            })?;

        collect_rows(&mut rows, row_to_node, op).await
    }

    /// Returns classes/structs ranked by number of contained members
    /// (methods, fields, constructors). Identifies "god classes" with
    /// excessive responsibility.
    pub async fn get_god_classes(
        &self,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u64, u64, u64)>> {
        let path_filter = match path_prefix {
            Some(_) => "AND n.file_path LIKE ?2".to_string(),
            None => String::new(),
        };

        let mut param_values = vec![Value::Integer(limit as i64)];
        if let Some(prefix) = path_prefix {
            param_values.push(path_prefix_like_value(prefix));
        }

        // After v9, containment is `nodes.parent_id`, not Contains edges.
        // Join each candidate container directly to its children via parent_id.
        let sql = format!(
            "SELECT n.id, n.kind, n.name, n.qualified_name, n.file_path,
                    n.start_line, n.end_line, n.start_column, n.end_column,
                    n.docstring, n.signature, n.visibility, n.is_async, n.branches, n.loops, n.returns, n.max_nesting, n.unsafe_blocks, n.unchecked_calls, n.assertions, n.updated_at, n.attrs_start_line, n.parent_id,
                    SUM(CASE WHEN c.kind IN ('method', 'abstract_method', 'constructor') THEN 1 ELSE 0 END) AS methods,
                    SUM(CASE WHEN c.kind = 'field' THEN 1 ELSE 0 END) AS fields,
                    COUNT(*) AS total
             FROM nodes n
             JOIN nodes c ON c.parent_id = n.id
             WHERE n.kind IN ('class', 'struct', 'inner_class', 'object')
               {path_filter}
             GROUP BY n.id
             ORDER BY total DESC
             LIMIT ?1"
        );

        let op = "get_god_classes";
        let mut rows = self
            .engine_conn()
            .query(&sql, params_from_iter(param_values))
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query god classes: {e}"),
                operation: op.to_string(),
            })?;

        let mut items = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read row: {e}"),
            operation: op.to_string(),
        })? {
            let node = row_to_node(&row).map_err(|e| TraceDecayError::Database {
                message: format!("failed to map row: {e}"),
                operation: op.to_string(),
            })?;
            let methods = row.get::<u64>(23).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read methods: {e}"),
                operation: op.to_string(),
            })?;
            let fields = row.get::<u64>(24).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read fields: {e}"),
                operation: op.to_string(),
            })?;
            let total = row.get::<u64>(25).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read total: {e}"),
                operation: op.to_string(),
            })?;
            items.push((node, methods, fields, total));
        }

        Ok(items)
    }

    /// Returns all nodes under a directory prefix filtered by kinds.
    ///
    /// Uses `LIKE dir || '%'` for the path prefix and an `IN` clause for kinds.
    ///
    /// Read through `rowid` keyset pages: a repository-root prefix selects most
    /// of the `nodes` table, which exceeds what the `SQLite` runtime will
    /// materialize for one query. The pages arrive in `rowid` order, so the
    /// `(file_path, start_line)` ordering callers see is restored here rather
    /// than by the database.
    pub async fn get_nodes_by_dir(&self, dir: &str, kinds: &[NodeKind]) -> Result<Vec<Node>> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }

        let sql = nodes_by_dir_page_sql(kinds.len());

        let mut leading: Vec<Value> = Vec::with_capacity(kinds.len() + 1);
        leading.push(Value::Text(dir.to_string()));
        for k in kinds {
            leading.push(Value::Text(k.as_str().to_string()));
        }

        let mut nodes = collect_rowid_pages_with(
            &self.engine_conn(),
            &sql,
            &leading,
            NODE_COLUMNS,
            row_to_node,
            "get_nodes_by_dir",
        )
        .await?;
        nodes.sort_by(|a, b| {
            a.file_path
                .cmp(&b.file_path)
                .then_with(|| a.start_line.cmp(&b.start_line))
        });
        Ok(nodes)
    }

    // ---------------------------------------------------------------------
    // Whole-graph aggregates for health/gini reports.
    //
    // These fold the node/edge tables inside `SQLite` (`GROUP BY`) so the
    // reports never materialize `Vec<Node>` / `Vec<Edge>` copies of a real
    // project's whole graph in the MCP process. Every method returns one row
    // per file (or per cross-file file pair), which callers then filter by
    // path scope in Rust. That group-then-filter order is byte-identical to
    // filtering nodes before folding, because every node in a file shares
    // that file's `file_path`, so a scope predicate keyed on `file_path`
    // partitions whole groups and never splits a per-file sum.
    // ---------------------------------------------------------------------

    /// Per-file sum of the raw complexity metric
    /// (`branches + loops + returns + max_nesting`).
    ///
    /// Returns one `(file_path, sum)` row per file holding at least one node.
    /// The columns are `INTEGER NOT NULL DEFAULT 0`, so the `SUM` is an exact
    /// integer and the `as f64` widening is lossless for any real project
    /// (well under 2^53), matching an incremental `f64` fold byte-for-byte.
    pub async fn complexity_sum_by_file(&self) -> Result<Vec<(String, f64)>> {
        self.file_metric_sums(
            "SELECT file_path, \
             CAST(SUM(branches + loops + returns + max_nesting) AS INTEGER) \
             FROM nodes GROUP BY file_path",
            "complexity_sum_by_file",
        )
        .await
    }

    /// Per-file sum of node line spans (`end_line - start_line + 1`, floored at
    /// 1 line). Mirrors the `end_line.saturating_sub(start_line) + 1` fold.
    pub async fn line_span_sum_by_file(&self) -> Result<Vec<(String, f64)>> {
        self.file_metric_sums(
            "SELECT file_path, \
             CAST(SUM(MAX(end_line - start_line, 0) + 1) AS INTEGER) \
             FROM nodes GROUP BY file_path",
            "line_span_sum_by_file",
        )
        .await
    }

    /// Shared driver for `SELECT file_path, <int sum> FROM nodes GROUP BY
    /// file_path` queries. The grouped result is one row per file (a few
    /// thousand at most), so it is read in a single query without paging.
    async fn file_metric_sums(&self, sql: &str, op: &str) -> Result<Vec<(String, f64)>> {
        let mut rows =
            self.engine_conn()
                .query(sql, ())
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to query per-file metric sums: {e}"),
                    operation: op.to_string(),
                })?;
        let mut items = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read per-file metric row: {e}"),
            operation: op.to_string(),
        })? {
            let file: String = row.get(0).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read file_path: {e}"),
                operation: op.to_string(),
            })?;
            let total: i64 = row.get(1).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read metric sum: {e}"),
                operation: op.to_string(),
            })?;
            items.push((file, total as f64));
        }
        Ok(items)
    }

    /// Per-function/method raw complexity, projected as `(file_path, name,
    /// branches + loops + returns + max_nesting)`. Used by the symbol-scope
    /// gini metric. Read through `rowid` keyset pages because the number of
    /// function/method nodes on a real project exceeds what the runtime will
    /// materialize for a single query.
    pub async fn symbol_complexity(&self) -> Result<Vec<(String, String, f64)>> {
        let rows: Vec<(String, String, i64)> = collect_rowid_pages(
            &self.engine_conn(),
            "SELECT file_path, name, (branches + loops + returns + max_nesting), rowid \
             FROM nodes \
             WHERE kind IN ('function', 'method') AND rowid > ?1 \
             ORDER BY rowid LIMIT ?2",
            3,
            |row| {
                Ok((
                    row.get::<String>(0)?,
                    row.get::<String>(1)?,
                    row.get::<i64>(2)?,
                ))
            },
            "symbol_complexity",
        )
        .await?;
        Ok(rows
            .into_iter()
            .map(|(file, name, value)| (file, name, value as f64))
            .collect())
    }

    /// The distinct set of file paths holding at least one node. Bounded by the
    /// file count (a few thousand), so it is read in a single query.
    pub async fn distinct_node_file_paths(&self) -> Result<Vec<String>> {
        let mut rows = self
            .engine_conn()
            .query("SELECT DISTINCT file_path FROM nodes", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query distinct node file paths: {e}"),
                operation: "distinct_node_file_paths".to_string(),
            })?;
        collect_rows(
            &mut rows,
            |row| row.get::<String>(0),
            "distinct_node_file_paths",
        )
        .await
    }

    /// Cross-file directed edge counts as `(src_file, tgt_file, count)`, one row
    /// per distinct ordered file pair connected by at least one edge whose
    /// endpoints live in different files. Every edge references real nodes
    /// (foreign key), so the inner join counts exactly the edges an
    /// all-edges fold would visit.
    ///
    /// Read through keyset pages on `(src_file, tgt_file)` — the same transport
    /// pattern as `build_file_adjacency` — because the distinct pair set can
    /// exceed the runtime's single-query materialization limit. The pair key is
    /// the `GROUP BY` key, so a page boundary never splits a group.
    pub async fn cross_file_edge_pair_counts(&self) -> Result<Vec<(String, String, u64)>> {
        const PAGE_ROWS: i64 = 2_000;
        let sql = "SELECT n1.file_path AS src, n2.file_path AS tgt, COUNT(*) AS cnt \
                   FROM edges e \
                   JOIN nodes n1 ON e.source = n1.id \
                   JOIN nodes n2 ON e.target = n2.id \
                   WHERE n1.file_path != n2.file_path \
                   AND (n1.file_path > ?1 OR (n1.file_path = ?1 AND n2.file_path > ?2)) \
                   GROUP BY n1.file_path, n2.file_path \
                   ORDER BY src, tgt \
                   LIMIT ?3";
        let mut items = Vec::new();
        let mut cursor = (String::new(), String::new());
        loop {
            let mut rows = self
                .engine_conn()
                .query(sql, (cursor.0.clone(), cursor.1.clone(), PAGE_ROWS))
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to query cross-file edge pair counts: {e}"),
                    operation: "cross_file_edge_pair_counts".to_string(),
                })?;
            let mut page_rows = 0_i64;
            while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
                message: format!("failed to read edge pair row: {e}"),
                operation: "cross_file_edge_pair_counts".to_string(),
            })? {
                let src: String = row.get(0).unwrap_or_default();
                let tgt: String = row.get(1).unwrap_or_default();
                let cnt: u64 = row.get(2).map_err(|e| TraceDecayError::Database {
                    message: format!("failed to read edge pair count: {e}"),
                    operation: "cross_file_edge_pair_counts".to_string(),
                })?;
                cursor = (src.clone(), tgt.clone());
                page_rows += 1;
                items.push((src, tgt, cnt));
            }
            drop(rows);
            if page_rows < PAGE_ROWS {
                break;
            }
        }
        Ok(items)
    }

    /// Per-file health aggregates folded in one `GROUP BY` scan: the weighted
    /// complexity sum (`branches*2 + loops*2 + max_nesting*3 + line_span`), the
    /// function/method count, and the count of function/method nodes carrying a
    /// `skip-test-coverage` docstring marker. Replaces a whole-table
    /// `get_all_nodes` fold plus a separate skip-marker scan in the health
    /// snapshot. One row per file holding at least one node.
    pub async fn health_file_aggregates(&self) -> Result<Vec<HealthFileAggregate>> {
        let sql = "SELECT file_path, \
                   CAST(SUM(branches * 2 + loops * 2 + max_nesting * 3 \
                            + (MAX(end_line - start_line, 0) + 1)) AS INTEGER) AS complexity, \
                   SUM(CASE WHEN kind IN ('function', 'method') THEN 1 ELSE 0 END) AS fns, \
                   SUM(CASE WHEN kind IN ('function', 'method') \
                             AND docstring LIKE '%skip-test-coverage%' \
                            THEN 1 ELSE 0 END) AS skipped \
                   FROM nodes GROUP BY file_path";
        let op = "health_file_aggregates";
        let mut rows =
            self.engine_conn()
                .query(sql, ())
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to query health file aggregates: {e}"),
                    operation: op.to_string(),
                })?;
        let mut items = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read health aggregate row: {e}"),
            operation: op.to_string(),
        })? {
            let file_path: String = row.get(0).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read file_path: {e}"),
                operation: op.to_string(),
            })?;
            let complexity: i64 = row.get(1).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read complexity sum: {e}"),
                operation: op.to_string(),
            })?;
            let fns: i64 = row.get(2).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read function/method count: {e}"),
                operation: op.to_string(),
            })?;
            let skipped: i64 = row.get(3).map_err(|e| TraceDecayError::Database {
                message: format!("failed to read skip-coverage count: {e}"),
                operation: op.to_string(),
            })?;
            items.push(HealthFileAggregate {
                file_path,
                complexity: complexity as f64,
                function_methods: fns.max(0) as usize,
                skipped_function_methods: skipped.max(0) as usize,
            });
        }
        Ok(items)
    }
}

/// One file's health aggregates, folded in `Database::health_file_aggregates`.
#[derive(Debug, Clone)]
pub struct HealthFileAggregate {
    /// The file these aggregates are scoped to.
    pub file_path: String,
    /// Weighted complexity sum over the file's nodes.
    pub complexity: f64,
    /// Number of `function`/`method` nodes in the file.
    pub function_methods: usize,
    /// Number of `function`/`method` nodes whose docstring carries the
    /// `skip-test-coverage` marker.
    pub skipped_function_methods: usize,
}

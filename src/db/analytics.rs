// Rust guideline compliant 2025-10-17
use libsql::params;

use super::connection::Database;
use super::rows::row_to_node;
use super::sql::{collect_rows, path_prefix_like_value};
use crate::errors::{Result, TraceDecayError};
use crate::types::*;

impl Database {
    /// Returns all nodes whose `name` column matches the given bare identifier.
    ///
    /// Pure index lookup against `idx_nodes_name` — O(log n) with no BM25
    /// scoring, no fuzzy match, no fallback. Use this when you already know
    /// the exact symbol name and don't want the relevance-ranked behavior of
    /// `search`. Multiple nodes can share a name (overloads, same-named items
    /// across modules); `LIMIT 200` caps pathological cases.
    pub async fn get_nodes_by_name(&self, name: &str) -> Result<Vec<Node>> {
        let sql = "SELECT id, kind, name, qualified_name, file_path,
                          start_line, end_line, start_column, end_column,
                          docstring, signature, visibility, is_async, branches, loops, returns, max_nesting, unsafe_blocks, unchecked_calls, assertions, updated_at, attrs_start_line, parent_id
                   FROM nodes
                   WHERE name = ?1
                   LIMIT 200";
        let mut rows =
            self.conn()
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
        // Exact match first — preserves the precise-lookup contract.
        let exact_sql = "SELECT id, kind, name, qualified_name, file_path,
                          start_line, end_line, start_column, end_column,
                          docstring, signature, visibility, is_async, branches, loops, returns, max_nesting, unsafe_blocks, unchecked_calls, assertions, updated_at, attrs_start_line, parent_id
                   FROM nodes
                   WHERE qualified_name = ?1";
        let mut rows = self
            .conn()
            .query(exact_sql, params![qname])
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query by qualified_name: {e}"),
                operation: "get_nodes_by_qualified_name".to_string(),
            })?;

        let exact: Vec<Node> =
            collect_rows(&mut rows, row_to_node, "get_nodes_by_qualified_name").await?;
        if !exact.is_empty() {
            return Ok(exact);
        }

        // Fallback strategy depends on whether the user passed a qualified
        // form or just a bare identifier:
        //
        // - `Type::method` (contains `::`) → suffix match. Recovers from
        //   extractor quirks (duplicated path segments, file-path prefixes
        //   the caller doesn't know about) and lets callers pass partial
        //   module paths. The leading `%` defeats `idx_nodes_qualified_name`,
        //   so this is a full table scan bounded by `LIMIT 50` — cheap at
        //   typical graph sizes.
        //
        // - `foo` (no `::`) → exact `name = ?` match. Uses `idx_nodes_name`,
        //   so it stays fast. Multiple nodes may share a name (overloads,
        //   `new()` constructors), `LIMIT 50` is a safety net.
        let (sql, pattern) = if qname.contains("::") {
            (
                "SELECT id, kind, name, qualified_name, file_path,
                        start_line, end_line, start_column, end_column,
                        docstring, signature, visibility, is_async, branches, loops, returns, max_nesting, unsafe_blocks, unchecked_calls, assertions, updated_at, attrs_start_line, parent_id
                 FROM nodes
                 WHERE qualified_name LIKE ?1
                 LIMIT 50",
                format!("%::{qname}"),
            )
        } else {
            (
                "SELECT id, kind, name, qualified_name, file_path,
                        start_line, end_line, start_column, end_column,
                        docstring, signature, visibility, is_async, branches, loops, returns, max_nesting, unsafe_blocks, unchecked_calls, assertions, updated_at, attrs_start_line, parent_id
                 FROM nodes
                 WHERE name = ?1
                 LIMIT 50",
                qname.to_string(),
            )
        };
        let mut fallback_rows = self
            .conn()
            .query(sql, params![pattern.as_str()])
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query by qualified_name fallback: {e}"),
                operation: "get_nodes_by_qualified_name".to_string(),
            })?;
        collect_rows(
            &mut fallback_rows,
            row_to_node,
            "get_nodes_by_qualified_name",
        )
        .await
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
        let mut param_values: Vec<libsql::Value> =
            vec![libsql::Value::Text(edge_kind.as_str().to_string())];
        let mut param_idx = 2;

        if let Some(nk) = node_kind {
            conditions.push(format!("n.kind = ?{param_idx}"));
            param_values.push(libsql::Value::Text(nk.as_str().to_string()));
            param_idx += 1;
        }
        if let Some(prefix) = path_prefix {
            conditions.push(format!("n.file_path LIKE ?{param_idx}"));
            param_values.push(libsql::Value::Text(format!("{prefix}%")));
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
        param_values.push(libsql::Value::Integer(limit as i64));

        let op = "get_ranked_nodes_by_edge_kind";
        let mut rows = self
            .conn()
            .query(&sql, libsql::params_from_iter(param_values))
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

    /// Returns nodes ranked by line span (`end_line` - `start_line` + 1), optionally
    /// filtered by node kind. Results are ordered by size descending.
    pub async fn get_largest_nodes(
        &self,
        node_kind: Option<&NodeKind>,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u32)>> {
        let mut conditions: Vec<String> = Vec::new();
        let mut param_values: Vec<libsql::Value> = Vec::new();
        let mut param_idx = 1;

        if let Some(nk) = node_kind {
            conditions.push(format!("kind = ?{param_idx}"));
            param_values.push(libsql::Value::Text(nk.as_str().to_string()));
            param_idx += 1;
        }
        if let Some(prefix) = path_prefix {
            conditions.push(format!("file_path LIKE ?{param_idx}"));
            param_values.push(libsql::Value::Text(format!("{prefix}%")));
            param_idx += 1;
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let sql = format!(
            "SELECT id, kind, name, qualified_name, file_path,
                    start_line, end_line, start_column, end_column,
                    docstring, signature, visibility, is_async, branches, loops, returns, max_nesting, unsafe_blocks, unchecked_calls, assertions, updated_at, attrs_start_line, parent_id,
                    (end_line - start_line + 1) AS lines
             FROM nodes
             {where_clause}
             ORDER BY lines DESC
             LIMIT ?{param_idx}"
        );
        param_values.push(libsql::Value::Integer(limit as i64));

        let op = "get_largest_nodes";
        let mut rows = self
            .conn()
            .query(&sql, libsql::params_from_iter(param_values))
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

        let mut param_values = vec![libsql::Value::Integer(limit as i64)];
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
            .conn()
            .query(&sql, libsql::params_from_iter(param_values))
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

        let mut param_values = vec![libsql::Value::Integer(limit as i64)];
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
            .conn()
            .query(&sql, libsql::params_from_iter(param_values))
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
        let (sql, param_values): (&str, Vec<libsql::Value>) = match path_prefix {
            Some(prefix) => (
                "SELECT file_path, kind, COUNT(*) AS cnt
                 FROM nodes
                 WHERE file_path LIKE ?1
                 GROUP BY file_path, kind
                 ORDER BY file_path, cnt DESC",
                vec![libsql::Value::Text(format!("{prefix}%"))],
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
            .conn()
            .query(sql, libsql::params_from_iter(param_values))
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
    pub async fn get_call_edges(&self, path_prefix: Option<&str>) -> Result<Vec<(String, String)>> {
        let op = "get_call_edges";
        let (sql, param_values): (String, Vec<libsql::Value>) = match path_prefix {
            Some(prefix) => (
                "SELECT e.source, e.target FROM edges e
                 JOIN nodes n ON e.source = n.id
                 WHERE e.kind = 'calls' AND n.file_path LIKE ?1"
                    .to_string(),
                vec![libsql::Value::Text(format!("{prefix}%"))],
            ),
            None => (
                "SELECT source, target FROM edges WHERE kind = 'calls'".to_string(),
                vec![],
            ),
        };
        let mut rows = self
            .conn()
            .query(&sql, libsql::params_from_iter(param_values))
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query call edges: {e}"),
                operation: op.to_string(),
            })?;

        let mut items = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read row: {e}"),
            operation: op.to_string(),
        })? {
            let source = row
                .get::<String>(0)
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to read source: {e}"),
                    operation: op.to_string(),
                })?;
            let target = row
                .get::<String>(1)
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to read target: {e}"),
                    operation: op.to_string(),
                })?;
            items.push((source, target));
        }

        Ok(items)
    }

    /// Returns all `calls` edges with their source line for cycle detection.
    ///
    /// Returns `(source_id, target_id, line)` tuples for every `calls` edge.
    pub async fn get_call_edges_with_lines(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<Vec<(String, String, Option<u32>)>> {
        let op = "get_call_edges_with_lines";
        let (sql, param_values): (String, Vec<libsql::Value>) = match path_prefix {
            Some(prefix) => (
                "SELECT e.source, e.target, e.line FROM edges e
                 JOIN nodes n ON e.source = n.id
                 WHERE e.kind = 'calls' AND n.file_path LIKE ?1"
                    .to_string(),
                vec![libsql::Value::Text(format!("{prefix}%"))],
            ),
            None => (
                "SELECT source, target, line FROM edges WHERE kind = 'calls'".to_string(),
                vec![],
            ),
        };
        let mut rows = self
            .conn()
            .query(&sql, libsql::params_from_iter(param_values))
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query call edges with lines: {e}"),
                operation: op.to_string(),
            })?;

        let mut items = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read row: {e}"),
            operation: op.to_string(),
        })? {
            let source = row
                .get::<String>(0)
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to read source: {e}"),
                    operation: op.to_string(),
                })?;
            let target = row
                .get::<String>(1)
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to read target: {e}"),
                    operation: op.to_string(),
                })?;
            let line = row.get::<u32>(2).ok();
            items.push((source, target, line));
        }

        Ok(items)
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
        let mut param_values: Vec<libsql::Value> = Vec::new();
        let mut param_idx = 1;

        match node_kind {
            Some(nk) => {
                conditions.push(format!("n.kind = ?{param_idx}"));
                param_values.push(libsql::Value::Text(nk.as_str().to_string()));
                param_idx += 1;
            }
            None => {
                conditions.push("n.kind IN ('function', 'method')".to_string());
            }
        }
        if let Some(prefix) = path_prefix {
            conditions.push(format!("n.file_path LIKE ?{param_idx}"));
            param_values.push(libsql::Value::Text(format!("{prefix}%")));
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
        param_values.push(libsql::Value::Integer(limit as i64));

        let op = "get_complexity_ranked";
        let mut rows = self
            .conn()
            .query(&sql, libsql::params_from_iter(param_values))
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

        let (sql, param_values): (String, Vec<libsql::Value>) = match path_prefix {
            Some(prefix) => (
                format!(
                    "SELECT id, kind, name, qualified_name, file_path,
                            start_line, end_line, start_column, end_column,
                            docstring, signature, visibility, is_async, branches, loops, returns, max_nesting, unsafe_blocks, unchecked_calls, assertions, updated_at, attrs_start_line, parent_id
                     FROM nodes
                     WHERE visibility = 'public'
                       AND (docstring IS NULL OR docstring = '')
                       AND kind IN ({DOC_COVERAGE_KINDS})
                       AND file_path LIKE ?1
                     ORDER BY file_path, start_line
                     LIMIT ?2"
                ),
                vec![
                    libsql::Value::Text(format!("{prefix}%")),
                    libsql::Value::Integer(limit as i64),
                ],
            ),
            None => (
                format!(
                    "SELECT id, kind, name, qualified_name, file_path,
                            start_line, end_line, start_column, end_column,
                            docstring, signature, visibility, is_async, branches, loops, returns, max_nesting, unsafe_blocks, unchecked_calls, assertions, updated_at, attrs_start_line, parent_id
                     FROM nodes
                     WHERE visibility = 'public'
                       AND (docstring IS NULL OR docstring = '')
                       AND kind IN ({DOC_COVERAGE_KINDS})
                     ORDER BY file_path, start_line
                     LIMIT ?1"
                ),
                vec![libsql::Value::Integer(limit as i64)],
            ),
        };

        let op = "get_undocumented_public_symbols";
        let mut rows = self
            .conn()
            .query(&sql, libsql::params_from_iter(param_values))
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

        let mut param_values = vec![libsql::Value::Integer(limit as i64)];
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
            .conn()
            .query(&sql, libsql::params_from_iter(param_values))
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
    pub async fn get_nodes_by_dir(&self, dir: &str, kinds: &[NodeKind]) -> Result<Vec<Node>> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }

        let kind_placeholders: Vec<String> = kinds
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 2))
            .collect();
        let sql = format!(
            "SELECT id, kind, name, qualified_name, file_path,
                    start_line, end_line, start_column, end_column,
                    docstring, signature, visibility, is_async,
                    branches, loops, returns, max_nesting,
                    unsafe_blocks, unchecked_calls, assertions, updated_at, attrs_start_line, parent_id
             FROM nodes
             WHERE file_path LIKE ?1 || '%' AND kind IN ({})
             ORDER BY file_path, start_line",
            kind_placeholders.join(", ")
        );

        let mut param_values: Vec<libsql::Value> = Vec::new();
        param_values.push(libsql::Value::Text(dir.to_string()));
        for k in kinds {
            param_values.push(libsql::Value::Text(k.as_str().to_string()));
        }

        let mut rows = self
            .conn()
            .query(&sql, libsql::params_from_iter(param_values))
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query nodes by dir: {e}"),
                operation: "get_nodes_by_dir".to_string(),
            })?;

        collect_rows(&mut rows, row_to_node, "get_nodes_by_dir").await
    }
}

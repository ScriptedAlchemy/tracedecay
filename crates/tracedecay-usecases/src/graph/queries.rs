// Rust guideline compliant 2025-10-17
use std::collections::{HashMap, HashSet};

use tracedecay_runtime_core::db::{Database, DatabaseWriteTransaction};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::types::*;

/// Metrics describing the connectivity and structure around a single node.
#[derive(Debug, Clone)]
pub struct NodeMetrics {
    /// Number of incoming edges (all kinds).
    pub incoming_edge_count: usize,
    /// Number of outgoing edges (all kinds).
    pub outgoing_edge_count: usize,
    /// Number of outgoing `Calls` edges (functions this node calls).
    pub call_count: usize,
    /// Number of incoming `Calls` edges (functions that call this node).
    pub caller_count: usize,
    /// Number of outgoing `Contains` edges (direct children).
    pub child_count: usize,
    /// Depth of the node in the containment hierarchy.
    pub depth: usize,
}

/// Distinct file-pair rows requested per page of the whole-graph adjacency
/// scan. Stays under the `SQLite` runtime's per-query materialization admission.
const ADJACENCY_SCAN_PAGE_ROWS: i64 = 2_000;

/// Bounded whole-graph file adjacency plus the rows examined to build it.
#[derive(Debug)]
pub struct FileAdjacencyScan {
    pub adjacency: HashMap<String, HashSet<String>>,
    pub files_examined: usize,
    pub dependency_edges_examined: usize,
}

/// Provides analytical query operations over the code graph.
pub struct GraphQueryManager<'a> {
    db: &'a Database,
}

fn row_to_node_dead_code(
    row: &tracedecay_runtime_core::db::engine::Row,
) -> std::result::Result<Node, tracedecay_runtime_core::db::engine::Error> {
    let kind_str = get_string_lossy(row, 1)?;
    let vis_str = get_string_lossy(row, 11)?;
    let is_async_int = row.get::<i64>(12)?;
    let start_line = row.get::<u32>(5)?;
    // Same contract as `row_to_node` in db/rows.rs: a stored 0 is a legitimate
    // value (item documented at the very top of a file), so trust the stored
    // integer verbatim and fall back to `start_line` only when the column is
    // genuinely absent — SQL NULL on a legacy row, or a SELECT list that does
    // not request column 21.
    let attrs_start_line = row
        .get::<Option<u32>>(21)
        .ok()
        .flatten()
        .unwrap_or(start_line);

    Ok(Node {
        id: get_string_lossy(row, 0)?,
        kind: NodeKind::from_str(&kind_str).unwrap_or(NodeKind::Function),
        name: get_string_lossy(row, 2)?,
        qualified_name: get_string_lossy(row, 3)?,
        file_path: get_string_lossy(row, 4)?,
        start_line,
        attrs_start_line,
        end_line: row.get::<u32>(6)?,
        start_column: row.get::<u32>(7)?,
        end_column: row.get::<u32>(8)?,
        signature: get_opt_string_lossy(row, 10)?,
        docstring: get_opt_string_lossy(row, 9)?,
        visibility: Visibility::from_str(&vis_str).unwrap_or_default(),
        is_async: is_async_int != 0,
        branches: row.get::<u32>(13)?,
        loops: row.get::<u32>(14)?,
        returns: row.get::<u32>(15)?,
        max_nesting: row.get::<u32>(16)?,
        unsafe_blocks: row.get::<u32>(17)?,
        unchecked_calls: row.get::<u32>(18)?,
        assertions: row.get::<u32>(19)?,
        updated_at: row.get::<u64>(20)?,
        parent_id: None,
    })
}

fn get_string_lossy(
    row: &tracedecay_runtime_core::db::engine::Row,
    idx: i32,
) -> std::result::Result<String, tracedecay_runtime_core::db::engine::Error> {
    let val = row.get::<tracedecay_runtime_core::db::engine::Value>(idx)?;
    match val {
        tracedecay_runtime_core::db::engine::Value::Text(s) => Ok(s),
        tracedecay_runtime_core::db::engine::Value::Blob(bytes) => {
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        }
        tracedecay_runtime_core::db::engine::Value::Null => Ok(String::new()),
        tracedecay_runtime_core::db::engine::Value::Integer(i) => Ok(i.to_string()),
        tracedecay_runtime_core::db::engine::Value::Real(f) => Ok(f.to_string()),
    }
}

fn get_opt_string_lossy(
    row: &tracedecay_runtime_core::db::engine::Row,
    idx: i32,
) -> std::result::Result<Option<String>, tracedecay_runtime_core::db::engine::Error> {
    let val = row.get::<tracedecay_runtime_core::db::engine::Value>(idx)?;
    match val {
        tracedecay_runtime_core::db::engine::Value::Text(s) => Ok(Some(s)),
        tracedecay_runtime_core::db::engine::Value::Blob(bytes) => {
            Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
        }
        tracedecay_runtime_core::db::engine::Value::Null => Ok(None),
        tracedecay_runtime_core::db::engine::Value::Integer(i) => Ok(Some(i.to_string())),
        tracedecay_runtime_core::db::engine::Value::Real(f) => Ok(Some(f.to_string())),
    }
}

impl<'a> GraphQueryManager<'a> {
    /// Creates a new `GraphQueryManager` backed by the given database.
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Finds nodes with zero incoming edges, indicating potentially dead code.
    ///
    /// Excludes:
    /// - Nodes named `"main"` (program entry points).
    /// - Nodes whose name starts with `"test"` (likely test functions).
    /// - Nodes annotated with a test-marker attribute — `#[test]`,
    ///   `#[tokio::test]`, `#[async_std::test]`, `#[wasm_bindgen_test]`, etc.
    ///   The libtest harness is the implicit caller of these and never shows
    ///   up as a graph edge, so without this filter most Rust tests with
    ///   non-`test*` names get misreported as dead.
    /// - By default, `pub` items (they may be part of a public API). Pass
    ///   `include_public=true` to drop this exclusion — useful when
    ///   auditing a workspace where most items are `pub` but only a subset
    ///   actually have callers in the indexed scope. Without the flag,
    ///   `pub`-heavy codebases were reporting 0 dead symbols.
    ///
    /// If `kinds` is non-empty, only nodes of the specified kinds are checked.
    pub async fn find_dead_code(
        &self,
        kinds: &[NodeKind],
        include_public: bool,
        limit: Option<usize>,
    ) -> Result<Vec<Node>> {
        let transaction = self.db.begin_write_transaction("find dead code").await?;
        let kind_filter = if kinds.is_empty() {
            String::new()
        } else {
            let kind_strs: Vec<String> =
                kinds.iter().map(|k| format!("'{}'", k.as_str())).collect();
            format!(" AND kind IN ({})", kind_strs.join(", "))
        };
        let visibility_filter = if include_public {
            ""
        } else {
            " AND visibility != 'public'"
        };

        // Only true "use sites" count as evidence that a symbol is alive:
        // `calls`, `implements`, `extends`, `type_of`, `returns`, `receives`,
        // `uses`. Bookkeeping edges (`contains`, `annotates`, `derives_macro`)
        // are emitted by the extractor even for unused code — every Rust
        // function tagged `#[inline]`, every `#[derive(Debug)]` on an unused
        // struct, every annotation_usage adds an incoming edge that is NOT a
        // real reference. Previously we excluded only `contains`, so any
        // attribute on an otherwise-unused function masked it from
        // dead-code detection — which is why the sonium run reported zero
        // dead functions across 5,715. The narrower allowlist below restores
        // the intended semantics: "no real caller / referencer = dead".
        // Test-marker exclusion uses a THREE-step "resolve + pre-join + probe":
        //   1) `collect_test_marker_ids` runs the leading-wildcard `LIKE`
        //      scan exactly once over `kind = 'annotation_usage'`.
        //   2) Those ids land in `temp.test_markers` (PK on `id`).
        //   3) `temp.test_annotated_targets` is built from
        //      `edges WHERE kind='annotates' AND source IN temp.test_markers`
        //      — i.e., "which node ids are annotated by ANY test marker."
        //      ~15 K rows on chromium.
        //   4) The dead-code SELECT then uses
        //      `nodes.id NOT IN (SELECT target FROM temp.test_annotated_targets)`
        //      — a single PK probe per candidate against a small table.
        //
        // History — DO NOT regress this:
        // - The pre-4.14.8 form joined `nodes a ON a.id = e2.source` inside
        //   the correlated `NOT EXISTS` and ran the `a.name LIKE '%::test'`
        //   chain per matching edge. Fast on scirs (0.1 s, 76 K
        //   annotation_usage) but timed out on chromium at the 25 s probe
        //   ceiling, cascade-poisoning every subsequent MCP tool call.
        // - 4.14.8's `WITH test_marker_ids AS (...)` CTE attempt was even
        //   worse: SQLite inlined the single-reference CTE, turning every
        //   dead-candidate row into a full annotation_usage scan. Regressed
        //   scirs from 0.1 s to >60 s, reverted in 4.14.9.
        // - 4.14.12's first attempt (single temp table + correlated
        //   `IN (SELECT id FROM temp.test_markers)` inside `NOT EXISTS`)
        //   also failed on chromium: SQLite picked `idx_edges_unique
        //   (source, target, kind)` for the subquery and iterated every
        //   marker as the outer driver for every candidate
        //   (~13K markers × ~134K candidates ≈ 1.7B probes). Timed out.
        // - The current pre-join form (two temp tables, indexed `NOT IN`)
        //   measures 0.75 s on chromium and 0.6 s on scirs. The
        //   wildcard scan still runs exactly once per call; the per-row
        //   probe is now against a ~15K-row indexed lookup table, not a
        //   correlated subquery the optimiser can re-shape.
        let result = self
            .find_dead_code_inner(&transaction, visibility_filter, &kind_filter, limit)
            .await;

        // Always drop both temp tables, even on the error path, so a
        // failed query does not leak rows to the next caller on the same
        // connection. Best-effort: a drop failure shouldn't mask the
        // original error.
        let _ = self
            .db
            .drop_test_annotated_targets_temp_table_unlocked(&transaction)
            .await;
        let _ = self
            .db
            .drop_test_marker_temp_table_unlocked(&transaction)
            .await;
        let _ = transaction.rollback().await;
        result
    }

    /// Body of `find_dead_code`. Split out so the caller can wrap us in a
    /// guaranteed `drop_*_temp_table()` even on the error path.
    async fn find_dead_code_inner(
        &self,
        connection: &DatabaseWriteTransaction<'_>,
        visibility_filter: &str,
        kind_filter: &str,
        limit: Option<usize>,
    ) -> Result<Vec<Node>> {
        let marker_ids = self.db.collect_test_marker_ids_on(connection).await?;
        let test_annotated_targets_filter = if marker_ids.is_empty() {
            ""
        } else {
            self.db
                .populate_test_marker_temp_table_unlocked(connection, &marker_ids)
                .await?;
            self.db
                .populate_test_annotated_targets_temp_table_unlocked(connection)
                .await?;
            "AND id NOT IN (SELECT target FROM temp.test_annotated_targets)"
        };

        let limit_clause = limit.map_or_else(String::new, |limit| format!("LIMIT {limit}"));
        let sql = format!(
            "SELECT id, kind, name, qualified_name, file_path, start_line, end_line,
                    start_column, end_column, docstring, signature, visibility,
                    is_async, branches, loops, returns, max_nesting, unsafe_blocks,
                    unchecked_calls, assertions, updated_at, attrs_start_line
             FROM nodes
             WHERE name != 'main'
             AND name NOT LIKE 'test%'
             {visibility_filter}
             {kind_filter}
             AND NOT EXISTS (
                 SELECT 1 FROM edges
                 WHERE target = nodes.id
                 AND kind IN ('calls', 'implements', 'extends', 'type_of', 'returns', 'receives', 'uses')
             )
             {test_annotated_targets_filter}
             ORDER BY file_path ASC, start_line ASC, id ASC
             {limit_clause}"
        );

        let mut rows =
            connection
                .query_engine(&sql, ())
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to find dead code: {e}"),
                    operation: "find_dead_code".to_string(),
                })?;

        let mut dead = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read row: {e}"),
            operation: "find_dead_code".to_string(),
        })? {
            let node = row_to_node_dead_code(&row).map_err(|error| TraceDecayError::Database {
                message: format!("failed to decode dead-code node: {error}"),
                operation: "find_dead_code".to_owned(),
            })?;
            dead.push(node);
        }
        Ok(dead)
    }

    /// Computes metrics for a single node describing its graph connectivity.
    pub async fn get_node_metrics(&self, node_id: &str) -> Result<NodeMetrics> {
        let incoming = self.db.get_incoming_edges(node_id, &[]).await?;
        let outgoing = self.db.get_outgoing_edges(node_id, &[]).await?;

        let caller_count = incoming
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .count();
        let call_count = outgoing
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .count();
        // Children come from parent_id after v9, not from Contains edges.
        let child_count = self.db.get_children_of(node_id).await?.len();

        // Compute depth by walking up the containment hierarchy.
        let depth = self.compute_depth(node_id).await?;

        Ok(NodeMetrics {
            incoming_edge_count: incoming.len(),
            outgoing_edge_count: outgoing.len(),
            call_count,
            caller_count,
            child_count,
            depth,
        })
    }

    /// Gets the file paths that the given file depends on.
    ///
    /// Examines outgoing `Uses` and `Calls` edges from all nodes in the
    /// specified file. Returns the deduplicated set of target file paths,
    /// excluding the source file itself.
    pub async fn get_file_dependencies(&self, file_path: &str) -> Result<Vec<String>> {
        let nodes = self.db.get_nodes_by_file(file_path).await?;
        if nodes.is_empty() {
            return Ok(Vec::new());
        }

        let node_ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
        let placeholders: Vec<String> = (1..=node_ids.len()).map(|i| format!("?{i}")).collect();
        let kind_filter = "('uses', 'calls')";

        let sql = format!(
            "SELECT DISTINCT e.target FROM edges e \
             WHERE e.source IN ({}) AND e.kind IN {kind_filter}",
            placeholders.join(", ")
        );

        let param_values: Vec<tracedecay_runtime_core::db::engine::Value> = node_ids
            .iter()
            .map(|id| tracedecay_runtime_core::db::engine::Value::Text(id.clone()))
            .collect();

        let mut rows = self
            .db
            .engine_conn()
            .query(
                &sql,
                tracedecay_runtime_core::db::engine::params_from_iter(param_values),
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query file dependencies: {e}"),
                operation: "get_file_dependencies".to_string(),
            })?;

        let mut target_ids: Vec<String> = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read target id: {e}"),
            operation: "get_file_dependencies".to_string(),
        })? {
            if let Ok(id) = row.get::<String>(0) {
                target_ids.push(id);
            }
        }

        if target_ids.is_empty() {
            return Ok(Vec::new());
        }

        let target_nodes = self.db.get_nodes_by_ids(&target_ids).await?;
        let dep_files: HashSet<String> = target_nodes
            .into_iter()
            .filter(|n| n.file_path != file_path)
            .map(|n| n.file_path)
            .collect();

        let mut result: Vec<String> = dep_files.into_iter().collect();
        result.sort();
        Ok(result)
    }

    /// Gets the file paths that depend on the given file.
    ///
    /// Examines incoming `Uses` and `Calls` edges to all nodes in the
    /// specified file. Returns the deduplicated set of source file paths,
    /// excluding the target file itself.
    pub async fn get_file_dependents(&self, file_path: &str) -> Result<Vec<String>> {
        let nodes = self.db.get_nodes_by_file(file_path).await?;
        if nodes.is_empty() {
            return Ok(Vec::new());
        }

        let node_ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
        let placeholders: Vec<String> = (1..=node_ids.len()).map(|i| format!("?{i}")).collect();
        let kind_filter = "('uses', 'calls')";

        let sql = format!(
            "SELECT DISTINCT e.source FROM edges e \
             WHERE e.target IN ({}) AND e.kind IN {kind_filter}",
            placeholders.join(", ")
        );

        let param_values: Vec<tracedecay_runtime_core::db::engine::Value> = node_ids
            .iter()
            .map(|id| tracedecay_runtime_core::db::engine::Value::Text(id.clone()))
            .collect();

        let mut rows = self
            .db
            .engine_conn()
            .query(
                &sql,
                tracedecay_runtime_core::db::engine::params_from_iter(param_values),
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to query file dependents: {e}"),
                operation: "get_file_dependents".to_string(),
            })?;

        let mut source_ids: Vec<String> = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to read source id: {e}"),
            operation: "get_file_dependents".to_string(),
        })? {
            if let Ok(id) = row.get::<String>(0) {
                source_ids.push(id);
            }
        }

        if source_ids.is_empty() {
            return Ok(Vec::new());
        }

        let source_nodes = self.db.get_nodes_by_ids(&source_ids).await?;
        let dependent_files: HashSet<String> = source_nodes
            .into_iter()
            .filter(|n| n.file_path != file_path)
            .map(|n| n.file_path)
            .collect();

        let mut result: Vec<String> = dependent_files.into_iter().collect();
        result.sort();
        Ok(result)
    }

    /// Detects circular dependencies at the file level.
    ///
    /// Builds a file-level dependency graph and groups files into
    /// strongly-connected components via Tarjan's algorithm. Returns one
    /// component per mutually-recursive group of files; trivial
    /// single-file components without self-loops are filtered out (they
    /// aren't cycles). Replaces the prior walk-enumeration approach that
    /// reported 73 overlapping "cycles" on real codebases — each just a
    /// different DFS path through the same component.
    pub async fn find_circular_dependencies(&self) -> Result<Vec<Vec<String>>> {
        // `build_file_adjacency` computes the same relation — distinct
        // (source file, target file) pairs over `calls` and `uses` edges,
        // self-edges excluded, every known file present as a key — through
        // keyset pages instead of one query round trip per file in the
        // project. Building it a file at a time cost thousands of sequential
        // round trips on the request path for an identical result.
        let adj = self.build_file_adjacency(None).await?;

        let sccs = super::scc::tarjan_scc(&adj);
        let mut cycles: Vec<Vec<String>> = sccs
            .into_iter()
            .filter(|s| super::scc::is_cyclic_scc(s, &adj))
            .collect();
        // Within each cycle, sort file paths so the output is stable
        // across runs (Tarjan's stack order is implementation-dependent).
        for cycle in &mut cycles {
            cycle.sort_unstable();
        }
        Ok(cycles)
    }

    /// Builds a file-level directed adjacency map from the code graph.
    ///
    /// For each file, collects the files it depends on via `calls` and
    /// `uses` (imports) edges. Self-edges are excluded. `implements` and
    /// `extends` are intentionally **not** followed: the Rust resolver
    /// fuzzy-binds `impl Debug for T` and similar to whatever node happens
    /// to share the trait's short name, which on real codebases produces
    /// long chains of spurious file-to-file dependencies (one bug report
    /// hit 19 levels through a chain of unrelated files terminating in a
    /// foreign crate's `lib.rs`).
    ///
    /// When `path_prefix` is `Some`, only files under that prefix are included
    /// (both as sources and targets).
    pub async fn build_file_adjacency(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<HashMap<String, HashSet<String>>> {
        // Read the distinct pairs through keyset pages: the whole join exceeds
        // what the SQLite runtime will materialize for a single query on a real
        // project. Every page is aggregated into the same adjacency map, so the
        // result stays a complete measurement.
        let sql = "SELECT DISTINCT n1.file_path AS src_file, n2.file_path AS tgt_file \
                   FROM edges e \
                   JOIN nodes n1 ON e.source = n1.id \
                   JOIN nodes n2 ON e.target = n2.id \
                   WHERE e.kind IN ('calls', 'uses') \
                   AND n1.file_path != n2.file_path \
                   AND (n1.file_path > ?1 OR (n1.file_path = ?1 AND n2.file_path > ?2)) \
                   ORDER BY src_file, tgt_file \
                   LIMIT ?3";

        // Normalise the prefix once: ensure it ends with '/'.
        let prefix: Option<String> = path_prefix.map(|p| {
            if p.ends_with('/') {
                p.to_string()
            } else {
                format!("{p}/")
            }
        });

        let mut adj: HashMap<String, HashSet<String>> = HashMap::new();
        let mut cursor = (String::new(), String::new());
        loop {
            let mut rows = self
                .db
                .conn()
                .query(
                    sql,
                    (cursor.0.clone(), cursor.1.clone(), ADJACENCY_SCAN_PAGE_ROWS),
                )
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to query file adjacency: {e}"),
                    operation: "build_file_adjacency".to_string(),
                })?;

            let mut page_rows = 0_i64;
            while let Some(row) = rows.next().await.map_err(|e| TraceDecayError::Database {
                message: format!("failed to read adjacency row: {e}"),
                operation: "build_file_adjacency".to_string(),
            })? {
                let src: String = row.get(0).unwrap_or_default();
                let tgt: String = row.get(1).unwrap_or_default();
                cursor = (src.clone(), tgt.clone());
                page_rows += 1;

                if let Some(ref pfx) = prefix
                    && (!src.starts_with(pfx.as_str()) || !tgt.starts_with(pfx.as_str()))
                {
                    continue;
                }

                adj.entry(src).or_default().insert(tgt);
            }
            drop(rows);
            if page_rows < ADJACENCY_SCAN_PAGE_ROWS {
                break;
            }
        }

        // Ensure every known file appears as a key (even leaf nodes with no deps).
        let all_files = self.db.get_all_files().await?;
        for file in all_files {
            if let Some(ref pfx) = prefix
                && !file.path.starts_with(pfx.as_str())
            {
                continue;
            }
            adj.entry(file.path).or_default();
        }

        Ok(adj)
    }

    /// Builds the file dependency adjacency while enforcing hard response-path
    /// budgets. The extra row requested from each query proves whether the
    /// result exceeded its budget; over-budget reads fail instead of returning
    /// a partial graph that could be mistaken for a complete measurement.
    pub async fn build_file_adjacency_bounded(
        &self,
        max_files: usize,
        max_dependency_edges: usize,
    ) -> Result<FileAdjacencyScan> {
        let file_limit = i64::try_from(max_files.saturating_add(1)).unwrap_or(i64::MAX);
        let mut file_rows = self
            .db
            .conn()
            .query(
                "SELECT path FROM files ORDER BY path LIMIT ?1",
                [file_limit],
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to query bounded file set: {error}"),
                operation: "build_file_adjacency_bounded".to_string(),
            })?;
        let mut files = Vec::new();
        while let Some(row) = file_rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to read bounded file row: {error}"),
                operation: "build_file_adjacency_bounded".to_string(),
            })?
        {
            files.push(
                row.get::<String>(0)
                    .map_err(|error| TraceDecayError::Database {
                        message: format!("invalid bounded file row: {error}"),
                        operation: "build_file_adjacency_bounded".to_string(),
                    })?,
            );
        }
        if files.len() > max_files {
            return Err(TraceDecayError::Config {
                message: format!(
                    "file adjacency exceeds the {max_files}-file dashboard scan budget"
                ),
            });
        }

        let edge_limit = i64::try_from(max_dependency_edges.saturating_add(1)).unwrap_or(i64::MAX);
        let mut edge_rows = self
            .db
            .conn()
            .query(
                "SELECT DISTINCT n1.file_path AS src_file, n2.file_path AS tgt_file
                 FROM edges e
                 JOIN nodes n1 ON e.source = n1.id
                 JOIN nodes n2 ON e.target = n2.id
                 WHERE e.kind IN ('calls', 'uses')
                   AND n1.file_path != n2.file_path
                 LIMIT ?1",
                [edge_limit],
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to query bounded file adjacency: {error}"),
                operation: "build_file_adjacency_bounded".to_string(),
            })?;
        let mut dependencies = Vec::new();
        while let Some(row) = edge_rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to read bounded adjacency row: {error}"),
                operation: "build_file_adjacency_bounded".to_string(),
            })?
        {
            let source = row
                .get::<String>(0)
                .map_err(|error| TraceDecayError::Database {
                    message: format!("invalid bounded adjacency source: {error}"),
                    operation: "build_file_adjacency_bounded".to_string(),
                })?;
            let target = row
                .get::<String>(1)
                .map_err(|error| TraceDecayError::Database {
                    message: format!("invalid bounded adjacency target: {error}"),
                    operation: "build_file_adjacency_bounded".to_string(),
                })?;
            dependencies.push((source, target));
        }
        if dependencies.len() > max_dependency_edges {
            return Err(TraceDecayError::Config {
                message: format!(
                    "file adjacency exceeds the {max_dependency_edges}-edge dashboard scan budget"
                ),
            });
        }

        let mut adjacency: HashMap<String, HashSet<String>> = files
            .iter()
            .cloned()
            .map(|path| (path, HashSet::new()))
            .collect();
        for (source, target) in &dependencies {
            adjacency
                .entry(source.clone())
                .or_default()
                .insert(target.clone());
            adjacency.entry(target.clone()).or_default();
        }

        Ok(FileAdjacencyScan {
            adjacency,
            files_examined: files.len(),
            dependency_edges_examined: dependencies.len(),
        })
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Computes the depth of a node in the containment hierarchy by walking
    /// up incoming `Contains` edges.
    async fn compute_depth(&self, node_id: &str) -> Result<usize> {
        const MAX_DEPTH: usize = 100;
        let mut depth: usize = 0;
        let mut current_id = node_id.to_string();
        let mut visited: HashSet<String> = HashSet::new();

        while depth < MAX_DEPTH {
            if visited.contains(&current_id) {
                break;
            }
            visited.insert(current_id.clone());

            // parent_id is the v9 truth for containment.
            let Some(node) = self.db.get_node_by_id(&current_id).await? else {
                break;
            };
            match node.parent_id {
                Some(parent) => {
                    current_id = parent;
                    depth += 1;
                }
                None => break,
            }
        }

        Ok(depth)
    }
}

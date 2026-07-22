use std::collections::BTreeMap;

use rusqlite::{Transaction, params, types::Type};
use tracedecay_store::{
    ConsistencyModeV1, FrozenWatermarkCoverageV1, FrozenWatermarkVectorV1, GraphNodeV1,
    GraphSearchResultV1, GraphSearchScoreV1, GraphStatsV1, RuntimeReadCoverageV1,
    RuntimeReadOperationV1, RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeReadResultV1,
    ShardWatermarkV1, StorageRuntimeErrorV1, UnavailableReasonV1, WatermarkCoverageStatusV1,
};

use crate::reader::ReaderQueryExecutor;

use super::CodeShardAccessV1;

const NODE_COLUMNS: &str = "
    id, kind, name, qualified_name, file_path,
    start_line, end_line, start_column, end_column,
    docstring, signature, visibility, is_async, branches, loops, returns,
    max_nesting, unsafe_blocks, unchecked_calls, assertions, updated_at,
    attrs_start_line, parent_id";
const QUALIFIED_NODE_COLUMNS: &str = "
    nodes.id, nodes.kind, nodes.name, nodes.qualified_name, nodes.file_path,
    nodes.start_line, nodes.end_line, nodes.start_column, nodes.end_column,
    nodes.docstring, nodes.signature, nodes.visibility, nodes.is_async,
    nodes.branches, nodes.loops, nodes.returns, nodes.max_nesting,
    nodes.unsafe_blocks, nodes.unchecked_calls, nodes.assertions, nodes.updated_at,
    nodes.attrs_start_line, nodes.parent_id";

#[derive(Clone, Copy, Debug)]
pub struct GraphReaderExecutor {
    access: CodeShardAccessV1,
}

impl GraphReaderExecutor {
    pub const fn new(access: CodeShardAccessV1) -> Self {
        Self { access }
    }
}

impl ReaderQueryExecutor for GraphReaderExecutor {
    fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        request: &RuntimeReadRequestV1,
    ) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
        let coverage = read_coverage(snapshot, request, self.access)?;
        if !coverage_allows_value(&coverage) {
            return outcome(None, coverage);
        }
        let value = match request.operation() {
            RuntimeReadOperationV1::GraphStats => RuntimeReadResultV1::GraphStats {
                stats: graph_stats(snapshot)?,
            },
            RuntimeReadOperationV1::GraphNode { node_id } => RuntimeReadResultV1::GraphNode {
                node: graph_node(snapshot, node_id)?,
            },
            RuntimeReadOperationV1::GraphSearch { query, limit } => {
                RuntimeReadResultV1::GraphSearch {
                    results: graph_search(snapshot, query, *limit)?,
                }
            }
            RuntimeReadOperationV1::GraphQuickCheck => RuntimeReadResultV1::GraphQuickCheck {
                healthy: graph_quick_check(snapshot)?,
            },
            _ => {
                return Err(infrastructure(
                    "graph reader received a non-graph read operation",
                ));
            }
        };
        outcome(Some(value), coverage)
    }
}

fn outcome(
    value: Option<RuntimeReadResultV1>,
    coverage: RuntimeReadCoverageV1,
) -> Result<RuntimeReadOutcomeV1, StorageRuntimeErrorV1> {
    RuntimeReadOutcomeV1::new(value, coverage)
        .map_err(|error| infrastructure(format!("construct graph read outcome: {error}")))
}

fn coverage_allows_value(coverage: &RuntimeReadCoverageV1) -> bool {
    matches!(
        coverage,
        RuntimeReadCoverageV1::Latest { .. }
            | RuntimeReadCoverageV1::Complete { .. }
            | RuntimeReadCoverageV1::Partial { .. }
    )
}

fn read_coverage(
    snapshot: &Transaction<'_>,
    request: &RuntimeReadRequestV1,
    access: CodeShardAccessV1,
) -> Result<RuntimeReadCoverageV1, StorageRuntimeErrorV1> {
    match request.consistency() {
        ConsistencyModeV1::LatestAvailable => Ok(RuntimeReadCoverageV1::Latest {
            observed: current_watermark(snapshot, request)?,
        }),
        ConsistencyModeV1::ExactSnapshot { .. }
            if access != CodeShardAccessV1::ImmutableSnapshot =>
        {
            Ok(RuntimeReadCoverageV1::Unavailable {
                coverage: None,
                reason: UnavailableReasonV1::SnapshotNotRetained,
            })
        }
        ConsistencyModeV1::ExactSnapshot { lease } => {
            let required = FrozenWatermarkVectorV1::new([lease.watermark.clone()])
                .map_err(|error| infrastructure(format!("build exact graph coverage: {error}")))?;
            let coverage = FrozenWatermarkCoverageV1::new(required, [lease.watermark.clone()])
                .map_err(|error| {
                    infrastructure(format!("observe exact graph coverage: {error}"))
                })?;
            Ok(RuntimeReadCoverageV1::Complete { coverage })
        }
        ConsistencyModeV1::AtLeast { commit_sequence } => {
            let required = FrozenWatermarkVectorV1::new([ShardWatermarkV1 {
                shard_id: request.binding().shard_id.clone(),
                incarnation: request.binding().incarnation,
                authority_epoch: request.binding().authority_epoch,
                commit_sequence: *commit_sequence,
            }])
            .map_err(|error| infrastructure(format!("build graph coverage: {error}")))?;
            classify_coverage(
                required,
                current_watermark(snapshot, request)?.into_iter().collect(),
            )
        }
        ConsistencyModeV1::FrozenWatermarkVector { vector } => {
            let observed = current_watermark(snapshot, request)?
                .filter(|watermark| vector.get(&watermark.shard_id).is_some())
                .into_iter()
                .collect();
            classify_coverage(vector.clone(), observed)
        }
    }
}

fn current_watermark(
    snapshot: &Transaction<'_>,
    request: &RuntimeReadRequestV1,
) -> Result<Option<ShardWatermarkV1>, StorageRuntimeErrorV1> {
    let table_exists = snapshot
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sqlite_schema
                 WHERE type = 'table' AND name = 'td_runtime_writer_checkpoint_v1'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(sqlite("inspect graph writer checkpoint schema"))?;
    if !table_exists {
        return Ok(None);
    }
    let shard_json = serde_json::to_string(&request.binding().shard_id)
        .map_err(|error| infrastructure(format!("encode graph shard identity: {error}")))?;
    let incarnation = i64::try_from(request.binding().incarnation.get())
        .map_err(|_| infrastructure("graph incarnation exceeds SQLite INTEGER"))?;
    let mut statement = snapshot
        .prepare(
            "SELECT watermark_json
             FROM td_runtime_writer_checkpoint_v1
             WHERE shard_json = ?1 AND incarnation = ?2",
        )
        .map_err(sqlite("prepare graph writer checkpoint query"))?;
    let mut rows = statement
        .query(params![shard_json, incarnation])
        .map_err(sqlite("query graph writer checkpoint"))?;
    let Some(row) = rows
        .next()
        .map_err(sqlite("read graph writer checkpoint"))?
    else {
        return Ok(None);
    };
    let raw = row
        .get::<_, String>(0)
        .map_err(sqlite("decode graph writer checkpoint value"))?;
    let watermark = serde_json::from_str::<ShardWatermarkV1>(&raw)
        .map_err(|error| infrastructure(format!("decode graph writer watermark: {error}")))?;
    if watermark.shard_id != request.binding().shard_id
        || watermark.incarnation != request.binding().incarnation
        || watermark.authority_epoch != request.binding().authority_epoch
    {
        return Ok(None);
    }
    Ok(Some(watermark))
}

fn classify_coverage(
    required: FrozenWatermarkVectorV1,
    observed: Vec<ShardWatermarkV1>,
) -> Result<RuntimeReadCoverageV1, StorageRuntimeErrorV1> {
    let coverage = FrozenWatermarkCoverageV1::new(required, observed)
        .map_err(|error| infrastructure(format!("classify graph coverage: {error}")))?;
    if coverage.is_complete() {
        return Ok(RuntimeReadCoverageV1::Complete { coverage });
    }
    if coverage.is_partial() {
        return Ok(RuntimeReadCoverageV1::Partial { coverage });
    }
    if coverage
        .required
        .iter()
        .any(|(shard, _)| coverage.status_for(shard) == WatermarkCoverageStatusV1::Stale)
    {
        return Ok(RuntimeReadCoverageV1::Stale { coverage });
    }
    Ok(RuntimeReadCoverageV1::Unavailable {
        coverage: Some(coverage),
        reason: UnavailableReasonV1::WatermarkNotReached,
    })
}

fn graph_node(
    snapshot: &Transaction<'_>,
    node_id: &str,
) -> Result<Option<GraphNodeV1>, StorageRuntimeErrorV1> {
    let sql = format!("SELECT {NODE_COLUMNS} FROM nodes WHERE id = ?1");
    let mut statement = snapshot
        .prepare(&sql)
        .map_err(sqlite("prepare graph node query"))?;
    let mut rows = statement
        .query(params![node_id])
        .map_err(sqlite("query graph node"))?;
    rows.next()
        .map_err(sqlite("read graph node"))?
        .map(row_to_graph_node)
        .transpose()
        .map_err(sqlite("map graph node"))
}

fn graph_search(
    snapshot: &Transaction<'_>,
    query: &str,
    limit: u32,
) -> Result<Vec<GraphSearchResultV1>, StorageRuntimeErrorV1> {
    let results = if table_exists(snapshot, "nodes_fts")? {
        let fts_query = fts_literal_query(query);
        graph_fts_search(snapshot, &fts_query, limit)?
    } else {
        Vec::new()
    };
    if !results.is_empty() {
        return Ok(results);
    }
    graph_like_search(snapshot, query, limit)
}

fn graph_fts_search(
    snapshot: &Transaction<'_>,
    query: &str,
    limit: u32,
) -> Result<Vec<GraphSearchResultV1>, StorageRuntimeErrorV1> {
    let sql = format!(
        "SELECT {QUALIFIED_NODE_COLUMNS},
                bm25(nodes_fts, 10.0, 5.0, 1.0, 2.0) AS rank
         FROM nodes_fts
         JOIN nodes ON nodes_fts.rowid = nodes.rowid
         WHERE nodes_fts MATCH ?1
         ORDER BY rank, nodes.id
         LIMIT ?2"
    );
    let mut statement = snapshot
        .prepare(&sql)
        .map_err(sqlite("prepare graph FTS query"))?;
    let rows = statement
        .query_map(params![query, i64::from(limit)], |row| {
            let node = row_to_graph_node(row)?;
            let rank = row.get::<_, f64>(23)?;
            let score = GraphSearchScoreV1::new(-rank)
                .map_err(|error| rusqlite::Error::InvalidParameterName(error.to_string()))?;
            Ok(GraphSearchResultV1 { node, score })
        })
        .map_err(sqlite("query graph FTS"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sqlite("read graph FTS results"))
}

fn graph_like_search(
    snapshot: &Transaction<'_>,
    query: &str,
    limit: u32,
) -> Result<Vec<GraphSearchResultV1>, StorageRuntimeErrorV1> {
    let sql = format!(
        "SELECT {NODE_COLUMNS}
         FROM nodes
         WHERE name LIKE ?1 ESCAPE '\\'
            OR qualified_name LIKE ?1 ESCAPE '\\'
            OR docstring LIKE ?1 ESCAPE '\\'
            OR signature LIKE ?1 ESCAPE '\\'
         ORDER BY id
         LIMIT ?2"
    );
    let pattern = format!("%{}%", escape_like(query));
    let mut statement = snapshot
        .prepare(&sql)
        .map_err(sqlite("prepare graph LIKE query"))?;
    let score = GraphSearchScoreV1::new(1.0)
        .map_err(|error| infrastructure(format!("build graph LIKE score: {error}")))?;
    let rows = statement
        .query_map(params![pattern, i64::from(limit)], |row| {
            Ok(GraphSearchResultV1 {
                node: row_to_graph_node(row)?,
                score,
            })
        })
        .map_err(sqlite("query graph LIKE fallback"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sqlite("read graph LIKE results"))
}

fn fts_literal_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn escape_like(query: &str) -> String {
    query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn graph_quick_check(snapshot: &Transaction<'_>) -> Result<bool, StorageRuntimeErrorV1> {
    let mut statement = snapshot
        .prepare("PRAGMA quick_check")
        .map_err(sqlite("prepare graph quick_check"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sqlite("run graph quick_check"))?;
    let values = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sqlite("read graph quick_check"))?;
    Ok(values.len() == 1 && values[0].eq_ignore_ascii_case("ok"))
}

fn graph_stats(snapshot: &Transaction<'_>) -> Result<GraphStatsV1, StorageRuntimeErrorV1> {
    let (node_count, edge_count, file_count, last_updated, total_source_bytes) = snapshot
        .query_row(
            "SELECT
               (SELECT COUNT(*) FROM nodes),
               (SELECT COUNT(*) FROM edges),
               (SELECT COUNT(*) FROM files),
               (SELECT COALESCE(MAX(indexed_at), 0) FROM files),
               (SELECT COALESCE(SUM(size), 0) FROM files)",
            [],
            |row| {
                Ok((
                    nonnegative(row.get(0)?),
                    nonnegative(row.get(1)?),
                    nonnegative(row.get(2)?),
                    nonnegative(row.get(3)?),
                    nonnegative(row.get(4)?),
                ))
            },
        )
        .map_err(sqlite("query graph scalar statistics"))?;
    let nodes_by_kind = grouped_counts(snapshot, "nodes")?;
    let edges_by_kind = grouped_counts(snapshot, "edges")?;
    let db_size_bytes = snapshot
        .query_row(
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(nonnegative)
        .map_err(sqlite("query graph database size"))?;
    let files_by_language = file_language_counts(snapshot)?;
    Ok(GraphStatsV1 {
        node_count,
        edge_count,
        file_count,
        nodes_by_kind,
        edges_by_kind,
        db_size_bytes,
        last_updated,
        total_source_bytes,
        files_by_language,
        last_sync_at: metadata_u64(snapshot, "last_sync_at")?,
        last_full_sync_at: metadata_u64(snapshot, "last_full_sync_at")?,
        last_sync_duration_ms: metadata_u64(snapshot, "last_sync_duration_ms")?,
    })
}

fn grouped_counts(
    snapshot: &Transaction<'_>,
    table: &'static str,
) -> Result<BTreeMap<String, u64>, StorageRuntimeErrorV1> {
    let sql = match table {
        "nodes" => "SELECT kind, COUNT(*) FROM nodes GROUP BY kind ORDER BY kind",
        "edges" => "SELECT kind, COUNT(*) FROM edges GROUP BY kind ORDER BY kind",
        _ => unreachable!("closed graph statistics table"),
    };
    let mut statement = snapshot
        .prepare(sql)
        .map_err(sqlite("prepare grouped graph statistics"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, nonnegative(row.get(1)?)))
        })
        .map_err(sqlite("query grouped graph statistics"))?;
    rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()
        .map_err(sqlite("read grouped graph statistics"))
}

fn file_language_counts(
    snapshot: &Transaction<'_>,
) -> Result<BTreeMap<String, u64>, StorageRuntimeErrorV1> {
    let mut statement = snapshot
        .prepare("SELECT path FROM files ORDER BY path")
        .map_err(sqlite("prepare graph file statistics"))?;
    let paths = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sqlite("query graph file statistics"))?;
    let mut counts = BTreeMap::new();
    for path in paths {
        let path = path.map_err(sqlite("read graph file statistics"))?;
        *counts
            .entry(display_language_for_path(&path).to_owned())
            .or_insert(0) += 1;
    }
    Ok(counts)
}

fn metadata_u64(snapshot: &Transaction<'_>, key: &str) -> Result<u64, StorageRuntimeErrorV1> {
    let mut statement = snapshot
        .prepare("SELECT value FROM metadata WHERE key = ?1")
        .map_err(sqlite("prepare graph metadata query"))?;
    let mut rows = statement
        .query(params![key])
        .map_err(sqlite("query graph metadata"))?;
    let Some(row) = rows.next().map_err(sqlite("read graph metadata"))? else {
        return Ok(0);
    };
    Ok(row
        .get::<_, String>(0)
        .map_err(sqlite("decode graph metadata"))?
        .parse()
        .unwrap_or(0))
}

fn table_exists(snapshot: &Transaction<'_>, table: &str) -> Result<bool, StorageRuntimeErrorV1> {
    snapshot
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1
             )",
            params![table],
            |row| row.get(0),
        )
        .map_err(sqlite("inspect graph table"))
}

pub(super) fn row_to_graph_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphNodeV1> {
    let start_line = row.get::<_, u32>(5)?;
    let node = GraphNodeV1 {
        id: row.get(0)?,
        kind: row.get(1)?,
        name: row.get(2)?,
        qualified_name: row.get(3)?,
        file_path: row.get(4)?,
        start_line,
        attrs_start_line: row.get::<_, Option<u32>>(21)?.unwrap_or(start_line),
        end_line: row.get(6)?,
        start_column: row.get(7)?,
        end_column: row.get(8)?,
        docstring: row.get(9)?,
        signature: row.get(10)?,
        visibility: row.get(11)?,
        is_async: row.get::<_, i64>(12)? != 0,
        branches: row.get(13)?,
        loops: row.get(14)?,
        returns: row.get(15)?,
        max_nesting: row.get(16)?,
        unsafe_blocks: row.get(17)?,
        unchecked_calls: row.get(18)?,
        assertions: row.get(19)?,
        updated_at: u64::try_from(row.get::<_, i64>(20)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(20, Type::Integer, Box::new(error))
        })?,
        parent_id: row.get(22)?,
    };
    node.validate()
        .map_err(|error| rusqlite::Error::InvalidParameterName(error.to_string()))?;
    Ok(node)
}

fn nonnegative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn sqlite(operation: &'static str) -> impl FnOnce(rusqlite::Error) -> StorageRuntimeErrorV1 {
    move |error| infrastructure(format!("{operation}: {error}"))
}

fn infrastructure(operation: impl Into<String>) -> StorageRuntimeErrorV1 {
    StorageRuntimeErrorV1::Infrastructure {
        operation: operation.into(),
    }
}

fn display_language_for_path(path: &str) -> &'static str {
    let basename = path.rsplit('/').next().unwrap_or(path);
    let lower = basename.to_ascii_lowercase();
    if lower == "dockerfile" || lower.starts_with("dockerfile.") {
        return "Dockerfile";
    }
    if lower == "makefile" {
        return "Makefile";
    }
    match path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => "Rust",
        "go" => "Go",
        "py" | "pyi" | "pyx" => "Python",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "ts" | "tsx" | "mts" | "cts" => "TypeScript",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "scala" | "sc" => "Scala",
        "swift" => "Swift",
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => "C++",
        "cs" => "C#",
        "fs" | "fsi" | "fsx" => "F#",
        "rb" => "Ruby",
        "php" => "PHP",
        "dart" => "Dart",
        "lua" => "Lua",
        "pl" | "pm" => "Perl",
        "sh" | "bash" => "Bash",
        "ps1" | "psm1" => "PowerShell",
        "nix" => "Nix",
        "zig" => "Zig",
        "proto" => "Protobuf",
        "toml" => "TOML",
        "sql" => "SQL",
        "r" => "R",
        "jl" => "Julia",
        "ex" | "exs" => "Elixir",
        "erl" | "hrl" => "Erlang",
        "hs" => "Haskell",
        "clj" | "cljs" | "cljc" | "edn" => "Clojure",
        "ml" | "mli" => "OCaml",
        "lean" => "Lean",
        "m" | "mm" => "Objective-C",
        "f" | "f90" | "f95" | "f03" | "f08" | "for" => "Fortran",
        "cbl" | "cob" | "cpy" => "COBOL",
        "pas" | "pp" | "dpr" => "Pascal",
        "vb" => "VB.NET",
        "bas" => "BASIC",
        "bat" | "cmd" => "Batch",
        "glsl" | "vert" | "frag" | "comp" | "geom" | "tesc" | "tese" => "GLSL",
        "qnt" => "Quint",
        _ => "Other",
    }
}

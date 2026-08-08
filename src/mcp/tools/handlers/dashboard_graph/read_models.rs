//! Wire decoding, error mapping, and overview aggregation shared across
//! dashboard graph operations.
//!
//! These are the small, stateless conversions every operation in the parent
//! adapter reaches for: turning a raw `nodes`/`edges` row (as produced by
//! `queries`) into its typed read model (`decode_node`, `decode_edge`), the
//! relational-to-domain edge-kind vocabulary (`relation_kind_str`), the two
//! error constructors every fallible read funnels through
//! (`unavailable`, `map_graph_error`), and the overview aggregation
//! (`overview_read_model` and its private per-kind/per-language/largest-file
//! helpers) that turns `GraphStats`/`FileRecord` into the dashboard's
//! `DashboardGraphOverviewV1`.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;
use tracedecay_application::{
    DashboardGraphEdgeV1, DashboardGraphKindCountV1, DashboardGraphLanguageCountV1,
    DashboardGraphLargestFileV1, DashboardGraphNodeV1, DashboardGraphOverviewV1,
    DashboardGraphReadErrorV1, DashboardGraphSpanV1, DashboardGraphTotalsV1,
};
use tracedecay_domain::RelationEdgeKindV1;
use tracedecay_domain::code_intelligence::{FileRecord, GraphStats};
use tracedecay_graph_db::GraphDbError;

/// Canonical relation kinds share the relational edge-kind vocabulary for
/// the kinds both sides define, so the served wire strings are stable across
/// the adjacency cutover.
pub(super) fn relation_kind_str(kind: RelationEdgeKindV1) -> &'static str {
    match kind {
        RelationEdgeKindV1::Calls => "calls",
        RelationEdgeKindV1::Uses => "uses",
        RelationEdgeKindV1::TypeOf => "type_of",
        RelationEdgeKindV1::Contains => "contains",
        RelationEdgeKindV1::Implements => "implements",
        RelationEdgeKindV1::Extends => "extends",
        RelationEdgeKindV1::Annotates => "annotates",
    }
}

pub(super) fn unavailable(detail: impl Into<String>) -> DashboardGraphReadErrorV1 {
    DashboardGraphReadErrorV1::Unavailable {
        detail: detail.into(),
    }
}

pub(super) fn map_graph_error(error: GraphDbError) -> DashboardGraphReadErrorV1 {
    match error {
        GraphDbError::Cancelled => DashboardGraphReadErrorV1::Cancelled,
        GraphDbError::DeadlineExceeded => DashboardGraphReadErrorV1::TimedOut,
        GraphDbError::InvalidRequest { message } => {
            DashboardGraphReadErrorV1::InvalidRequest { detail: message }
        }
        corrupt @ (GraphDbError::Corrupt { .. }
        | GraphDbError::ProjectionMismatch { .. }
        | GraphDbError::GenerationMismatch { .. }
        | GraphDbError::ResetRequired { .. }) => DashboardGraphReadErrorV1::Corrupt {
            detail: corrupt.to_string(),
        },
        other => DashboardGraphReadErrorV1::Unavailable {
            detail: other.to_string(),
        },
    }
}

pub(super) fn i64_field(row: &Value, key: &str) -> i64 {
    row.get(key).and_then(Value::as_i64).unwrap_or(0)
}

pub(super) fn str_field<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

pub(super) fn decode_node(row: Value) -> Result<DashboardGraphNodeV1, DashboardGraphReadErrorV1> {
    let mut node: DashboardGraphNodeV1 =
        serde_json::from_value(row).map_err(|error| DashboardGraphReadErrorV1::Corrupt {
            detail: format!("dashboard graph node row is invalid: {error}"),
        })?;
    node.span = Some(DashboardGraphSpanV1 {
        start_line: node.start_line.unwrap_or(0),
        end_line: node.end_line.unwrap_or(0),
        start_column: node.start_column.unwrap_or(0),
        end_column: node.end_column.unwrap_or(0),
        attrs_start_line: node.attrs_start_line.unwrap_or(0),
    });
    Ok(node)
}

pub(super) fn decode_edge(row: Value) -> Result<DashboardGraphEdgeV1, DashboardGraphReadErrorV1> {
    serde_json::from_value(row).map_err(|error| DashboardGraphReadErrorV1::Corrupt {
        detail: format!("dashboard graph edge row is invalid: {error}"),
    })
}

fn language_for_path(path: &str) -> &'static str {
    let Some((_, ext)) = path.rsplit_once('.') else {
        return "unknown";
    };
    match ext {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "scala" | "sc" => "scala",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "kt" | "kts" => "kotlin",
        "cs" => "csharp",
        "swift" => "swift",
        "rb" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "zig" => "zig",
        "sh" | "bash" | "zsh" => "shell",
        "md" | "mdx" => "markdown",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "sql" => "sql",
        "html" | "css" => "web",
        _ => "other",
    }
}

fn saturating_count(count: u64) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

fn kind_counts(counts: &HashMap<String, u64>) -> Vec<DashboardGraphKindCountV1> {
    let mut entries: Vec<_> = counts.iter().collect();
    entries.sort_by(|(left_label, left_count), (right_label, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_label.cmp(right_label))
    });
    entries
        .into_iter()
        .map(|(kind, count)| DashboardGraphKindCountV1 {
            kind: kind.clone(),
            count: saturating_count(*count),
        })
        .collect()
}

fn files_by_language(files: &[FileRecord]) -> Vec<DashboardGraphLanguageCountV1> {
    let mut counts: BTreeMap<&'static str, i64> = BTreeMap::new();
    for file in files {
        *counts.entry(language_for_path(&file.path)).or_insert(0) += 1;
    }
    let mut rows: Vec<DashboardGraphLanguageCountV1> = counts
        .into_iter()
        .map(|(language, count)| DashboardGraphLanguageCountV1 {
            language: language.to_owned(),
            count,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.language.cmp(&b.language))
    });
    rows
}

fn largest_files(files: &[FileRecord]) -> Vec<DashboardGraphLargestFileV1> {
    let mut files: Vec<_> = files.iter().collect();
    files.sort_by(|left, right| {
        right
            .node_count
            .cmp(&left.node_count)
            .then_with(|| left.path.cmp(&right.path))
    });
    files
        .into_iter()
        .take(12)
        .map(|file| DashboardGraphLargestFileV1 {
            path: file.path.clone(),
            node_count: i64::from(file.node_count),
            size: file.size,
        })
        .collect()
}

pub(super) fn overview_read_model(
    stats: &GraphStats,
    files: &[FileRecord],
    top_connected: Vec<DashboardGraphNodeV1>,
) -> DashboardGraphOverviewV1 {
    DashboardGraphOverviewV1 {
        totals: DashboardGraphTotalsV1 {
            nodes: stats.node_count,
            edges: stats.edge_count,
            files: stats.file_count,
        },
        nodes_by_kind: kind_counts(&stats.nodes_by_kind),
        edges_by_kind: kind_counts(&stats.edges_by_kind),
        files_by_language: files_by_language(files),
        largest_files: largest_files(files),
        top_connected,
    }
}

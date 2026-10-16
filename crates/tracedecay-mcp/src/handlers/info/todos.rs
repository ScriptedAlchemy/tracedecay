//! `tracedecay_todos`, marker-word scan (TODO, FIXME, …) across indexed files.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::decode_primitive_request;
use crate::handlers::graph::graph_tool_completion;
use serde_json::Value;
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{TodoMarkerV1, TodosResultV1, TodosSurfaceRequestV1};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_graph_query::VerifiedGraphQuery;
use tracedecay_runtime_core::storage::ProjectPath;

use super::verified::{
    end_line, indexed_file_counts, required_file_path, required_metadata, symbols_in_files,
};

/// Default marker kinds recognised by `tracedecay_todos`.
const DEFAULT_TODO_KINDS: &[&str] = &[
    "TODO",
    "FIXME",
    "XXX",
    "HACK",
    "WIP",
    "NOTE",
    "UNIMPLEMENTED",
];

/// True if `text` contains `marker` exactly as written (the uppercase marker
/// spelling) bounded by non-identifier characters or string ends. Matching
/// case-sensitively keeps identifiers such as `todo` or `note` out of the scan.
fn contains_marker_word(text: &str, marker: &str) -> bool {
    let bytes = text.as_bytes();
    let is_identifier = |byte: &u8| byte.is_ascii_alphanumeric() || *byte == b'_';
    text.match_indices(marker).any(|(start, _)| {
        !start
            .checked_sub(1)
            .and_then(|before| bytes.get(before))
            .is_some_and(is_identifier)
            && !bytes.get(start + marker.len()).is_some_and(is_identifier)
    })
}

#[tracing::instrument(name = "mcp.info.todos.total", level = "trace", skip_all)]
pub async fn compute_todos(
    graph: &VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let request: TodosSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_todos")?;
    let kinds = request
        .kinds
        .as_ref()
        .map(|values| {
            values
                .iter()
                .map(|value| value.to_uppercase())
                .collect::<Vec<_>>()
        })
        .filter(|v: &Vec<String>| !v.is_empty())
        .unwrap_or_else(|| {
            DEFAULT_TODO_KINDS
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        });

    let path = request
        .path
        .clone()
        .or_else(|| scope_prefix.map(str::to_owned));
    let limit = request.limit.map_or(200, |value| value.min(2000) as usize);

    let mut file_counts = {
        let _span = tracing::trace_span!("mcp.info.todos.files").entered();
        indexed_file_counts(graph)?
    };
    if let Some(prefix) = path.as_deref() {
        file_counts
            .retain(|file| tracedecay_domain::path_matches_scope(&file.logical_path, Some(prefix)));
    }
    file_counts.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    let files = file_counts
        .iter()
        .map(|file| file.logical_path.clone())
        .collect::<Vec<_>>();
    // The marker walk reads every candidate source file, so it belongs on a
    // blocking worker like the sibling analysis scans.
    let project_root = graph.project_root()?.to_path_buf();
    let MarkerScan {
        mut markers,
        touched,
        by_kind,
    } = tracing::Instrument::instrument(
        tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
            scan_markers(&project_root, &files, &kinds, limit)
        }),
        tracing::trace_span!("mcp.info.todos.scan"),
    )
    .await
    .map_err(|join_error| TraceDecayError::Config {
        message: format!("tracedecay_todos scan failed to join: {join_error}"),
    })??;

    // Only files that carry a marker need their symbols, to name the
    // innermost symbol enclosing each marker.
    file_counts.retain(|file| touched.contains(&file.logical_path));
    let symbols = {
        let _span = tracing::trace_span!("mcp.info.todos.symbols").entered();
        symbols_in_files(graph, &file_counts)?
    };
    name_enclosing_symbols(&mut markers, &symbols)?;

    let result = TodosResultV1 {
        match_count: markers.len(),
        by_kind,
        markers,
        freshness: None,
    };
    Ok(graph_tool_completion(
        GraphToolResultV1::Todos(result),
        touched,
    ))
}

struct MarkerScan {
    markers: Vec<TodoMarkerV1>,
    touched: Vec<String>,
    by_kind: BTreeMap<String, u64>,
}

/// The first `limit` markers of `files`, at most one per line, the first
/// requested kind winning.
fn scan_markers(
    project_root: &Path,
    files: &[String],
    kinds: &[String],
    limit: usize,
) -> Result<MarkerScan> {
    let mut scan = MarkerScan {
        markers: Vec::new(),
        touched: Vec::new(),
        by_kind: BTreeMap::new(),
    };
    for file in files {
        let project_path = ProjectPath::resolve(project_root, Path::new(file))?;
        let source = tracedecay_runtime_core::sync::read_source_file(&project_path.absolute_path())
            .map_err(|error| TraceDecayError::Config {
                message: format!("cannot read indexed source '{file}': {error}"),
            })?;
        for (idx, line) in source.lines().enumerate() {
            let Some(kind) = kinds.iter().find(|kind| contains_marker_word(line, kind)) else {
                continue;
            };
            *scan.by_kind.entry(kind.clone()).or_insert(0) += 1;
            scan.markers.push(TodoMarkerV1 {
                kind: kind.clone(),
                file: file.clone(),
                line: (idx as u32) + 1,
                text: line.trim().to_owned(),
                enclosing: None,
            });
            if !scan.touched.contains(file) {
                scan.touched.push(file.clone());
            }
            if scan.markers.len() >= limit {
                return Ok(scan);
            }
        }
    }
    Ok(scan)
}

/// Names each marker's innermost enclosing symbol; the first of equally
/// short spans in occurrence order wins.
fn name_enclosing_symbols(
    markers: &mut [TodoMarkerV1],
    symbols: &[CodeGraphSymbolSummaryV1],
) -> Result<()> {
    let mut symbols_by_file = HashMap::<&str, Vec<(&str, u32, u32)>>::new();
    for symbol in symbols {
        let metadata = required_metadata(symbol)?;
        let start = metadata.start_line.saturating_add(1);
        let end = end_line(metadata)?.saturating_add(1);
        symbols_by_file
            .entry(required_file_path(symbol)?)
            .or_default()
            .push((metadata.qualified_name.as_str(), start, end));
    }
    for marker in markers {
        let mut enclosing: Option<(&str, u32)> = None;
        let spans = symbols_by_file
            .get(marker.file.as_str())
            .into_iter()
            .flatten();
        for (qualified_name, start, end) in spans {
            let span = *end - *start;
            if *start <= marker.line
                && marker.line <= *end
                && enclosing.is_none_or(|(_, shortest)| span < shortest)
            {
                enclosing = Some((*qualified_name, span));
            }
        }
        marker.enclosing = enclosing.map(|(qualified_name, _)| qualified_name.to_owned());
    }
    Ok(())
}

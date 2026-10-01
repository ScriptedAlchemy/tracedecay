//! `tracedecay_todos`, marker-word scan (TODO, FIXME, …) across indexed files.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::decode_primitive_request;
use crate::handlers::graph::graph_tool_completion;
use serde_json::Value;
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

/// True if `text` contains `marker` as a standalone uppercase word
/// (case-insensitive, surrounded by non-alphanumeric characters or string ends).
fn contains_marker_word(text: &str, marker: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    let marker_lower = marker.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mlen = marker_lower.len();
    let mut idx = 0;
    while idx + mlen <= bytes.len() {
        if &bytes[idx..idx + mlen] == marker_lower.as_bytes() {
            let before_ok =
                idx == 0 || !bytes[idx - 1].is_ascii_alphanumeric() && bytes[idx - 1] != b'_';
            let after_ok = idx + mlen == bytes.len()
                || (!bytes[idx + mlen].is_ascii_alphanumeric() && bytes[idx + mlen] != b'_');
            if before_ok && after_ok {
                return Some(idx);
            }
        }
        idx += 1;
    }
    None
}

#[hotpath::measure(label = "mcp.info.todos.total")]
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

    let mut file_counts =
        hotpath::measure_block!("mcp.info.todos.files", indexed_file_counts(graph)?);
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
    let (mut markers, touched, by_kind) = hotpath::future!(
        tokio::task::spawn_blocking(move || -> Result<_> {
            let mut markers = Vec::<TodoMarkerV1>::new();
            let mut touched: Vec<String> = Vec::new();
            let mut by_kind = BTreeMap::<String, u64>::new();

            'outer: for file in &files {
                let project_path = ProjectPath::resolve(&project_root, Path::new(file))?;
                let source =
                    tracedecay_runtime_core::sync::read_source_file(&project_path.absolute_path())
                        .map_err(|error| TraceDecayError::Config {
                            message: format!("cannot read indexed source '{file}': {error}"),
                        })?;

                for (idx, line) in source.lines().enumerate() {
                    let line_no = (idx as u32) + 1;
                    for kind in &kinds {
                        if contains_marker_word(line, kind).is_some() {
                            *by_kind.entry(kind.clone()).or_insert(0) += 1;
                            markers.push(TodoMarkerV1 {
                                kind: kind.clone(),
                                file: file.clone(),
                                line: line_no,
                                text: line.trim().to_owned(),
                                enclosing: None,
                            });
                            if !touched.contains(file) {
                                touched.push(file.clone());
                            }
                            if markers.len() >= limit {
                                break 'outer;
                            }
                            break; // one marker per line is enough
                        }
                    }
                }
            }
            Ok((markers, touched, by_kind))
        }),
        label = "mcp.info.todos.scan"
    )
    .await
    .map_err(|join_error| TraceDecayError::Config {
        message: format!("tracedecay_todos scan failed to join: {join_error}"),
    })??;

    // Only files that carry a marker need their symbols, to name the
    // innermost symbol enclosing each marker.
    file_counts.retain(|file| touched.contains(&file.logical_path));
    let symbols = hotpath::measure_block!(
        "mcp.info.todos.symbols",
        symbols_in_files(graph, &file_counts)?
    );
    let mut symbols_by_file = HashMap::<&str, Vec<(&str, u32, u32)>>::new();
    for symbol in &symbols {
        let metadata = required_metadata(symbol)?;
        let start = metadata.start_line.saturating_add(1);
        let end = end_line(metadata)?.saturating_add(1);
        symbols_by_file
            .entry(required_file_path(symbol)?)
            .or_default()
            .push((metadata.qualified_name.as_str(), start, end));
    }
    for marker in &mut markers {
        let mut enclosing = None;
        for (qualified_name, start, end) in symbols_by_file
            .get(marker.file.as_str())
            .into_iter()
            .flatten()
        {
            if *start <= marker.line && marker.line <= *end {
                let span = *end - *start;
                if enclosing
                    .as_ref()
                    .is_none_or(|(_, shortest_span)| span < *shortest_span)
                {
                    enclosing = Some((*qualified_name, span));
                }
            }
        }
        marker.enclosing = enclosing.map(|(qualified_name, _)| qualified_name.to_owned());
    }

    let result = TodosResultV1 {
        match_count: markers.len(),
        by_kind,
        markers,
    };
    Ok(graph_tool_completion(
        GraphToolResultV1::Todos(result),
        touched,
    ))
}

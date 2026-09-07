//! `tracedecay_todos` — marker-word scan (TODO, FIXME, …) across indexed files.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::ToolResult;
use crate::{decode_primitive_request, generic_tool_result};
use serde_json::Value;
use tracedecay_application::retrieval::{TodoMarkerV1, TodosResultV1, TodosSurfaceRequestV1};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_graph_query::VerifiedGraphQuery;
use tracedecay_runtime_core::storage::ProjectPath;

use super::verified::{all_symbols, end_line, required_file_path, required_metadata};

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
pub async fn handle_todos(
    graph: &VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
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

    let symbols = hotpath::measure_block!("mcp.info.todos.symbols", all_symbols(graph)?);
    let mut files = symbols
        .iter()
        .map(|symbol| required_file_path(symbol).map(str::to_owned))
        .collect::<Result<Vec<_>>>()?;
    files.sort();
    files.dedup();
    let mut symbols_by_file = HashMap::<String, Vec<(String, u32, u32)>>::new();
    for symbol in &symbols {
        let metadata = required_metadata(symbol)?;
        let start = metadata.start_line.saturating_add(1);
        let end = end_line(metadata)?.saturating_add(1);
        symbols_by_file
            .entry(required_file_path(symbol)?.to_owned())
            .or_default()
            .push((metadata.qualified_name.clone(), start, end));
    }
    // Graph phase is done. The marker walk reads every candidate source file,
    // so it belongs on a blocking worker like the sibling analysis scans.
    let project_root = graph.project_root()?.to_path_buf();
    let response_project_root = project_root.clone();
    let (markers, touched, by_kind) = hotpath::future!(
        tokio::task::spawn_blocking(move || -> Result<_> {
            let mut markers = Vec::<TodoMarkerV1>::new();
            let mut touched: Vec<String> = Vec::new();
            let mut by_kind = BTreeMap::<String, u64>::new();

            'outer: for file in &files {
                if let Some(prefix) = path.as_deref()
                    && !tracedecay_runtime_core::path_scope::path_matches_scope(file, Some(prefix))
                {
                    continue;
                }
                let project_path = ProjectPath::resolve(&project_root, Path::new(file))?;
                let source =
                    tracedecay_runtime_core::sync::read_source_file(&project_path.absolute_path())
                        .map_err(|error| TraceDecayError::Config {
                            message: format!("cannot read indexed source '{file}': {error}"),
                        })?;
                let nodes = symbols_by_file.get(file);

                for (idx, line) in source.lines().enumerate() {
                    let line_no = (idx as u32) + 1;
                    for kind in &kinds {
                        if contains_marker_word(line, kind).is_some() {
                            let mut enclosing = None;
                            if let Some(nodes) = nodes {
                                for (qualified_name, start, end) in nodes {
                                    if *start <= line_no && line_no <= *end {
                                        let span = *end - *start;
                                        if enclosing
                                            .as_ref()
                                            .is_none_or(|(_, shortest_span)| span < *shortest_span)
                                        {
                                            enclosing = Some((qualified_name.clone(), span));
                                        }
                                    }
                                }
                            }
                            let enclosing = enclosing.map(|(qualified_name, _)| qualified_name);
                            *by_kind.entry(kind.clone()).or_insert(0) += 1;
                            markers.push(TodoMarkerV1 {
                                kind: kind.clone(),
                                file: file.clone(),
                                line: line_no,
                                text: line.trim().to_owned(),
                                enclosing,
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

    let result = TodosResultV1 {
        match_count: markers.len(),
        by_kind,
        markers,
    };
    let output = serde_json::to_value(result)?;
    Ok(generic_tool_result(
        Some(&response_project_root),
        &args,
        &output,
        touched,
    ))
}

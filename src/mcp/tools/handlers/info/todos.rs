//! `tracedecay_todos` — marker-word scan (TODO, FIXME, …) across indexed files.

use super::*;

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

/// Handles `tracedecay_todos` tool calls.
pub(crate) async fn handle_todos(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let kinds: Vec<String> = args
        .get("kinds")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_uppercase))
                .collect::<Vec<_>>()
        })
        .filter(|v: &Vec<String>| !v.is_empty())
        .unwrap_or_else(|| {
            DEFAULT_TODO_KINDS
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        });

    let path = effective_path(&args, scope_prefix);
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(200, |v| v.min(2000) as usize);

    let project_root = cg.project_root();
    let files = indexed_files(cg, graph)?;
    let symbols = all_symbols(graph)?;
    let mut symbols_by_file = HashMap::<&str, Vec<_>>::new();
    for symbol in &symbols {
        let metadata = required_metadata(symbol)?;
        let start = metadata.start_line.saturating_add(1);
        let end = end_line(metadata)?.saturating_add(1);
        symbols_by_file
            .entry(required_file_path(symbol)?)
            .or_default()
            .push((metadata, start, end));
    }
    let mut markers: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    let mut by_kind: HashMap<String, u64> = HashMap::new();

    'outer: for file in &files {
        if let Some(prefix) = path
            && !crate::path_scope::path_matches_scope(&file.path, Some(prefix))
        {
            continue;
        }
        let project_path = ProjectPath::resolve(project_root, Path::new(&file.path))?;
        let source =
            crate::sync::read_source_file(&project_path.absolute_path()).map_err(|error| {
                TraceDecayError::Config {
                    message: format!("cannot read indexed source '{}': {error}", file.path),
                }
            })?;
        let nodes = symbols_by_file.get(file.path.as_str());

        for (idx, line) in source.lines().enumerate() {
            let line_no = (idx as u32) + 1;
            for kind in &kinds {
                if contains_marker_word(line, kind).is_some() {
                    let mut enclosing = None;
                    if let Some(nodes) = nodes {
                        for &(metadata, start, end) in nodes {
                            if start <= line_no && line_no <= end {
                                let span = end - start;
                                if enclosing
                                    .as_ref()
                                    .is_none_or(|(_, shortest_span)| span < *shortest_span)
                                {
                                    enclosing = Some((metadata.qualified_name.clone(), span));
                                }
                            }
                        }
                    }
                    let enclosing = enclosing.map(|(qualified_name, _)| qualified_name);
                    *by_kind.entry(kind.clone()).or_insert(0) += 1;
                    markers.push(json!({
                        "kind": kind,
                        "file": file.path,
                        "line": line_no,
                        "text": line.trim(),
                        "enclosing": enclosing,
                    }));
                    if !touched.contains(&file.path) {
                        touched.push(file.path.clone());
                    }
                    if markers.len() >= limit {
                        break 'outer;
                    }
                    break; // one marker per line is enough
                }
            }
        }
    }

    let counts = serde_json::to_value(&by_kind)?;
    let output = json!({
        "match_count": markers.len(),
        "by_kind": counts,
        "markers": markers,
    });
    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched,
    ))
}

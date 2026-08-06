//! `tracedecay_body` — source bodies for symbols matched by name.

use super::*;

/// Extract the source spanning tree-sitter rows `start_line..=end_line`
/// (0-based, inclusive) from `source`. Node line fields are stored as the
/// raw tree-sitter row index, so the caller passes them through unchanged.
/// Returns the empty string if the range is out of bounds.
pub(crate) fn extract_lines(source: &str, start_line: u32, end_line: u32) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let start = start_line as usize;
    let end = (end_line as usize).saturating_add(1).min(lines.len());
    if start >= lines.len() || start >= end {
        return String::new();
    }
    lines[start..end].join("\n")
}

/// Handles `tracedecay_body` tool calls.
pub(crate) async fn handle_body(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
    deadline: Option<&tracedecay_application::Deadline>,
    cancellation: Option<&tracedecay_application::CancellationSignal>,
) -> Result<ToolResult> {
    let symbol =
        args.get("symbol")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: symbol".to_string(),
            })?;

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(3, |v| v.clamp(1, 20) as usize);

    let chosen = body_candidates(
        cg,
        symbol,
        limit,
        scope_prefix,
        dependency_hints::lazy_indexing_requested(&args),
        deadline,
        cancellation,
    )
    .await?;

    if chosen.is_empty() {
        return Ok(ToolResult::new(
            json!({
                "content": [{ "type": "text", "text": format!("No symbol named '{symbol}' found.") }]
            }),
            vec![],
        ));
    }

    let project_root = cg.project_root();
    let mut matches: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();

    for result in &chosen {
        let n = &result.node;
        let body = source_body_for_node(
            project_root,
            &n.file_path,
            n.start_line,
            n.end_line,
            &mut touched,
        );
        matches.push(json!({
            "id": n.id,
            "name": n.name,
            "qualified_name": n.qualified_name,
            "kind": n.kind.as_str(),
            "file": n.file_path,
            "start_line": n.start_line.saturating_add(1),
            "end_line": n.end_line.saturating_add(1),
            "signature": n.signature,
            "body": body,
        }));
    }

    let output = json!({
        "match_count": matches.len(),
        "matches": matches,
    });
    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched,
        || render_body_md(&output),
    ))
}

/// Renders `tracedecay_body` matches like `render_read_md` rather than dumping
/// source into a table cell with newlines collapsed: each match gets a heading,
/// a location line, an optional signature, a token count, and a fenced code
/// block tagged with the file's language extension.
fn render_body_md(value: &Value) -> String {
    use crate::context::read_modes::estimate_tokens;

    let mut md = Md::new();
    let matches = value.get("matches").and_then(Value::as_array);
    let count = matches.map_or(0, std::vec::Vec::len);
    md.heading(2, &format!("Body matches ({count})"));

    let Some(matches) = matches else {
        return md.render();
    };
    for m in matches {
        let name = render::field_str(m, "name");
        let kind = render::field_str(m, "kind");
        let file = render::field_str(m, "file");
        let start = render::field_i64(m, "start_line");
        let end = render::field_i64(m, "end_line");
        let signature = render::field_str(m, "signature");
        let body = render::field_str(m, "body");

        md.blank();
        md.heading(3, &format!("{name} ({kind})"));
        md.field("location", &format!("{file}:{start}-{end}"));
        if !signature.is_empty() {
            md.field("signature", signature);
        }
        md.field("tokens", &estimate_tokens(body).to_string());
        md.blank();
        let lang = file.rsplit_once('.').map_or("", |(_, ext)| ext);
        md.code(lang, body);
    }
    md.render()
}

async fn body_candidates(
    cg: &TraceDecay,
    symbol: &str,
    limit: usize,
    scope_prefix: Option<&str>,
    lazy_index_ignored_dependencies: bool,
    deadline: Option<&tracedecay_application::Deadline>,
    cancellation: Option<&tracedecay_application::CancellationSignal>,
) -> Result<Vec<crate::types::SearchResult>> {
    // First try an exact-name lookup against the DB — this avoids the BM25
    // ranker's tendency to bury a definition under unrelated noise when the
    // bare name is common (e.g. `gmres` exists as both a `pub fn` and a
    // struct field). Falls back to suffix / name matching.
    let exact_nodes = cg.get_nodes_by_qualified_name(symbol).await?;
    let mut exact_nodes = filter_by_scope(exact_nodes, scope_prefix, |n| &n.file_path);
    if exact_nodes.is_empty() && lazy_index_ignored_dependencies {
        let indexed = dependency_hints::lazy_index_ignored_dependency_candidates(
            cg,
            symbol,
            limit,
            scope_prefix,
            deadline,
            cancellation,
        )
        .await?;
        if !indexed.is_empty() {
            exact_nodes = filter_by_scope(
                cg.get_nodes_by_qualified_name(symbol).await?,
                scope_prefix,
                |n| &n.file_path,
            );
        }
    }

    // Wrap as SearchResult so the existing scoring/rendering path works.
    let mut candidates: Vec<crate::types::SearchResult> = exact_nodes
        .into_iter()
        .map(|node| crate::types::SearchResult { node, score: 0.0 })
        .collect();

    // If exact lookup returned nothing, fall back to BM25 search.
    if candidates.is_empty() {
        let raw = cg.search(symbol, (limit * 4).max(20)).await?;
        candidates = filter_by_scope(raw, scope_prefix, |r| &r.node.file_path);
    }

    // Whether the matches came from the exact lookup or the search fallback,
    // sort by `body_kind_preference` so callable / type definitions surface
    // above fields, variants, uses, etc. This is the bug-#1 fix: when both a
    // function and a same-named field exist, the function wins.
    candidates.sort_by_key(|r| body_kind_preference(&r.node.kind));
    candidates.truncate(limit);
    Ok(candidates)
}

fn source_body_for_node(
    project_root: &Path,
    file_path: &str,
    start_line: u32,
    end_line: u32,
    touched: &mut Vec<String>,
) -> String {
    let project_path = ProjectPath::resolve(project_root, Path::new(file_path));
    match project_path {
        Ok(ref path) => match crate::sync::read_source_file(&path.absolute_path()) {
            Ok(source) => {
                if !touched.iter().any(|path| path == file_path) {
                    touched.push(file_path.to_string());
                }
                extract_lines(&source, start_line, end_line)
            }
            Err(_) => String::from("<file unreadable>"),
        },
        Err(_) => String::from("<file path outside project>"),
    }
}

/// Ordering key used by `handle_body` to choose between same-named symbols.
/// Lower number = higher preference (sorted ascending). Callable kinds rank
/// best because the user almost always asks for "show me the body of X"
/// expecting a function or method; type definitions are next; fields,
/// variants, use statements come last.
fn body_kind_preference(kind: &NodeKind) -> u8 {
    match kind {
        NodeKind::Function
        | NodeKind::Method
        | NodeKind::StructMethod
        | NodeKind::Constructor
        | NodeKind::AbstractMethod
        | NodeKind::ArrowFunction
        | NodeKind::Procedure => 0,
        NodeKind::Struct
        | NodeKind::Enum
        | NodeKind::Trait
        | NodeKind::Class
        | NodeKind::InnerClass
        | NodeKind::Interface
        | NodeKind::InterfaceType
        | NodeKind::Record
        | NodeKind::CaseClass
        | NodeKind::DataClass
        | NodeKind::SealedClass
        | NodeKind::TypeAlias
        | NodeKind::Union
        | NodeKind::Typedef => 1,
        NodeKind::Impl => 2,
        NodeKind::Const | NodeKind::Static | NodeKind::Macro | NodeKind::PreprocessorDef => 3,
        NodeKind::Field
        | NodeKind::ValField
        | NodeKind::VarField
        | NodeKind::Property
        | NodeKind::CSharpProperty
        | NodeKind::EnumVariant => 4,
        NodeKind::Use | NodeKind::Include => 5,
        _ => 6,
    }
}

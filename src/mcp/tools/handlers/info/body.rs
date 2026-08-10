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
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
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
    if super::super::dependency_hints::lazy_indexing_requested(&args) {
        return Err(info_graph_error(
            "verified-body-lazy-indexing-unavailable",
            "lazy dependency indexing cannot mutate the generation pinned for this body request",
        ));
    }

    let chosen = body_candidates(graph, symbol, limit, scope_prefix)?;

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
        let (metadata, file_path) = required_symbol_parts(result)?;
        let body = source_body_for_node(
            project_root,
            file_path,
            metadata.start_line,
            end_line(metadata)?,
            &mut touched,
        )?;
        matches.push(json!({
            "id": result.occurrence.as_str(),
            "name": metadata.simple_name,
            "qualified_name": metadata.qualified_name,
            "kind": metadata.kind,
            "file": file_path,
            "start_line": metadata.start_line.saturating_add(1),
            "end_line": end_line(metadata)?.saturating_add(1),
            "signature": metadata.signature,
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

fn body_candidates(
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    symbol: &str,
    limit: usize,
    scope_prefix: Option<&str>,
) -> Result<Vec<tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1>> {
    let mut candidates = graph.resolve_qualified_name(symbol, None, 1_000)?;
    if candidates.is_empty() {
        candidates = graph.resolve_simple_name(symbol, None, 1_000)?;
    }
    let mut scoped = Vec::new();
    for candidate in candidates {
        let path = required_file_path(&candidate)?;
        let metadata = required_metadata(&candidate)?;
        if scope_prefix.is_none_or(|scope| crate::path_scope::path_matches_scope(path, Some(scope)))
        {
            let preference = NodeKind::from_str(&metadata.kind)
                .map_or(u8::MAX, |kind| body_kind_preference(&kind));
            scoped.push((preference, candidate));
        }
    }
    scoped.sort_by_key(|(preference, _)| *preference);
    let mut candidates = scoped
        .into_iter()
        .map(|(_, candidate)| candidate)
        .collect::<Vec<_>>();
    candidates.truncate(limit);
    Ok(candidates)
}

fn source_body_for_node(
    project_root: &Path,
    file_path: &str,
    start_line: u32,
    end_line: u32,
    touched: &mut Vec<String>,
) -> Result<String> {
    let project_path = ProjectPath::resolve(project_root, Path::new(file_path));
    match project_path {
        Ok(ref path) => match crate::sync::read_source_file(&path.absolute_path()) {
            Ok(source) => {
                if !touched.iter().any(|path| path == file_path) {
                    touched.push(file_path.to_string());
                }
                Ok(extract_lines(&source, start_line, end_line))
            }
            Err(error) => Err(TraceDecayError::Config {
                message: format!("cannot read indexed source body '{file_path}': {error}"),
            }),
        },
        Err(error) => Err(error),
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

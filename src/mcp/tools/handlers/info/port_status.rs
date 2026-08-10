//! `tracedecay_port_status` — cross-directory symbol coverage between a source and target port.

use super::*;
use std::collections::BTreeMap;

use tracedecay_application::retrieval::{
    PortMatchedSymbolV1, PortStatusResultV1, PortStatusSurfaceRequestV1, PortTargetOnlySymbolV1,
    PortUnmatchedSymbolV1,
};

use super::super::support::decode_primitive_request;

/// Returns the compatibility group for a node kind string used in port matching.
///
/// Kinds in the same group are considered cross-language equivalents:
/// - group 0: class, struct (cross-language data type)
/// - group 1: function
/// - group 2: method
/// - group 3: interface, trait
/// - group 4: enum
/// - group 5: module
fn kind_compat_group(kind: &str) -> u8 {
    match kind {
        "class" | "struct" => 0,
        "function" => 1,
        "method" => 2,
        "interface" | "trait" => 3,
        "enum" => 4,
        "module" => 5,
        _ => 255,
    }
}

/// Composite match key used by `handle_port_status`.
///
/// Combines the lowercased name, an optional parent qualifier (for methods,
/// fields, and variants), and a kind compatibility group, so siblings whose
/// names happen to collide (`Biquad::new` vs `Adaa::new`) do not cross-match.
type PortKey = (String, Option<String>, u8);

/// Returns true for kinds that conceptually have a parent type/owner whose
/// identity matters for matching (methods, fields, variants, etc.). Top-level
/// items (struct, function, …) return false — their parent in `qualified_name`
/// is just the file path and is not useful for cross-port matching.
fn port_kind_has_parent(kind: &str) -> bool {
    matches!(
        kind,
        "method"
            | "field"
            | "enum_variant"
            | "struct_method"
            | "abstract_method"
            | "constructor"
            | "csharp_property"
            | "property"
            | "val"
            | "var"
    )
}

/// Extracts the parent qualifier from a node's `qualified_name`, stripping
/// generic parameters so `Biquad<T>::new` and `Biquad::new` share the same
/// parent. Returns `None` for kinds where the parent qualifier is not the
/// containing type (e.g. top-level structs whose parent is the file path).
fn port_parent_qualifier(kind: &str, qualified_name: &str) -> Option<String> {
    if !port_kind_has_parent(kind) {
        return None;
    }
    let parts: Vec<&str> = qualified_name.split("::").collect();
    if parts.len() < 2 {
        return None;
    }
    let parent = parts.get(parts.len() - 2)?;
    // Strip generic parameters: `Biquad<T>` -> `Biquad`.
    let parent_no_generics = parent.split('<').next()?;
    Some(parent_no_generics.trim().to_string())
}

/// Handles `tracedecay_port_status` tool calls.
pub(crate) async fn handle_port_status(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let request: PortStatusSurfaceRequestV1 =
        decode_primitive_request(&args, "tracedecay_port_status")?;
    let kind_strs = request.kinds.as_ref().map_or_else(
        || {
            PORT_DEFAULT_KINDS
                .iter()
                .map(std::string::ToString::to_string)
                .collect()
        },
        Clone::clone,
    );

    let kinds: Vec<NodeKind> = kind_strs
        .iter()
        .filter_map(|s| NodeKind::from_str(s))
        .collect();

    if kinds.is_empty() {
        return Err(TraceDecayError::Config {
            message: "invalid parameter: kinds must contain at least one supported node kind"
                .to_owned(),
        });
    }

    let source_nodes = symbols_in_dir(graph, &request.source_dir, &kinds)?;
    let target_nodes = symbols_in_dir(graph, &request.target_dir, &kinds)?;

    // Match key includes the parent qualifier (e.g. enclosing struct/class) for
    // kinds that have one, so `Biquad::new` does NOT collide with `Adaa::new`.
    // Top-level kinds (struct, function, …) keep using name-only matching.
    let mut target_map = HashMap::<PortKey, Vec<_>>::new();
    for node in &target_nodes {
        let metadata = required_metadata(node)?;
        let key: PortKey = (
            metadata.simple_name.to_lowercase(),
            port_parent_qualifier(&metadata.kind, &metadata.qualified_name)
                .map(|value| value.to_lowercase()),
            kind_compat_group(&metadata.kind),
        );
        target_map.entry(key).or_default().push(node);
    }

    let mut matched_symbols = Vec::<PortMatchedSymbolV1>::new();
    let mut matched_target_ids = HashSet::new();
    let mut unmatched_by_file = BTreeMap::<String, Vec<PortUnmatchedSymbolV1>>::new();

    for src_node in &source_nodes {
        let (source_metadata, source_file) = required_symbol_parts(src_node)?;
        let key: PortKey = (
            source_metadata.simple_name.to_lowercase(),
            port_parent_qualifier(&source_metadata.kind, &source_metadata.qualified_name)
                .map(|value| value.to_lowercase()),
            kind_compat_group(&source_metadata.kind),
        );
        if let Some(targets) = target_map.get(&key) {
            // Take the first match
            let tgt = targets[0];
            let (target_metadata, target_file) = required_symbol_parts(tgt)?;
            matched_symbols.push(PortMatchedSymbolV1 {
                name: source_metadata.simple_name.clone(),
                source_kind: source_metadata.kind.clone(),
                target_kind: target_metadata.kind.clone(),
                source_file: source_file.to_owned(),
                target_file: target_file.to_owned(),
            });
            matched_target_ids.insert(tgt.occurrence.clone());
        } else {
            unmatched_by_file
                .entry(source_file.to_owned())
                .or_default()
                .push(PortUnmatchedSymbolV1 {
                    name: source_metadata.simple_name.clone(),
                    kind: source_metadata.kind.clone(),
                    line: source_metadata.start_line.saturating_add(1),
                });
        }
    }

    // Target-only symbols (in target but no source match)
    let mut target_only = Vec::new();
    for node in &target_nodes {
        if matched_target_ids.contains(&node.occurrence) {
            continue;
        }
        let (metadata, file) = required_symbol_parts(node)?;
        target_only.push(PortTargetOnlySymbolV1 {
            name: metadata.simple_name.clone(),
            kind: metadata.kind.clone(),
            file: file.to_owned(),
            line: metadata.start_line.saturating_add(1),
        });
    }

    let source_count = source_nodes.len();
    let matched_count = matched_symbols.len();
    let unmatched_count = source_count - matched_count;
    let coverage = if source_count > 0 {
        (matched_count as f64 / source_count as f64) * 100.0
    } else {
        0.0
    };

    let touched_paths = source_nodes
        .iter()
        .chain(target_nodes.iter())
        .map(required_file_path)
        .collect::<Result<Vec<_>>>()?;
    let touched_files = unique_file_paths(touched_paths.into_iter());

    let result = PortStatusResultV1 {
        source_dir: request.source_dir,
        target_dir: request.target_dir,
        source_count,
        target_count: target_nodes.len(),
        matched: matched_count,
        unmatched: unmatched_count,
        target_only: target_only.len(),
        coverage_percent: (coverage * 10.0).round() / 10.0,
        unmatched_by_file,
        matched_symbols,
        target_only_symbols: target_only,
    };
    let output = serde_json::to_value(result)?;

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
    ))
}

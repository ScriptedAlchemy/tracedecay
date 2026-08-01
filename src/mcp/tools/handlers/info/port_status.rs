//! `tracedecay_port_status` — cross-directory symbol coverage between a source and target port.

use super::*;

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
fn port_parent_qualifier(node: &crate::types::Node) -> Option<String> {
    if !port_kind_has_parent(node.kind.as_str()) {
        return None;
    }
    let parts: Vec<&str> = node.qualified_name.split("::").collect();
    if parts.len() < 2 {
        return None;
    }
    let parent = parts[parts.len() - 2];
    // Strip generic parameters: `Biquad<T>` -> `Biquad`.
    let parent_no_generics = parent.split('<').next().unwrap_or(parent);
    Some(parent_no_generics.trim().to_string())
}

/// Handles `tracedecay_port_status` tool calls.
pub(crate) async fn handle_port_status(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    require_object_args(&args, "tracedecay_port_status")?;

    let source_dir = args
        .get("source_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: source_dir".to_string(),
        })?;

    let target_dir = args
        .get("target_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: target_dir".to_string(),
        })?;

    let kind_strs: Vec<String> = args.get("kinds").and_then(|v| v.as_array()).map_or_else(
        || {
            PORT_DEFAULT_KINDS
                .iter()
                .map(std::string::ToString::to_string)
                .collect()
        },
        |arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
                .collect()
        },
    );

    let kinds: Vec<NodeKind> = kind_strs
        .iter()
        .filter_map(|s| NodeKind::from_str(s))
        .collect();

    if kinds.is_empty() {
        return Ok(ToolResult::new(
            json!({
                "content": [{ "type": "text", "text": "No valid node kinds specified." }]
            }),
            vec![],
        ));
    }

    let source_nodes = cg.get_nodes_by_dir(source_dir, &kinds).await?;
    let target_nodes = cg.get_nodes_by_dir(target_dir, &kinds).await?;

    // Match key includes the parent qualifier (e.g. enclosing struct/class) for
    // kinds that have one, so `Biquad::new` does NOT collide with `Adaa::new`.
    // Top-level kinds (struct, function, …) keep using name-only matching.
    let mut target_map: HashMap<PortKey, Vec<&crate::types::Node>> = HashMap::new();
    for node in &target_nodes {
        let key: PortKey = (
            node.name.to_lowercase(),
            port_parent_qualifier(node).map(|s| s.to_lowercase()),
            kind_compat_group(node.kind.as_str()),
        );
        target_map.entry(key).or_default().push(node);
    }

    let mut matched_symbols: Vec<Value> = Vec::new();
    let mut matched_target_ids: HashSet<String> = HashSet::new();
    let mut unmatched_by_file: HashMap<String, Vec<Value>> = HashMap::new();

    for src_node in &source_nodes {
        let key: PortKey = (
            src_node.name.to_lowercase(),
            port_parent_qualifier(src_node).map(|s| s.to_lowercase()),
            kind_compat_group(src_node.kind.as_str()),
        );
        if let Some(targets) = target_map.get(&key) {
            // Take the first match
            let tgt = targets[0];
            matched_symbols.push(json!({
                "name": src_node.name,
                "source_kind": src_node.kind.as_str(),
                "target_kind": tgt.kind.as_str(),
                "source_file": src_node.file_path,
                "target_file": tgt.file_path,
            }));
            matched_target_ids.insert(tgt.id.clone());
        } else {
            unmatched_by_file
                .entry(src_node.file_path.clone())
                .or_default()
                .push(json!({
                    "name": src_node.name,
                    "kind": src_node.kind.as_str(),
                    "line": src_node.start_line,
                }));
        }
    }

    // Target-only symbols (in target but no source match)
    let target_only: Vec<Value> = target_nodes
        .iter()
        .filter(|n| !matched_target_ids.contains(&n.id))
        .map(|n| {
            json!({
                "name": n.name,
                "kind": n.kind.as_str(),
                "file": n.file_path,
                "line": n.start_line,
            })
        })
        .collect();

    let source_count = source_nodes.len();
    let matched_count = matched_symbols.len();
    let unmatched_count = source_count - matched_count;
    let coverage = if source_count > 0 {
        (matched_count as f64 / source_count as f64) * 100.0
    } else {
        0.0
    };

    let touched_files = unique_file_paths(
        source_nodes
            .iter()
            .chain(target_nodes.iter())
            .map(|n| n.file_path.as_str()),
    );

    let result = json!({
        "source_dir": source_dir,
        "target_dir": target_dir,
        "source_count": source_count,
        "target_count": target_nodes.len(),
        "matched": matched_count,
        "unmatched": unmatched_count,
        "target_only": target_only.len(),
        "coverage_percent": (coverage * 10.0).round() / 10.0,
        "unmatched_by_file": unmatched_by_file,
        "matched_symbols": matched_symbols,
        "target_only_symbols": target_only,
    });

    Ok(generic_tool_result(
        Some(cg.project_root()),
        &args,
        &result,
        touched_files,
    ))
}

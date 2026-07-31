//! Structural analysis tool handlers: `dead_code`, `hotspots`, `circular`,
//! `coupling`, `rank`, `largest`, `recursion`, `complexity`, `distribution`,
//! `unused_imports`, `god_class`, `doc_coverage`, `inheritance_depth`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_lsp::analyzer::activity::{active_languages_for_files, documents_for_adapter};
use tracedecay_lsp::analyzer::broker::{
    CodeDiagnostic, DiagnosticBroker, DiagnosticSeverity as BrokerDiagnosticSeverity, NodeSpan,
    enclosing_node_for_line,
};

use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;
use crate::types::NodeKind;

use super::super::ToolResult;
use super::super::render;
use super::support::{effective_path, filter_by_scope, rendered_tool_result, unique_file_paths};

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// True when `path` names a Rust source file (case-insensitive `.rs`). Gates
/// tree-sitter masking, which parses with the Rust grammar and would
/// mis-tokenise other languages.
fn path_is_rust(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
}

/// Returns the identifiers a `use` statement brings into scope, parsing
/// grouped and aliased forms. Examples:
///   `foo::bar`             → bar
///   `foo::bar as baz`      → baz
///   `foo::{a, b}`          → a, b
///   `foo::{a, b as c}`     → a, c
///   `foo::{a, nested::b}`  → a, b
///   `foo::{self, bar}`     → foo, bar   (self brings the module in)
///   `foo::*`               → (empty, glob — handled separately)
fn identifiers_from_use_path(path: &str) -> Vec<String> {
    let trimmed = path.trim().trim_end_matches(';').trim();
    if trimmed.ends_with('*') {
        return Vec::new();
    }
    if let (Some(open), Some(close)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if close <= open {
            return Vec::new();
        }
        let prefix = trimmed[..open].trim().trim_end_matches("::").trim();
        let parent = prefix
            .rsplit("::")
            .next()
            .unwrap_or(prefix)
            .trim()
            .to_string();
        let inside = &trimmed[open + 1..close];
        let mut out: Vec<String> = Vec::new();
        let mut depth = 0i32;
        let mut start = 0usize;
        let bytes = inside.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                b',' if depth == 0 => {
                    let item = &inside[start..i];
                    push_identifier(&mut out, item, &parent);
                    start = i + 1;
                }
                _ => {}
            }
            i += 1;
        }
        push_identifier(&mut out, &inside[start..], &parent);
        return out;
    }
    let last_seg = trimmed.rsplit("::").next().unwrap_or(trimmed).trim();
    let id = identifier_from_segment(last_seg);
    if id.is_empty() || id == "*" {
        Vec::new()
    } else {
        vec![id]
    }
}

fn push_identifier(out: &mut Vec<String>, item: &str, parent: &str) {
    let item = item.trim();
    if item.is_empty() {
        return;
    }
    // Nested group: `foo::{a, sub::{x, y}}` — recurse on the nested part.
    if item.contains('{') {
        for id in identifiers_from_use_path(item) {
            out.push(id);
        }
        return;
    }
    let last_seg = item.rsplit("::").next().unwrap_or(item).trim();
    let id = identifier_from_segment(last_seg);
    if id.is_empty() {
        return;
    }
    if id == "self" {
        // `use foo::{self, bar}` brings `foo` itself into scope.
        if !parent.is_empty() {
            out.push(parent.to_string());
        }
        return;
    }
    if id == "*" {
        return;
    }
    out.push(id);
}

/// Resolves a single use-tree segment (no `::`) into the identifier it
/// brings into scope, accounting for `as` aliases.
fn identifier_from_segment(seg: &str) -> String {
    let seg = seg.trim().trim_end_matches(';').trim();
    if seg.is_empty() {
        return String::new();
    }
    // `foo as bar` → keep `bar`.
    let after_as = seg.split_whitespace().collect::<Vec<_>>();
    if let Some(pos) = after_as.iter().position(|w| *w == "as")
        && let Some(alias) = after_as.get(pos + 1)
    {
        return (*alias).to_string();
    }
    seg.split_whitespace()
        .next()
        .unwrap_or(seg)
        .trim()
        .to_string()
}

fn path_matches_optional_scope(path: &str, scope_prefix: Option<&str>) -> bool {
    crate::path_scope::path_matches_scope(path, scope_prefix)
}

/// Handles `tracedecay_dead_code` tool calls.
pub(super) async fn handle_dead_code(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let kinds: Vec<NodeKind> = args.get("kinds").and_then(|v| v.as_array()).map_or_else(
        || vec![NodeKind::Function, NodeKind::Method],
        |arr| {
            arr.iter()
                .filter_map(|v| v.as_str().and_then(NodeKind::from_str))
                .collect()
        },
    );

    let include_public = args
        .get("include_public")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(100, |value| value.clamp(1, 1_000) as usize);
    let dead = cg
        .find_dead_code_bounded(&kinds, include_public, limit)
        .await?;
    let dead = filter_by_scope(dead, scope_prefix, |n| &n.file_path);

    let touched_files = unique_file_paths(dead.iter().map(|n| n.file_path.as_str()));

    let items: Vec<Value> = dead
        .iter()
        .map(|n| {
            json!({
                "id": n.id,
                "name": n.name,
                "kind": n.kind.as_str(),
                "file": n.file_path,
                "line": n.start_line,
                "signature": n.signature,
            })
        })
        .collect();

    let output = json!({
        "dead_code_count": items.len(),
        "symbols": items,
    });

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
        || render::generic_md(&output),
    ))
}

/// Default and ceiling for the number of cycles `tracedecay_circular` reports
/// in one call. A whole-repository cycle list runs to tens of kilobytes, which
/// the response budget then truncates into a retrieval handle; a declared limit
/// keeps the answer inside the budget and states what it left out.
const CIRCULAR_DEFAULT_LIMIT: usize = 25;
const CIRCULAR_MAX_LIMIT: usize = 200;

/// Default and ceiling for member files listed per reported cycle.
///
/// Bounding the cycle count alone does not bound the answer: a single
/// strongly connected component in a real workspace can contain hundreds of
/// files, so `limit: 3` still rendered tens of kilobytes and landed in the
/// truncation envelope. Each entry therefore reports a bounded member list
/// plus its true member count, so a declared bound always fits the budget.
const CIRCULAR_DEFAULT_MEMBER_LIMIT: usize = 12;
const CIRCULAR_MAX_MEMBER_LIMIT: usize = 200;

/// One reported cycle: the members that fit the declared member bound, plus
/// the component's true size so the omission is stated rather than hidden.
#[derive(Debug, PartialEq, Eq)]
struct BoundedCycle {
    members: Vec<String>,
    member_count: usize,
    omitted_member_count: usize,
}

/// Handles `tracedecay_circular` tool calls.
pub(super) async fn handle_circular(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(CIRCULAR_DEFAULT_LIMIT, |limit| {
            (limit as usize).clamp(1, CIRCULAR_MAX_LIMIT)
        });
    let member_limit = args
        .get("member_limit")
        .and_then(Value::as_u64)
        .map_or(CIRCULAR_DEFAULT_MEMBER_LIMIT, |limit| {
            (limit as usize).clamp(1, CIRCULAR_MAX_MEMBER_LIMIT)
        });

    let all_cycles = cg.find_circular_dependencies().await?;
    let cycle_count = all_cycles.len();
    let (cycles, omitted) = bound_cycles(all_cycles, limit, member_limit);

    let output = circular_output(&cycles, cycle_count, omitted, limit, member_limit);

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        vec![],
        || render_circular_md(&cycles, cycle_count, omitted, limit),
    ))
}

fn circular_output(
    cycles: &[BoundedCycle],
    cycle_count: usize,
    omitted: usize,
    limit: usize,
    member_limit: usize,
) -> Value {
    let items: Vec<Value> = cycles
        .iter()
        .map(|cycle| {
            json!({
                "members": cycle.members,
                "member_count": cycle.member_count,
                "omitted_member_count": cycle.omitted_member_count,
            })
        })
        .collect();
    json!({
        "cycle_count": cycle_count,
        "reported_cycle_count": cycles.len(),
        "omitted_cycle_count": omitted,
        "limit": limit,
        "member_limit": member_limit,
        "cycles": items,
    })
}

/// Renders file-level dependency cycles as arrow chains that preserve cycle
/// order (`a.rs -> b.rs -> a.rs`) instead of collapsing the members into a
/// directory tree, which destroys the cyclic relationship. Each SCC's member
/// files are joined with ` -> ` and the first is repeated at the end to close
/// the loop.
/// Orders cycles largest-first and bounds them to `limit` cycles of
/// `member_limit` members each, returning the bounded page and the number of
/// cycles it leaves out.
///
/// The largest strongly connected components are the ones worth breaking, so a
/// bounded page reports the worst offenders rather than an arbitrary prefix.
/// Ties fall back to path order so repeated calls agree. Both the omitted cycle
/// count and each component's true member count are returned rather than
/// dropped: the caller always states what it left out.
fn bound_cycles(
    mut cycles: Vec<Vec<String>>,
    limit: usize,
    member_limit: usize,
) -> (Vec<BoundedCycle>, usize) {
    let omitted = cycles.len().saturating_sub(limit);
    cycles.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    cycles.truncate(limit);
    let bounded = cycles
        .into_iter()
        .map(|mut members| {
            let member_count = members.len();
            members.truncate(member_limit);
            BoundedCycle {
                omitted_member_count: member_count.saturating_sub(members.len()),
                members,
                member_count,
            }
        })
        .collect();
    (bounded, omitted)
}

fn render_circular_md(
    cycles: &[BoundedCycle],
    cycle_count: usize,
    omitted: usize,
    limit: usize,
) -> String {
    use std::fmt::Write as _;

    if cycle_count == 0 {
        return "No circular dependencies found.\n".to_string();
    }
    let mut out = String::new();
    let _ = writeln!(out, "# Circular Dependencies ({cycle_count})\n");
    for (i, cycle) in cycles.iter().enumerate() {
        let Some(entry) = cycle.members.first() else {
            continue;
        };
        let mut chain = cycle.members.join(" -> ");
        if cycle.omitted_member_count > 0 {
            // An elided component is not a closed loop; say so instead of
            // rendering a chain that reads as the whole cycle.
            let _ = write!(
                chain,
                " -> … ({} further member(s) not shown of {} at member_limit)",
                cycle.omitted_member_count, cycle.member_count
            );
        } else {
            // Close the loop by repeating the entry file.
            let _ = write!(chain, " -> {entry}");
        }
        let _ = writeln!(out, "{}. {chain}", i + 1);
    }
    if omitted > 0 {
        let _ = writeln!(
            out,
            "\n{omitted} further cycle(s) not shown at limit {limit}; raise `limit` (max {CIRCULAR_MAX_LIMIT}) to see more."
        );
    }
    out
}

/// Handles `tracedecay_hotspots` tool calls.
pub(super) async fn handle_hotspots(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);
    debug_assert!(limit > 0, "handle_hotspots limit must be positive");

    let hotspots = cg.get_hotspot_nodes(scope_prefix, limit).await?;
    let mut items: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    for (node, incoming, outgoing) in hotspots {
        touched.push(node.file_path.clone());
        items.push(json!({
            "id": node.id,
            "name": node.name,
            "kind": node.kind.as_str(),
            "file": node.file_path,
            "line": node.start_line,
            "incoming": incoming,
            "outgoing": outgoing,
            "total": incoming + outgoing,
        }));
    }

    let touched_files = unique_file_paths(touched.iter().map(std::string::String::as_str));

    let output = json!({
        "hotspot_count": items.len(),
        "hotspots": items,
    });

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
        || render::generic_md(&output),
    ))
}

/// Default and ceiling for the unused imports reported in one page.
const UNUSED_IMPORTS_DEFAULT_LIMIT: usize = 50;
const UNUSED_IMPORTS_MAX_LIMIT: usize = 500;

/// Files inspected in one call before the answer becomes a typed partial.
///
/// The scan reads and masks each candidate file's source, so an unbounded walk
/// over a large workspace never returns inside the caller's deadline. A file
/// budget keeps the call bounded and the response states the continuation
/// cursor rather than reporting a short list as the whole truth.
const UNUSED_IMPORTS_FILE_BUDGET: usize = 400;

/// Files fetched from the graph per keyset page while walking candidates.
const UNUSED_IMPORTS_FILE_PAGE: usize = 64;

/// The line span in which an identifier occurs within one masked file.
///
/// An identifier is referenced outside a `use` statement's own line range
/// exactly when its first occurrence precedes that range or its last
/// occurrence follows it, so two line numbers answer the question without
/// rescanning the file per identifier.
#[derive(Clone, Copy)]
struct IdentifierSpan {
    first_line: u32,
    last_line: u32,
}

/// Indexes every identifier in a masked source once, so each import's
/// reference check is a map lookup instead of a full-file scan.
fn identifier_spans(source: &str) -> HashMap<String, IdentifierSpan> {
    let mut spans: HashMap<String, IdentifierSpan> = HashMap::new();
    for (line_index, line) in source.lines().enumerate() {
        let line_index = line_index as u32;
        for identifier in identifiers_in_line(line) {
            spans
                .entry(identifier)
                .and_modify(|span| span.last_line = line_index)
                .or_insert(IdentifierSpan {
                    first_line: line_index,
                    last_line: line_index,
                });
        }
    }
    spans
}

/// Splits a masked source line into whole identifier tokens (boundaries are
/// any non-`[A-Za-z0-9_]` char or the line ends), so `Map` never matches
/// inside `HashMap`.
fn identifiers_in_line(line: &str) -> Vec<String> {
    let mut identifiers = Vec::new();
    let mut current = String::new();
    for character in line.chars() {
        if character.is_alphanumeric() || character == '_' {
            current.push(character);
        } else if !current.is_empty() {
            identifiers.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        identifiers.push(current);
    }
    identifiers
}

/// Handles `tracedecay_unused_imports` tool calls.
///
/// Walks candidate files in path order, so `cursor` resumes the walk exactly
/// where the previous page stopped and `complete` reports whether the answer
/// covers the whole scope.
pub(super) async fn handle_unused_imports(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(UNUSED_IMPORTS_DEFAULT_LIMIT, |limit| {
            (limit as usize).clamp(1, UNUSED_IMPORTS_MAX_LIMIT)
        });
    let mut after_path = args
        .get("cursor")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let project_root = cg.project_root();
    let mut unused: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    let mut scanned_files = 0usize;
    // The cursor is exclusive, so it must name the last file this call
    // finished. Advancing it past a file the call never inspected would drop
    // that file from every continuation.
    let mut last_scanned: Option<String> = None;
    let mut partial_reason: Option<&str> = None;

    'walk: loop {
        let files = cg
            .file_paths_with_nodes_of_kind(
                NodeKind::Use,
                after_path.as_deref(),
                UNUSED_IMPORTS_FILE_PAGE,
            )
            .await?;
        if files.is_empty() {
            break;
        }
        for file_path in files {
            if !path_matches_optional_scope(&file_path, scope_prefix) {
                after_path = Some(file_path);
                continue;
            }
            if scanned_files >= UNUSED_IMPORTS_FILE_BUDGET {
                partial_reason = Some("file_budget_exhausted");
                break 'walk;
            }
            scanned_files += 1;
            let file_unused = unused_imports_in_file(cg, project_root, &file_path).await?;
            if !file_unused.is_empty() {
                touched.push(file_path.clone());
            }
            unused.extend(file_unused);
            after_path = Some(file_path.clone());
            last_scanned = Some(file_path);
            if unused.len() >= limit {
                unused.truncate(limit);
                partial_reason = Some("limit_reached");
                break 'walk;
            }
        }
    }
    // A partial answer without a resumable cursor would be a dead end, so a
    // stop that produced no inspected file is reported as complete coverage of
    // what the scope contains rather than a fabricated continuation.
    let next_cursor = partial_reason.and(last_scanned);
    if next_cursor.is_none() {
        partial_reason = None;
    }

    let touched_files = unique_file_paths(touched.iter().map(std::string::String::as_str));

    let output = json!({
        "unused_import_count": unused.len(),
        "imports": unused,
        "limit": limit,
        "scanned_files": scanned_files,
        "complete": partial_reason.is_none(),
        "partial_reason": partial_reason,
        "next_cursor": next_cursor,
    });

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
        || render::unused_imports_md(&output),
    ))
}

/// Reports the unused imports of one file.
///
/// Source-text scan (comments and string/char literals masked, so an import
/// merely named in a comment is not read as a real use): a `use` identifier is
/// unused when it appears nowhere outside its own statement. A graph-only check
/// is unreliable because the Rust resolver creates no `Uses` edge for
/// std/foreign-crate imports.
///
/// `pub use` re-exports are intentional public aliases and are never reported.
async fn unused_imports_in_file(
    cg: &TraceDecay,
    project_root: &Path,
    file_path: &str,
) -> Result<Vec<Value>> {
    let use_nodes: Vec<crate::types::Node> = cg
        .get_nodes_by_file(file_path)
        .await?
        .into_iter()
        .filter(|node| node.kind == NodeKind::Use)
        .filter(|node| node.visibility != crate::types::Visibility::Pub)
        .collect();
    if use_nodes.is_empty() {
        return Ok(Vec::new());
    }
    let Ok(source) = std::fs::read_to_string(project_root.join(file_path)) else {
        return Ok(Vec::new());
    };
    let spans =
        identifier_spans(&tracedecay_code_extraction::source_mask::masked_rust_source(&source));

    let mut unused = Vec::new();
    for use_node in use_nodes {
        // The Use node's `name` is the full import path as written. Three
        // shapes show up in real Rust code:
        //   - `foo::bar`           → single identifier `bar`
        //   - `foo::bar as baz`    → single identifier `baz`
        //   - `foo::{a, b as c}`   → grouped: identifiers `a`, `c`
        // Grouped imports must expand, otherwise an unused member inside a
        // partially-used group is missed and the literal `{a, b as c}` is
        // treated as one identifier that matches nothing.
        for identifier in identifiers_from_use_path(&use_node.name) {
            let referenced = spans.get(&identifier).is_some_and(|span| {
                span.first_line < use_node.start_line || span.last_line > use_node.end_line
            });
            if !referenced {
                unused.push(json!({
                    "id": use_node.id,
                    "name": use_node.name,
                    "unused": identifier,
                    "file": file_path,
                    "line": use_node.start_line,
                }));
            }
        }
    }
    Ok(unused)
}

/// Handles `tracedecay_rank` tool calls.
pub(super) async fn handle_rank(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    use crate::types::EdgeKind;
    debug_assert!(args.is_object(), "handle_rank expects an object argument");

    let edge_kind_str = args
        .get("edge_kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: edge_kind".to_string(),
        })?;

    let edge_kind = EdgeKind::from_str(edge_kind_str).ok_or_else(|| TraceDecayError::Config {
        message: format!(
            "invalid edge_kind '{edge_kind_str}'. Valid values: implements, extends, calls, uses, contains, annotates, derives_macro"
        ),
    })?;

    let direction = args
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("incoming");

    let incoming = match direction {
        "incoming" => true,
        "outgoing" => false,
        _ => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "invalid direction '{direction}'. Valid values: incoming, outgoing"
                ),
            });
        }
    };

    let node_kind = args
        .get("node_kind")
        .and_then(|v| v.as_str())
        .and_then(NodeKind::from_str);

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);

    let path_prefix = effective_path(&args, scope_prefix);

    let results = cg
        .get_ranked_nodes_by_edge_kind(&edge_kind, node_kind.as_ref(), incoming, path_prefix, limit)
        .await?;

    let touched_files = unique_file_paths(results.iter().map(|(n, _)| n.file_path.as_str()));

    let items: Vec<Value> = results
        .iter()
        .map(|(node, count)| {
            json!({
                "id": node.id,
                "name": node.name,
                "kind": node.kind.as_str(),
                "file": node.file_path,
                "line": node.start_line,
                "count": count,
            })
        })
        .collect();

    let output = json!({
        "edge_kind": edge_kind_str,
        "direction": direction,
        "node_kind_filter": args.get("node_kind").and_then(|v| v.as_str()),
        "result_count": items.len(),
        "ranking": items,
    });

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
        || render::generic_md(&output),
    ))
}

/// Handles `tracedecay_largest` tool calls.
pub(super) async fn handle_largest(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let node_kind = args
        .get("node_kind")
        .and_then(|v| v.as_str())
        .and_then(NodeKind::from_str);

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);

    let path_prefix = effective_path(&args, scope_prefix);

    let results = cg
        .get_largest_nodes(node_kind.as_ref(), path_prefix, limit)
        .await?;

    let touched_files = unique_file_paths(results.iter().map(|(n, _)| n.file_path.as_str()));

    let items: Vec<Value> = results
        .iter()
        .map(|(node, lines)| {
            json!({
                "id": node.id,
                "name": node.name,
                "kind": node.kind.as_str(),
                "file": node.file_path,
                "start_line": node.start_line,
                "end_line": node.end_line,
                "lines": lines,
            })
        })
        .collect();

    let output = json!({
        "node_kind_filter": args.get("node_kind").and_then(|v| v.as_str()),
        "result_count": items.len(),
        "ranking": items,
    });

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
        || render::generic_md(&output),
    ))
}

/// Handles `tracedecay_coupling` tool calls.
pub(super) async fn handle_coupling(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let direction = args
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("fan_in");

    let fan_in = match direction {
        "fan_in" => true,
        "fan_out" => false,
        _ => {
            return Err(TraceDecayError::Config {
                message: format!("invalid direction '{direction}'. Valid values: fan_in, fan_out"),
            });
        }
    };

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);

    let path_prefix = effective_path(&args, scope_prefix);

    let results = cg.get_file_coupling(fan_in, path_prefix, limit).await?;

    let items: Vec<Value> = results
        .iter()
        .map(|(file, count)| {
            json!({
                "file": file,
                "coupled_files": count,
            })
        })
        .collect();

    let output = json!({
        "direction": direction,
        "result_count": items.len(),
        "ranking": items,
    });

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        vec![],
        || render::generic_md(&output),
    ))
}

/// Handles `tracedecay_inheritance_depth` tool calls.
pub(super) async fn handle_inheritance_depth(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);

    let path_prefix = effective_path(&args, scope_prefix);

    let results = cg.get_inheritance_depth(path_prefix, limit).await?;

    let touched_files = unique_file_paths(results.iter().map(|(n, _)| n.file_path.as_str()));

    let items: Vec<Value> = results
        .iter()
        .map(|(node, depth)| {
            json!({
                "id": node.id,
                "name": node.name,
                "kind": node.kind.as_str(),
                "file": node.file_path,
                "line": node.start_line,
                "depth": depth,
            })
        })
        .collect();

    let output = json!({
        "result_count": items.len(),
        "ranking": items,
    });

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
        || render::generic_md(&output),
    ))
}

/// Handles `tracedecay_distribution` tool calls.
pub(super) async fn handle_distribution(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    debug_assert!(
        args.is_object(),
        "handle_distribution expects an object argument"
    );
    let path_prefix = effective_path(&args, scope_prefix);
    let summary = args
        .get("summary")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let results = cg.get_node_distribution(path_prefix).await?;

    let output = if summary {
        // Aggregate counts across all files
        let mut totals: HashMap<String, u64> = HashMap::new();
        for (_file, kind, count) in &results {
            *totals.entry(kind.clone()).or_insert(0) += count;
        }
        let mut sorted: Vec<(String, u64)> = totals.into_iter().collect();
        sorted.sort_by_key(|x| std::cmp::Reverse(x.1));

        let items: Vec<Value> = sorted
            .iter()
            .map(|(kind, count)| json!({ "kind": kind, "count": count }))
            .collect();

        json!({
            "path_filter": path_prefix,
            "mode": "summary",
            "total_kinds": items.len(),
            "distribution": items,
        })
    } else {
        // Per-file breakdown, grouped by file
        let mut by_file: Vec<(String, Vec<Value>)> = Vec::new();
        let mut current_file = String::new();
        for (file, kind, count) in &results {
            if *file != current_file {
                current_file.clone_from(file);
                by_file.push((file.clone(), Vec::new()));
            }
            if let Some(last) = by_file.last_mut() {
                last.1.push(json!({ "kind": kind, "count": count }));
            }
        }

        let items: Vec<Value> = by_file
            .iter()
            .map(|(file, kinds)| json!({ "file": file, "kinds": kinds }))
            .collect();

        json!({
            "path_filter": path_prefix,
            "mode": "per_file",
            "file_count": items.len(),
            "files": items,
        })
    };

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        vec![],
        || render::generic_md(&output),
    ))
}

/// Handles `tracedecay_recursion` tool calls.
///
/// Detects cycles in the call graph using iterative DFS on the calls-only
/// edge subgraph. Each cycle is a vec of node IDs forming the loop.
pub(super) async fn handle_recursion(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);
    let path_prefix = effective_path(&args, scope_prefix);

    debug_assert!(limit > 0, "handle_recursion limit must be positive");

    let call_edges = cg.get_call_edges_with_lines(path_prefix).await?;

    let mut adj: HashMap<String, HashSet<String>> = HashMap::new();
    let mut node_cache: HashMap<String, Option<crate::types::Node>> = HashMap::new();
    let mut lines_cache: HashMap<String, Option<Vec<String>>> = HashMap::new();

    for (src, tgt, line) in &call_edges {
        if src == tgt {
            let Some(node) = cached_node(cg, &mut node_cache, src).await? else {
                continue;
            };
            if !is_direct_self_call(cg, &mut lines_cache, &node, *line) {
                continue;
            }
        }
        adj.entry(src.clone()).or_default().insert(tgt.clone());
        adj.entry(tgt.clone()).or_default();
    }

    // Collect only the cyclic SCCs, then sort smallest-first so we keep
    // shorter / more interesting cycles when the cap kicks in. We still need
    // every cyclic SCC enumerated before sorting (truncating early would bias
    // toward Tarjan emission order), but we cap the per-SCC path search.
    let mut cyclic_sccs: Vec<Vec<String>> = crate::graph::scc::tarjan_scc(&adj)
        .into_iter()
        .filter(|scc| crate::graph::scc::is_cyclic_scc(scc, &adj))
        .collect();
    cyclic_sccs.sort_by_key(Vec::len);

    let mut cycles: Vec<Vec<String>> = Vec::new();
    for mut scc in cyclic_sccs {
        if cycles.len() >= limit {
            break;
        }
        if let Some(path) = cycle_path_for_scc(&mut scc, &adj) {
            cycles.push(path);
        }
    }
    cycles.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    cycles.truncate(limit);

    // Resolve node details for each cycle
    let mut cycle_items: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    for cycle in &cycles {
        let mut chain: Vec<Value> = Vec::new();
        for node_id in cycle {
            if let Some(node) = cg.get_node(node_id).await? {
                touched.push(node.file_path.clone());
                chain.push(json!({
                    "id": node.id,
                    "name": node.name,
                    "kind": node.kind.as_str(),
                    "file": node.file_path,
                    "line": node.start_line,
                }));
            } else {
                chain.push(json!({ "id": node_id }));
            }
        }
        cycle_items.push(json!({
            "length": cycle.len() - 1,
            "chain": chain,
        }));
    }

    let touched_files = unique_file_paths(touched.iter().map(std::string::String::as_str));

    let output = json!({
        "cycle_count": cycle_items.len(),
        "cycles": cycle_items,
    });

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
        || render::generic_md(&output),
    ))
}

async fn cached_node(
    cg: &TraceDecay,
    cache: &mut HashMap<String, Option<crate::types::Node>>,
    id: &str,
) -> Result<Option<crate::types::Node>> {
    if let Some(node) = cache.get(id) {
        return Ok(node.clone());
    }
    let node = cg.get_node(id).await?;
    cache.insert(id.to_string(), node.clone());
    Ok(node)
}

fn cached_lines<'a>(
    cg: &TraceDecay,
    cache: &'a mut HashMap<String, Option<Vec<String>>>,
    file_path: &str,
) -> Option<&'a Vec<String>> {
    if !cache.contains_key(file_path) {
        let abs = cg.project_root().join(file_path);
        // Blank comments and string/char literals for Rust files so a
        // `name(` that appears only inside a comment or string is not mistaken
        // for a real self-call. Non-Rust files are scanned raw (the Rust
        // grammar would mis-tokenise them).
        let lines = std::fs::read_to_string(abs).ok().map(|content| {
            let scanned = if path_is_rust(file_path) {
                tracedecay_code_extraction::source_mask::masked_rust_source_with(
                    &content,
                    tracedecay_code_extraction::source_mask::MaskOptions::CODE_SCAN,
                )
            } else {
                content
            };
            scanned.lines().map(str::to_string).collect()
        });
        cache.insert(file_path.to_string(), lines);
    }
    cache.get(file_path).and_then(Option::as_ref)
}

fn is_direct_self_call(
    cg: &TraceDecay,
    lines_cache: &mut HashMap<String, Option<Vec<String>>>,
    node: &crate::types::Node,
    edge_line: Option<u32>,
) -> bool {
    let Some(lines) = cached_lines(cg, lines_cache, &node.file_path) else {
        return false;
    };
    if lines.is_empty() {
        return false;
    }

    let mut candidate_lines: Vec<u32> = edge_line.into_iter().collect();
    if let Some(line) = edge_line {
        candidate_lines.push(line.saturating_sub(1));
        candidate_lines.push(line.saturating_add(1));
    }
    candidate_lines.sort_unstable();
    candidate_lines.dedup();

    for line in candidate_lines {
        let Some(text) = lines.get(line as usize) else {
            continue;
        };
        if looks_like_function_declaration(text, &node.name) {
            continue;
        }
        if has_qualified_call(text, node) || has_bare_call(text, &node.name) {
            return true;
        }
    }

    false
}

fn looks_like_function_declaration(line: &str, name: &str) -> bool {
    let Some(pos) = line.find(name) else {
        return false;
    };
    let prefix = &line[..pos];
    (prefix.contains("fn ")
        || prefix.contains("function ")
        || prefix.contains("def ")
        || prefix.contains("sub "))
        && call_suffix_starts(&line[pos + name.len()..])
}

fn parent_type_name(node: &crate::types::Node) -> Option<&str> {
    let needle = format!("::{}", node.name);
    node.qualified_name
        .strip_suffix(&needle)
        .and_then(|parent| parent.rsplit("::").next())
        .filter(|parent| !parent.is_empty())
}

fn has_qualified_call(line: &str, node: &crate::types::Node) -> bool {
    let Some(parent) = parent_type_name(node) else {
        return false;
    };
    let type_call = format!("{parent}::{}", node.name);
    if line
        .match_indices(&type_call)
        .any(|(idx, _)| call_suffix_starts(&line[idx + type_call.len()..]))
    {
        return true;
    }

    let self_call = format!("Self::{}", node.name);
    if line
        .match_indices(&self_call)
        .any(|(idx, _)| call_suffix_starts(&line[idx + self_call.len()..]))
    {
        return true;
    }

    let self_method_call = format!("self.{}", node.name);
    line.match_indices(&self_method_call)
        .any(|(idx, _)| call_suffix_starts(&line[idx + self_method_call.len()..]))
}

fn has_bare_call(line: &str, name: &str) -> bool {
    // Fast path: a bare call always needs an opening paren on the same line.
    // For common short names like `new`/`get`/`len` this short-circuits the
    // expensive `match_indices + is_ident_byte` scan on lines that obviously
    // can't contain a call (assignments, comments, docstrings, …).
    if name.is_empty() || !line.contains('(') {
        return false;
    }
    let bytes = line.as_bytes();
    let name_len = name.len();
    line.match_indices(name).any(|(idx, _)| {
        // Reject substring matches inside a larger identifier on either side:
        // `name=new` should not match `newer`, `renew`, etc. Cheap byte
        // checks before the more expensive prefix-trim probe.
        let before_ok = idx == 0 || !is_ident_byte(bytes[idx - 1]);
        if !before_ok {
            return false;
        }
        let after_idx = idx + name_len;
        let after_ok = after_idx == bytes.len() || !is_ident_byte(bytes[after_idx]);
        if !after_ok {
            return false;
        }
        let prefix = line[..idx].trim_end();
        if prefix.ends_with('.') || prefix.ends_with(':') {
            return false;
        }
        call_suffix_starts(&line[after_idx..])
    })
}

fn call_suffix_starts(suffix: &str) -> bool {
    suffix.trim_start().starts_with('(')
}

fn cycle_path_for_scc(
    scc: &mut [String],
    adj: &HashMap<String, HashSet<String>>,
) -> Option<Vec<String>> {
    scc.sort();
    let scc_set: HashSet<&str> = scc.iter().map(std::string::String::as_str).collect();
    if scc.len() == 1 {
        let id = scc[0].clone();
        if adj
            .get(&id)
            .is_some_and(|neighbors| neighbors.contains(&id))
        {
            return Some(vec![id.clone(), id]);
        }
        return None;
    }

    for start in scc.iter() {
        // `path` and `seen` operate on borrowed ids from `scc_set`: the SCC
        // outlives this call, so we never need to allocate `String`s during
        // the DFS itself. The final result has to be `Vec<String>` because
        // it leaves the function, so we materialise once at the end.
        let start_ref: &str = start.as_str();
        let mut path: Vec<&str> = vec![start_ref];
        let mut seen: HashSet<&str> = HashSet::from([start_ref]);
        if dfs_cycle_path(start_ref, start_ref, &scc_set, adj, &mut path, &mut seen) {
            return Some(path.into_iter().map(str::to_string).collect());
        }
    }
    None
}

fn dfs_cycle_path<'a>(
    current: &'a str,
    start: &'a str,
    scc_set: &HashSet<&'a str>,
    adj: &'a HashMap<String, HashSet<String>>,
    path: &mut Vec<&'a str>,
    seen: &mut HashSet<&'a str>,
) -> bool {
    let Some(neighbors) = adj.get(current) else {
        return false;
    };
    let mut neighbors: Vec<&'a str> = neighbors
        .iter()
        .filter_map(|n| scc_set.get(n.as_str()).copied())
        .collect();
    neighbors.sort_unstable();

    for neighbor in neighbors {
        if neighbor == start && path.len() > 1 {
            path.push(start);
            return true;
        }
        if !seen.insert(neighbor) {
            continue;
        }
        path.push(neighbor);
        if dfs_cycle_path(neighbor, start, scc_set, adj, path, seen) {
            return true;
        }
        path.pop();
        seen.remove(neighbor);
    }
    false
}

/// Handles `tracedecay_complexity` tool calls.
pub(super) async fn handle_complexity(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let node_kind = args
        .get("node_kind")
        .and_then(|v| v.as_str())
        .and_then(NodeKind::from_str);

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);

    let path_prefix = effective_path(&args, scope_prefix);

    let results = cg
        .get_complexity_ranked(node_kind.as_ref(), path_prefix, limit)
        .await?;

    let touched_files =
        unique_file_paths(results.iter().map(|(n, _, _, _, _)| n.file_path.as_str()));

    let items: Vec<Value> = results
        .iter()
        .map(|(node, lines, fan_out, fan_in, score)| {
            json!({
                "id": node.id,
                "name": node.name,
                "kind": node.kind.as_str(),
                "file": node.file_path,
                "line": node.start_line,
                "lines": lines,
                "cyclomatic_complexity": node.branches + 1,
                "branches": node.branches,
                "loops": node.loops,
                "returns": node.returns,
                "max_nesting": node.max_nesting,
                "unsafe_blocks": node.unsafe_blocks,
                "unchecked_calls": node.unchecked_calls,
                "assertions": node.assertions,
                "fan_out": fan_out,
                "fan_in": fan_in,
                "score": score,
            })
        })
        .collect();

    let output = json!({
        "formula": "lines + (fan_out × 3) + fan_in",
        "note": "cyclomatic_complexity = branches + 1 (computed from AST during extraction)",
        "result_count": items.len(),
        "ranking": items,
    });

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
        || render::generic_md(&output),
    ))
}

/// Handles `tracedecay_doc_coverage` tool calls.
pub(super) async fn handle_doc_coverage(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let path_prefix = effective_path(&args, scope_prefix);

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(50, |v| v.min(500) as usize);

    let results = cg
        .get_undocumented_public_symbols(path_prefix, limit)
        .await?;

    let touched_files = unique_file_paths(results.iter().map(|n| n.file_path.as_str()));

    // Group by file for readability
    let mut by_file: HashMap<String, Vec<Value>> = HashMap::new();
    for node in &results {
        by_file
            .entry(node.file_path.clone())
            .or_default()
            .push(json!({
                "id": node.id,
                "name": node.name,
                "kind": node.kind.as_str(),
                "line": node.start_line,
                "signature": node.signature,
            }));
    }

    let mut file_items: Vec<Value> = by_file
        .into_iter()
        .map(|(file, symbols)| {
            json!({
                "file": file,
                "count": symbols.len(),
                "symbols": symbols,
            })
        })
        .collect();
    file_items.sort_by(|a, b| b["count"].as_u64().cmp(&a["count"].as_u64()));

    let output = json!({
        "path_filter": path_prefix,
        "total_undocumented": results.len(),
        "file_count": file_items.len(),
        "files": file_items,
    });

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
        || render::generic_md(&output),
    ))
}

/// Handles `tracedecay_god_class` tool calls.
pub(super) async fn handle_god_class(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);

    let path_prefix = effective_path(&args, scope_prefix);

    let results = cg.get_god_classes(path_prefix, limit).await?;

    let touched_files = unique_file_paths(results.iter().map(|(n, _, _, _)| n.file_path.as_str()));

    let items: Vec<Value> = results
        .iter()
        .map(|(node, methods, fields, total)| {
            json!({
                "id": node.id,
                "name": node.name,
                "kind": node.kind.as_str(),
                "file": node.file_path,
                "line": node.start_line,
                "methods": methods,
                "fields": fields,
                "total_members": total,
            })
        })
        .collect();

    let output = json!({
        "result_count": items.len(),
        "ranking": items,
    });

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
        || render::generic_md(&output),
    ))
}

// ---------------------------------------------------------------------------
// tracedecay_unsafe_patterns
// ---------------------------------------------------------------------------

const UNSAFE_KINDS: &[&str] = &[
    "unwrap",
    "expect",
    "panic",
    "todo",
    "unimplemented",
    "unsafe_block",
];

fn line_matches_unsafe_kind(line: &str, kind: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with("///") {
        return false;
    }
    match kind {
        "unwrap" => contains_method_call(line, "unwrap", true),
        "expect" => contains_method_call(line, "expect", false),
        "panic" => line.contains("panic!("),
        "todo" => line.contains("todo!("),
        "unimplemented" => line.contains("unimplemented!(") || line.contains("unimplemented!()"),
        "unsafe_block" => contains_unsafe_block_start(line),
        _ => false,
    }
}

fn contains_method_call(line: &str, method: &str, empty_parens: bool) -> bool {
    let needle = format!(".{method}");
    let bytes = line.as_bytes();
    let mut start = 0usize;
    while let Some(pos) = line[start..].find(&needle) {
        let abs = start + pos;
        let after = abs + needle.len();
        let next = bytes.get(after).copied();
        let is_word_boundary = !matches!(next, Some(c) if c.is_ascii_alphanumeric() || c == b'_');
        if is_word_boundary && next == Some(b'(') {
            if empty_parens {
                if line[after + 1..].trim_start().starts_with(')') {
                    return true;
                }
            } else {
                return true;
            }
        }
        start = abs + needle.len();
    }
    false
}

fn contains_unsafe_block_start(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut start = 0usize;
    while let Some(pos) = line[start..].find("unsafe") {
        let abs = start + pos;
        let prev_ok =
            abs == 0 || !matches!(bytes[abs - 1], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_');
        let after = abs + "unsafe".len();
        let next = bytes.get(after).copied();
        let next_ok = matches!(next, Some(b' ' | b'\t' | b'{'));
        if prev_ok && next_ok {
            let rest = line[after..].trim_start();
            if rest.starts_with('{')
                || rest.starts_with("fn ")
                || rest.starts_with("impl ")
                || rest.starts_with("trait ")
            {
                return true;
            }
        }
        start = abs + "unsafe".len();
    }
    false
}

fn path_looks_like_test(path: &str) -> bool {
    path.starts_with("tests/")
        || path.contains("/tests/")
        || path.ends_with("_test.rs")
        || path.ends_with("_tests.rs")
        || path.ends_with("_test.go")
        || path.contains("/__tests__/")
        || path.ends_with(".test.ts")
        || path.ends_with(".test.tsx")
        || path.ends_with(".test.js")
        || path.ends_with("_test.py")
        || path.ends_with("Test.java")
}

pub(super) async fn handle_unsafe_patterns(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let kinds: Vec<String> = args
        .get("kinds")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .filter(|v: &Vec<String>| !v.is_empty())
        .unwrap_or_else(|| UNSAFE_KINDS.iter().map(|s| (*s).to_string()).collect());

    let path = effective_path(&args, scope_prefix);
    let exclude_tests = args
        .get("exclude_tests")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(200, |v| v.min(2000) as usize);

    let project_root = cg.project_root();
    let files = cg.get_all_files().await?;
    let mut matches: Vec<Value> = Vec::new();
    let mut by_kind: HashMap<String, u64> = HashMap::new();
    let mut touched: Vec<String> = Vec::new();

    'outer: for file in &files {
        if !path_matches_optional_scope(&file.path, path) {
            continue;
        }
        let in_test = path_looks_like_test(&file.path);
        if exclude_tests && in_test {
            continue;
        }
        let abs_path = project_root.join(&file.path);
        let Ok(source) = crate::sync::read_source_file(&abs_path) else {
            continue;
        };
        // Blank comments and string/char literals for Rust files so an
        // `unsafe`/`unwrap`/`panic!` mentioned inside a comment or string is not
        // reported as a real risk site. Detection runs on the masked copy;
        // the original line is kept for the emitted snippet. Non-Rust files are
        // scanned raw (the Rust grammar would mis-tokenise them).
        let masked = if path_is_rust(&file.path) {
            tracedecay_code_extraction::source_mask::masked_rust_source_with(
                &source,
                tracedecay_code_extraction::source_mask::MaskOptions::CODE_SCAN,
            )
        } else {
            source.clone()
        };
        let nodes = cg.get_nodes_by_file(&file.path).await?;

        for (idx, (line, masked_line)) in source.lines().zip(masked.lines()).enumerate() {
            let line_no = (idx as u32) + 1;
            for kind in &kinds {
                if line_matches_unsafe_kind(masked_line, kind) {
                    let enclosing = nodes
                        .iter()
                        .filter(|n| n.start_line <= line_no && line_no <= n.end_line)
                        .min_by_key(|n| n.end_line.saturating_sub(n.start_line))
                        .map(|n| n.qualified_name.clone());
                    *by_kind.entry(kind.clone()).or_insert(0) += 1;
                    matches.push(json!({
                        "kind": kind,
                        "file": file.path,
                        "line": line_no,
                        "snippet": line.trim(),
                        "enclosing": enclosing,
                        "in_test": in_test,
                    }));
                    if !touched.contains(&file.path) {
                        touched.push(file.path.clone());
                    }
                    if matches.len() >= limit {
                        break 'outer;
                    }
                }
            }
        }
    }

    let counts = serde_json::to_value(&by_kind).unwrap_or(json!({}));
    let payload = json!({
        "match_count": matches.len(),
        "by_kind": counts,
        "matches": matches,
    });
    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &payload,
        touched,
        || render::risky_patterns_md(&payload),
    ))
}

// ---------------------------------------------------------------------------
// tracedecay_diagnostics
// ---------------------------------------------------------------------------

fn diagnostics_scope_arg(args: &Value) -> Result<(&str, crate::diagnostics::Scope)> {
    use crate::diagnostics::Scope;

    let scope_str = args
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("workspace");

    let scope = match scope_str {
        "workspace" => Scope::Workspace,
        "package" => Scope::Package {
            name: required_diagnostics_scope_value(args, "package", "name")?,
        },
        "file" => Scope::File {
            path: required_diagnostics_scope_value(args, "file", "path")?,
        },
        other => {
            return Err(TraceDecayError::Config {
                message: format!("unknown scope '{other}'; expected workspace, package, or file"),
            });
        }
    };

    Ok((scope_str, scope))
}

fn required_diagnostics_scope_value(args: &Value, scope: &str, name: &str) -> Result<String> {
    args.get(name)
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("scope='{scope}' requires a '{name}' argument"),
        })
        .map(str::to_string)
}

async fn enclosing_diagnostic_node(
    cg: &TraceDecay,
    spans_by_file: &mut HashMap<String, Vec<NodeSpan>>,
    file: &str,
    line_start: u32,
) -> Result<Option<String>> {
    if !spans_by_file.contains_key(file) {
        let spans = cg
            .get_nodes_by_file(file)
            .await?
            .into_iter()
            .map(|n| NodeSpan {
                start_line: n.start_line,
                end_line: n.end_line,
                qualified_name: n.qualified_name,
            })
            .collect();
        spans_by_file.insert(file.to_string(), spans);
    }

    Ok(spans_by_file
        .get(file)
        .and_then(|spans| enclosing_node_for_line(spans, line_start)))
}

/// Whether the diagnostics prewarm behaviour is enabled. Off by default: the
/// first `tracedecay_diagnostics` call on a cold Rust tree otherwise blocks for
/// minutes while cargo builds every dependency, which agents rationally avoid.
/// When enabled by the pinned resolved configuration snapshot, a cold tree
/// instead kicks a detached `cargo check` and returns a `warming` status
/// immediately. Legacy environment precedence is resolved before the snapshot
/// is published, never in this request path.
fn diagnostics_prewarm_enabled(config_flag: bool) -> bool {
    config_flag
}

/// Build the early-return `warming` payload for a cold prewarm. Factored out so
/// the warming path is unit-testable without spawning cargo.
fn diagnostics_warming_result(project_root: &std::path::Path, args: &Value) -> ToolResult {
    let target_dir = crate::diagnostics::rust_diagnostics_target_dir(project_root);
    let payload = json!({
        "status": "warming",
        "message": format!(
            "dependency build started (~minutes); re-call tracedecay_diagnostics after \
             it finishes, or run `cargo check` in your shell meanwhile. Build target: {}",
            target_dir.display()
        ),
        "target_dir": target_dir.display().to_string(),
        "diagnostic_count": 0,
    });
    rendered_tool_result(Some(project_root), args, &payload, vec![], || {
        render::generic_md(&payload)
    })
}

/// Best-effort per-project session↔git correlation index health for the
/// diagnostics payload. Read-only and fail-open: a missing or unopenable store,
/// or absent correlation tables, is reported as an explicitly *empty* index
/// (with a remediation notice) rather than omitted — so an unpopulated
/// `session_git_spans` (which makes `tracedecay_sessions_for` silently return
/// nothing) is always visible here.
async fn session_correlation_health_json(
    session_db: Option<&crate::global_db::RegisteredGlobalDb>,
) -> Value {
    let health = match session_db {
        Some(db) => crate::store::GlobalDbGitCorrelationStore::new(db)
            .correlation_index_health()
            .await
            .ok(),
        None => None,
    };
    match health {
        Some(health) if health.tables_present => {
            let empty = health.is_empty();
            json!({
                "tables_present": true,
                "span_count": health.span_count,
                "commit_count": health.commit_count,
                "last_span_write": health.last_span_write,
                "backfill_watermark": health.backfill_watermark,
                "index_empty": empty,
                "notice": if empty {
                    "correlation index empty — `tracedecay_sessions_for` will return nothing until it is populated; it auto-backfills on the next MCP server startup, or run `tracedecay sessions git-backfill` to populate it now"
                } else {
                    "correlation index populated"
                },
            })
        }
        _ => json!({
            "tables_present": false,
            "span_count": 0,
            "commit_count": 0,
            "last_span_write": Value::Null,
            "backfill_watermark": Value::Null,
            "index_empty": true,
            "notice": "correlation index not yet created — `tracedecay_sessions_for` will return nothing until it is populated; it auto-backfills on the next MCP server startup, or run `tracedecay sessions git-backfill` to populate it now",
        }),
    }
}

pub(super) async fn handle_diagnostics(
    cg: &TraceDecay,
    args: Value,
    diagnostics_cache: Option<&crate::diagnostics::DiagnosticsCache>,
    diagnostics_lsp: Option<&tokio::sync::Mutex<DiagnosticBroker>>,
    session_db: Option<&crate::global_db::RegisteredGlobalDb>,
) -> Result<ToolResult> {
    use crate::diagnostics::run_all;

    let (scope_str, scope) = diagnostics_scope_arg(&args)?;
    let project_root = cg.project_root().to_path_buf();

    // Cold-start avoidance: on a fresh tree the first cargo check builds every
    // dependency and blocks for minutes. When prewarm is enabled, spawn that
    // build detached and return a `warming` status immediately so the agent can
    // keep working and re-call once it is warm. Default-off preserves the
    // original blocking behaviour for callers who want the answer inline.
    if diagnostics_prewarm_enabled(cg.get_config().diagnostics_prewarm)
        && crate::diagnostics::is_rust_diagnostics_cold(&project_root)
    {
        crate::diagnostics::spawn_rust_diagnostics_prewarm(&project_root)?;
        return Ok(diagnostics_warming_result(&project_root, &args));
    }

    let mut diagnostics =
        if let Some(lsp_diagnostics) = lsp_file_diagnostics(cg, &scope, diagnostics_lsp).await? {
            lsp_diagnostics
        } else if let Some(cache) = diagnostics_cache {
            cache.run(&project_root, &scope).await?
        } else {
            run_all(&project_root, &scope).await?
        };

    if let crate::diagnostics::Scope::File { path } = &scope {
        diagnostics.retain(|d| d.file == *path);
    }

    let mut entries: Vec<Value> = Vec::with_capacity(diagnostics.len());
    let mut error_count = 0u64;
    let mut warning_count = 0u64;
    let mut spans_by_file: HashMap<String, Vec<NodeSpan>> = HashMap::new();

    for diag in &diagnostics {
        match diag.level.as_str() {
            "error" => error_count += 1,
            "warning" => warning_count += 1,
            _ => {}
        }

        let enclosing =
            enclosing_diagnostic_node(cg, &mut spans_by_file, &diag.file, diag.line_start).await?;

        entries.push(json!({
            "file": diag.file,
            "line_start": diag.line_start,
            "line_end": diag.line_end,
            "level": diag.level,
            "code": diag.code,
            "message": diag.message,
            "driver": diag.driver,
            "enclosing": enclosing,
        }));
    }

    let payload = json!({
        "scope": scope_str,
        "diagnostic_count": entries.len(),
        "error_count": error_count,
        "warning_count": warning_count,
        "diagnostics": entries,
        "session_correlation": session_correlation_health_json(session_db).await,
    });
    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &payload,
        unique_file_paths(diagnostics.iter().map(|d| d.file.as_str())),
        || render::generic_md(&payload),
    ))
}

async fn lsp_file_diagnostics(
    cg: &TraceDecay,
    scope: &crate::diagnostics::Scope,
    diagnostics_lsp: Option<&tokio::sync::Mutex<DiagnosticBroker>>,
) -> Result<Option<Vec<crate::diagnostics::Diagnostic>>> {
    let crate::diagnostics::Scope::File { path } = scope else {
        return Ok(None);
    };
    let Some(diagnostics_lsp) = diagnostics_lsp else {
        return Ok(None);
    };

    let adapter = {
        let broker = diagnostics_lsp.lock().await;
        broker
            .snapshot()
            .engines
            .into_iter()
            .filter_map(|engine| broker.adapter_for(&engine.language))
            .find(|adapter| {
                active_languages_for_files(
                    cg.project_root(),
                    std::slice::from_ref(adapter),
                    std::slice::from_ref(path),
                )
                .contains(&adapter.language)
            })
    };
    let Some(adapter) = adapter else {
        return Ok(None);
    };
    let language = adapter.language.clone();
    let documents = documents_for_adapter(cg.project_root(), &adapter, vec![path.clone()]).await?;
    if documents.is_empty() {
        return Ok(None);
    }

    let snapshot = {
        let mut broker = diagnostics_lsp.lock().await;
        if broker
            .refresh_documents(&language, documents, Duration::from_secs(2))
            .await
            .is_err()
        {
            return Ok(None);
        }
        broker.snapshot()
    };

    Ok(Some(
        snapshot
            .diagnostics
            .into_iter()
            .filter(|diagnostic| diagnostic.file == *path)
            .map(lsp_diagnostic_to_compiler_diagnostic)
            .collect(),
    ))
}

fn lsp_diagnostic_to_compiler_diagnostic(
    diagnostic: CodeDiagnostic,
) -> crate::diagnostics::Diagnostic {
    crate::diagnostics::Diagnostic {
        file: diagnostic.file,
        line_start: diagnostic.line_start,
        line_end: diagnostic.line_end,
        level: match diagnostic.severity {
            BrokerDiagnosticSeverity::Error => "error",
            BrokerDiagnosticSeverity::Warning => "warning",
            BrokerDiagnosticSeverity::Information => "information",
            BrokerDiagnosticSeverity::Hint => "hint",
        }
        .to_string(),
        code: diagnostic.code.unwrap_or_default(),
        message: diagnostic.message,
        driver: "lsp",
    }
}

// ---------------------------------------------------------------------------
// tracedecay_constructors
// ---------------------------------------------------------------------------

pub(super) async fn handle_constructors(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let struct_name =
        args.get("struct")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "tracedecay_constructors requires a 'struct' argument".to_string(),
            })?;
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(100, |v| v.clamp(1, 1000) as usize);

    let candidates = cg
        .db()
        .search_nodes_by_exact_name(&[struct_name.to_string()], 50)
        .await?;
    let struct_nodes: Vec<&crate::types::Node> = candidates
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::Struct | NodeKind::Class | NodeKind::CaseClass
            )
        })
        .collect();

    if struct_nodes.is_empty() {
        let payload = json!({
            "found": false,
            "struct": struct_name,
            "message": format!("No struct, class, or case-class named '{struct_name}' found."),
            "match_count": 0,
            "sites": [],
        });
        return Ok(rendered_tool_result(
            Some(cg.project_root()),
            &args,
            &payload,
            vec![],
            || render::generic_md(&payload),
        ));
    }

    let mut expected_fields: HashSet<String> = HashSet::new();
    for sn in &struct_nodes {
        let children = cg.db().get_children_of(&sn.id).await?;
        for child in children {
            if matches!(
                child.kind,
                NodeKind::Field | NodeKind::ValField | NodeKind::VarField
            ) {
                expected_fields.insert(child.name);
            }
        }
    }

    let project_root = cg.project_root();
    let files = cg.get_all_files().await?;
    let mut sites: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();

    'outer: for file in &files {
        if !path_matches_optional_scope(&file.path, scope_prefix) {
            continue;
        }
        let abs = project_root.join(&file.path);
        let Ok(source) = crate::sync::read_source_file(&abs) else {
            continue;
        };

        for site in find_struct_literals(&source, struct_name) {
            let field_list = parse_literal_fields(&source, site.brace_open_byte);
            let missing: Vec<String> = if expected_fields.is_empty() {
                Vec::new()
            } else {
                expected_fields
                    .iter()
                    .filter(|f| !field_list.contains(f))
                    .cloned()
                    .collect()
            };
            if !touched.contains(&file.path) {
                touched.push(file.path.clone());
            }
            sites.push(json!({
                "file": file.path,
                "line": site.line,
                "fields": field_list,
                "missing_fields": missing,
            }));
            if sites.len() >= limit {
                break 'outer;
            }
        }
    }

    let payload = json!({
        "struct": struct_name,
        "expected_fields": expected_fields.iter().cloned().collect::<Vec<_>>(),
        "match_count": sites.len(),
        "sites": sites,
    });
    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &payload,
        touched,
        || render::generic_md(&payload),
    ))
}

#[derive(Debug, Clone, Copy)]
struct LiteralSite {
    line: u32,
    brace_open_byte: usize,
}

fn find_struct_literals(source: &str, struct_name: &str) -> Vec<LiteralSite> {
    let bytes = source.as_bytes();
    let mut pattern_stack: Vec<i32> = Vec::new();
    let mut depth: i32 = 0;
    let mut string_delim: Option<u8> = None;
    let mut prev_was_backslash = false;
    let mut out: Vec<LiteralSite> = Vec::new();
    let mut byte = 0usize;
    let n = bytes.len();
    while byte < n {
        let b = bytes[byte];

        if let Some(delim) = string_delim {
            if !prev_was_backslash && b == delim {
                string_delim = None;
                prev_was_backslash = false;
                byte += 1;
                continue;
            }
            prev_was_backslash = !prev_was_backslash && b == b'\\';
            byte += 1;
            continue;
        }
        if b == b'"' {
            string_delim = Some(b'"');
            prev_was_backslash = false;
            byte += 1;
            continue;
        }
        if b == b'\'' {
            let after = bytes.get(byte + 1).copied();
            if matches!(after, Some(b'a'..=b'z' | b'A'..=b'Z' | b'_')) {
                let mut probe = byte + 1;
                while let Some(c) = bytes.get(probe) {
                    if matches!(c, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_') {
                        probe += 1;
                    } else {
                        break;
                    }
                }
                if bytes.get(probe).copied() != Some(b'\'') {
                    byte += 1;
                    continue;
                }
            }
            string_delim = Some(b'\'');
            prev_was_backslash = false;
            byte += 1;
            continue;
        }

        if matches_word(bytes, byte, b"match") {
            pattern_stack.push(depth);
            byte += "match".len();
            continue;
        }
        if matches_word(bytes, byte, b"if") && lookahead_let(bytes, byte + 2) {
            pattern_stack.push(depth);
            byte += "if".len();
            continue;
        }
        if matches_word(bytes, byte, b"while") && lookahead_let(bytes, byte + 5) {
            pattern_stack.push(depth);
            byte += "while".len();
            continue;
        }

        if b == b'{' {
            depth += 1;
            byte += 1;
            continue;
        }
        if b == b'}' {
            depth -= 1;
            if let Some(&entered_at) = pattern_stack.last()
                && depth == entered_at
            {
                pattern_stack.pop();
            }
            byte += 1;
            continue;
        }

        if matches_word(bytes, byte, struct_name.as_bytes()) {
            let start = byte;
            let end = start + struct_name.len();

            let mut probe = end;
            while let Some(c) = bytes.get(probe) {
                if c.is_ascii_whitespace() {
                    probe += 1;
                } else {
                    break;
                }
            }
            if bytes.get(probe).copied() != Some(b'{') {
                byte = end;
                continue;
            }
            if has_disqualifying_prefix(source, start) {
                byte = end;
                continue;
            }
            if !pattern_stack.is_empty() {
                byte = end;
                continue;
            }
            let line = source[..start].bytes().filter(|c| *c == b'\n').count() as u32 + 1;
            out.push(LiteralSite {
                line,
                brace_open_byte: probe,
            });
            byte = probe + 1;
            continue;
        }

        byte += 1;
    }
    out
}

fn lookahead_let(bytes: &[u8], at: usize) -> bool {
    let mut probe = at;
    while let Some(b) = bytes.get(probe) {
        if b.is_ascii_whitespace() {
            probe += 1;
        } else {
            break;
        }
    }
    matches_word(bytes, probe, b"let")
}

fn matches_word(bytes: &[u8], at: usize, needle: &[u8]) -> bool {
    if at + needle.len() > bytes.len() {
        return false;
    }
    if &bytes[at..at + needle.len()] != needle {
        return false;
    }
    let left_ok = at == 0
        || !matches!(
            bytes[at - 1],
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_'
        );
    let right_ok = match bytes.get(at + needle.len()) {
        None => true,
        Some(b) => !matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_'),
    };
    left_ok && right_ok
}

fn has_disqualifying_prefix(source: &str, idx: usize) -> bool {
    let bytes = source.as_bytes();
    let mut probe = idx;
    while probe > 0 && bytes[probe - 1].is_ascii_whitespace() {
        probe -= 1;
    }
    if probe == 0 {
        return false;
    }
    if probe >= 2 && &bytes[probe - 2..probe] == b"->" {
        return true;
    }
    let id_end = probe;
    let mut id_start = probe;
    while id_start > 0
        && matches!(
            bytes[id_start - 1],
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_'
        )
    {
        id_start -= 1;
    }
    if id_start == id_end {
        return false;
    }
    let token = &source[id_start..id_end];
    matches!(
        token,
        "struct" | "enum" | "union" | "impl" | "trait" | "type"
    )
}

fn parse_literal_fields(source: &str, open_byte: usize) -> Vec<String> {
    let bytes = source.as_bytes();
    if bytes.get(open_byte).copied() != Some(b'{') {
        return Vec::new();
    }
    let mut depth = 0i32;
    let mut close_byte = None;
    for (i, b) in bytes.iter().enumerate().skip(open_byte) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    close_byte = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close_byte else {
        return Vec::new();
    };
    let body = &source[open_byte + 1..close];

    let mut fields: Vec<String> = Vec::new();
    let mut depth_brace = 0i32;
    let mut depth_paren = 0i32;
    let mut current = String::new();
    for c in body.chars() {
        match c {
            '{' | '[' => depth_brace += 1,
            '}' | ']' => depth_brace -= 1,
            '(' => depth_paren += 1,
            ')' => depth_paren -= 1,
            ',' if depth_brace == 0 && depth_paren == 0 => {
                if let Some(name) = field_name_from_chunk(&current) {
                    fields.push(name);
                }
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    if let Some(name) = field_name_from_chunk(&current) {
        fields.push(name);
    }
    fields
}

fn field_name_from_chunk(chunk: &str) -> Option<String> {
    let trimmed = chunk.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("..") || trimmed.starts_with("//") {
        return None;
    }
    let name_end = trimmed
        .find(|c: char| c == ':' || c == ',' || c.is_whitespace())
        .unwrap_or(trimmed.len());
    let name = &trimmed[..name_end];
    if name.is_empty() {
        return None;
    }
    if !name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        return None;
    }
    Some(name.to_string())
}

// ---------------------------------------------------------------------------
// tracedecay_field_sites
// ---------------------------------------------------------------------------

pub(super) async fn handle_field_sites(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let raw =
        args.get("field")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "tracedecay_field_sites requires a 'field' argument".to_string(),
            })?;
    let writes_only = args
        .get("writes_only")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(200, |v| v.clamp(1, 2000) as usize);

    let (qualifier, field_name) = match raw.rsplit_once("::") {
        Some((q, f)) => (Some(q.to_string()), f.to_string()),
        None => (None, raw.to_string()),
    };

    let project_root = cg.project_root();
    let files = cg.get_all_files().await?;
    let mut writes: Vec<Value> = Vec::new();
    let mut reads: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();

    'outer: for file in &files {
        if !path_matches_optional_scope(&file.path, scope_prefix) {
            continue;
        }
        let abs = project_root.join(&file.path);
        let Ok(source) = crate::sync::read_source_file(&abs) else {
            continue;
        };
        let nodes = cg.get_nodes_by_file(&file.path).await?;

        for site in find_field_references(&source, &field_name) {
            let line_text = line_at(&source, site.byte).unwrap_or("");
            let enclosing = nodes
                .iter()
                .filter(|n| n.start_line <= site.line && site.line <= n.end_line)
                .min_by_key(|n| n.end_line.saturating_sub(n.start_line))
                .map(|n| n.qualified_name.clone());
            let entry = json!({
                "file": file.path,
                "line": site.line,
                "enclosing": enclosing,
                "snippet": line_text.trim(),
            });
            if !touched.contains(&file.path) {
                touched.push(file.path.clone());
            }
            match site.kind {
                FieldRefKind::Write => {
                    writes.push(entry);
                    if writes.len() >= limit && (writes_only || reads.len() >= limit) {
                        break 'outer;
                    }
                }
                FieldRefKind::Read => {
                    if writes_only {
                        continue;
                    }
                    reads.push(entry);
                    if reads.len() >= limit && writes.len() >= limit {
                        break 'outer;
                    }
                }
            }
        }
    }

    let qualifier_applied = false;
    let payload = if writes_only {
        json!({
            "field": raw,
            "qualifier": qualifier,
            "qualifier_applied": qualifier_applied,
            "write_count": writes.len(),
            "write_sites": writes,
        })
    } else {
        json!({
            "field": raw,
            "qualifier": qualifier,
            "qualifier_applied": qualifier_applied,
            "write_count": writes.len(),
            "read_count": reads.len(),
            "write_sites": writes,
            "read_sites": reads,
        })
    };
    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &payload,
        touched,
        || render::generic_md(&payload),
    ))
}

#[derive(Debug, Clone, Copy)]
enum FieldRefKind {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy)]
struct FieldSite {
    byte: usize,
    line: u32,
    kind: FieldRefKind,
}

fn find_field_references(source: &str, field: &str) -> Vec<FieldSite> {
    let bytes = source.as_bytes();
    let needle = format!(".{field}");
    let mut out: Vec<FieldSite> = Vec::new();
    let mut byte = 0usize;
    while let Some(rel) = source[byte..].find(&needle) {
        let dot = byte + rel;
        let name_start = dot + 1;
        let name_end = name_start + field.len();
        let right_ok = match bytes.get(name_end) {
            None => true,
            Some(b) => !matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_'),
        };
        if !right_ok {
            byte = name_end;
            continue;
        }
        if line_is_comment(source, dot) {
            byte = name_end;
            continue;
        }

        let line = source[..dot].bytes().filter(|c| *c == b'\n').count() as u32 + 1;
        let kind = classify_field_reference(source, name_end);
        out.push(FieldSite {
            byte: name_end,
            line,
            kind,
        });
        byte = name_end;
    }
    out
}

fn classify_field_reference(source: &str, after_name: usize) -> FieldRefKind {
    let bytes = source.as_bytes();
    let mut probe = after_name;
    while let Some(b) = bytes.get(probe) {
        if *b == b' ' || *b == b'\t' {
            probe += 1;
        } else {
            break;
        }
    }

    if let Some(b'\n') = bytes.get(probe).copied() {
        probe += 1;
        while let Some(b) = bytes.get(probe) {
            if *b == b' ' || *b == b'\t' {
                probe += 1;
            } else {
                break;
            }
        }
    }

    let next = bytes.get(probe).copied();
    let next2 = bytes.get(probe + 1).copied();
    match (next, next2) {
        (Some(b'='), Some(b'=' | b'>')) => FieldRefKind::Read,
        (Some(b'='), _) => FieldRefKind::Write,
        (Some(b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^'), Some(b'=')) => {
            FieldRefKind::Write
        }
        (Some(b'<'), Some(b'<')) | (Some(b'>'), Some(b'>')) => {
            if bytes.get(probe + 2).copied() == Some(b'=') {
                FieldRefKind::Write
            } else {
                FieldRefKind::Read
            }
        }
        _ => {
            if has_mut_borrow_prefix(source, after_name.saturating_sub(1)) {
                FieldRefKind::Write
            } else {
                FieldRefKind::Read
            }
        }
    }
}

fn has_mut_borrow_prefix(source: &str, idx: usize) -> bool {
    let bytes = source.as_bytes();
    let mut probe = idx;
    while probe > 0
        && matches!(
            bytes[probe],
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'.' | b':' | b'?'
        )
    {
        probe -= 1;
    }
    while probe > 0 && bytes[probe].is_ascii_whitespace() {
        probe -= 1;
    }
    if probe < 4 {
        return false;
    }
    let window = &source[probe.saturating_sub(4)..=probe];
    window.ends_with("&mut")
}

fn line_at(source: &str, byte: usize) -> Option<&str> {
    let line_start = source[..byte].rfind('\n').map_or(0, |i| i + 1);
    let line_end = source[byte..].find('\n').map_or(source.len(), |i| byte + i);
    source.get(line_start..line_end)
}

fn line_is_comment(source: &str, byte: usize) -> bool {
    let line_start = source[..byte].rfind('\n').map_or(0, |i| i + 1);
    let line = &source[line_start..];
    let trimmed = line.trim_start();
    trimmed.starts_with("//")
}

#[cfg(test)]
mod circular_render_tests {
    use super::{
        CIRCULAR_DEFAULT_MEMBER_LIMIT, CIRCULAR_MAX_LIMIT, bound_cycles, circular_output,
        render_circular_md,
    };

    /// Mirrors [`crate::mcp::tools::MAX_RESPONSE_CHARS`], the point at which a
    /// response is replaced by a preview envelope plus a retrieval handle.
    const RESPONSE_BUDGET: usize = 15_000;

    fn cycle(files: &[&str]) -> Vec<String> {
        files.iter().map(|file| (*file).to_string()).collect()
    }

    fn bounded(files: &[&str], member_limit: usize) -> Vec<super::BoundedCycle> {
        bound_cycles(vec![cycle(files)], 1, member_limit).0
    }

    #[test]
    fn renders_arrow_chain_closing_the_loop() {
        let cycles = bounded(&["a.rs", "b.rs"], CIRCULAR_DEFAULT_MEMBER_LIMIT);
        let out = render_circular_md(&cycles, 1, 0, 25);
        assert!(out.contains("a.rs -> b.rs -> a.rs"), "got: {out}");
        assert!(out.contains("Circular Dependencies (1)"), "got: {out}");
    }

    #[test]
    fn renders_empty_state() {
        let out = render_circular_md(&[], 0, 0, 25);
        assert!(out.contains("No circular dependencies found"), "got: {out}");
    }

    #[test]
    fn numbers_multiple_cycles() {
        let (cycles, _) = bound_cycles(
            vec![cycle(&["c.rs", "d.rs", "e.rs"]), cycle(&["a.rs", "b.rs"])],
            2,
            CIRCULAR_DEFAULT_MEMBER_LIMIT,
        );
        let out = render_circular_md(&cycles, 2, 0, 25);
        assert!(
            out.contains("1. c.rs -> d.rs -> e.rs -> c.rs"),
            "got: {out}"
        );
        assert!(out.contains("2. a.rs -> b.rs -> a.rs"), "got: {out}");
    }

    #[test]
    fn bounded_page_keeps_the_largest_cycles_and_counts_the_rest() {
        let cycles = vec![
            cycle(&["small-b.rs", "small-b2.rs"]),
            cycle(&["big.rs", "big2.rs", "big3.rs", "big4.rs"]),
            cycle(&["small-a.rs", "small-a2.rs"]),
        ];

        let (page, omitted) = bound_cycles(cycles, 2, CIRCULAR_DEFAULT_MEMBER_LIMIT);

        assert_eq!(omitted, 1, "the omitted cycle must be counted, not dropped");
        assert_eq!(page.len(), 2);
        assert_eq!(
            page[0].members,
            cycle(&["big.rs", "big2.rs", "big3.rs", "big4.rs"])
        );
        assert_eq!(page[0].member_count, 4);
        assert_eq!(page[0].omitted_member_count, 0);
        // Ties resolve by path order so repeated calls agree.
        assert_eq!(page[1].members, cycle(&["small-a.rs", "small-a2.rs"]));
    }

    #[test]
    fn unbounded_page_reports_no_omission() {
        let cycles = vec![cycle(&["a.rs", "b.rs"])];
        let (page, omitted) = bound_cycles(cycles, 25, CIRCULAR_DEFAULT_MEMBER_LIMIT);
        assert_eq!(omitted, 0);
        assert_eq!(page.len(), 1);
    }

    #[test]
    fn omission_notice_states_the_remainder_and_the_ceiling() {
        let cycles = bounded(&["a.rs", "b.rs"], CIRCULAR_DEFAULT_MEMBER_LIMIT);
        let out = render_circular_md(&cycles, 9, 8, 1);
        assert!(out.contains("Circular Dependencies (9)"), "got: {out}");
        assert!(
            out.contains("8 further cycle(s) not shown at limit 1"),
            "got: {out}"
        );
        assert!(out.contains(&CIRCULAR_MAX_LIMIT.to_string()), "got: {out}");
    }

    /// A single strongly connected component can hold hundreds of files. The
    /// declared bound must shape the answer before rendering, so both the JSON
    /// payload and the markdown stay inside the response budget and state the
    /// component's true size.
    #[test]
    fn wide_component_is_bounded_within_the_response_budget() {
        let members: Vec<String> = (0..400)
            .map(|index| {
                format!("crates/tracedecay-application/src/deeply/nested/module_{index:04}.rs")
            })
            .collect();
        let member_count = members.len();

        let (page, omitted) = bound_cycles(vec![members], 3, CIRCULAR_DEFAULT_MEMBER_LIMIT);

        assert_eq!(omitted, 0);
        assert_eq!(page[0].member_count, member_count);
        assert_eq!(page[0].members.len(), CIRCULAR_DEFAULT_MEMBER_LIMIT);
        assert_eq!(
            page[0].omitted_member_count,
            member_count - CIRCULAR_DEFAULT_MEMBER_LIMIT
        );

        let payload = circular_output(&page, 1, omitted, 3, CIRCULAR_DEFAULT_MEMBER_LIMIT);
        let serialized = serde_json::to_string_pretty(&payload).expect("payload serializes");
        assert!(
            serialized.len() <= RESPONSE_BUDGET,
            "bounded payload is {} chars, over the {RESPONSE_BUDGET} budget",
            serialized.len()
        );

        let markdown = render_circular_md(&page, 1, omitted, 3);
        assert!(
            markdown.len() <= RESPONSE_BUDGET,
            "bounded markdown is {} chars, over the {RESPONSE_BUDGET} budget",
            markdown.len()
        );
        assert!(
            markdown.contains("further member(s) not shown"),
            "the bounded member list must state its omission: {markdown}"
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod diagnostics_warming_tests {
    use super::{diagnostics_prewarm_enabled, diagnostics_warming_result};
    use serde_json::{Value, json};
    use std::path::Path;

    #[test]
    fn prewarm_follows_resolved_config_snapshot() {
        assert!(!diagnostics_prewarm_enabled(false));
        assert!(diagnostics_prewarm_enabled(true));
    }

    #[test]
    fn warming_result_reports_status_and_target_dir() {
        let root = Path::new("/tmp/tracedecay-warming-proj");
        let result = diagnostics_warming_result(root, &json!({}));
        let text = result.value["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("warming"),
            "status should be surfaced: {text}"
        );
        assert!(
            text.to_lowercase().contains("re-call"),
            "should tell the agent to re-call: {text}"
        );
        // The private diagnostics target dir is namespaced by project id, so the
        // message must not leak a repo-local `target/`.
        assert!(
            text.contains("tracedecay-target"),
            "should point at the private diagnostics target dir: {text}"
        );
        assert!(
            !text.trim_start().starts_with('{'),
            "default output should be Markdown: {text}"
        );

        let json_result = diagnostics_warming_result(root, &json!({ "format": "json" }));
        let Some(json_text) = json_result.value["content"][0]["text"].as_str() else {
            panic!("format=json should include text content");
        };
        let json_payload: Value = serde_json::from_str(json_text)
            .unwrap_or_else(|err| panic!("format=json should stay parseable JSON: {err}"));
        assert_eq!(json_payload["status"], "warming");
        assert_eq!(json_payload["diagnostic_count"], 0);
    }
}

#[cfg(test)]
mod unsafe_pattern_detection_tests {
    use super::{contains_unsafe_block_start, line_matches_unsafe_kind};

    #[test]
    fn detects_unsafe_block_inside_safe_fn() {
        // An `unsafe { }` block living inside an otherwise-safe function — the
        // exact shape the audit fixture plants.
        assert!(line_matches_unsafe_kind(
            "    unsafe { *ptr as usize }",
            "unsafe_block"
        ));
        assert!(contains_unsafe_block_start("    unsafe { *ptr as usize }"));
    }

    #[test]
    fn detects_unsafe_fn_impl_and_trait() {
        assert!(line_matches_unsafe_kind(
            "pub unsafe fn raw(&self) {",
            "unsafe_block"
        ));
        assert!(line_matches_unsafe_kind(
            "unsafe impl Send for Foo {}",
            "unsafe_block"
        ));
        assert!(line_matches_unsafe_kind(
            "unsafe trait Zeroable {}",
            "unsafe_block"
        ));
    }

    #[test]
    fn ignores_safe_code_and_comments() {
        // Plain safe code has no unsafe markers.
        assert!(!line_matches_unsafe_kind(
            "let x = total as usize;",
            "unsafe_block"
        ));
        // The word appears only in a comment/doc line: not a real unsafe site.
        assert!(!line_matches_unsafe_kind(
            "// this is not unsafe { } really",
            "unsafe_block"
        ));
        assert!(!line_matches_unsafe_kind(
            "/// drop the needless unsafe block",
            "unsafe_block"
        ));
        // A substring of a longer identifier must not trip the word-boundary check.
        assert!(!contains_unsafe_block_start("let unsafely = 1;"));
        assert!(!contains_unsafe_block_start("let make_unsafe_thing = 2;"));
    }
}

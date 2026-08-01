//! `tracedecay_unused_imports` — `use`-statement identifiers weighed against identifier spans in the rest of the file.

use super::*;

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
pub(crate) async fn handle_unused_imports(
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

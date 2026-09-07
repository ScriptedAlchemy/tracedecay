//! `tracedecay_unused_imports` — `use`-statement identifiers weighed against identifier spans in the rest of the file.

use std::collections::HashSet;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay_code_extraction::RustExtractor;
use tracedecay_domain::code_intelligence::{Node, NodeKind, Visibility};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_graph_query::VerifiedGraphQuery;

use super::{path_is_rust, verified_analysis_symbols};
use crate::ToolResult;
use crate::handlers::support::{rendered_tool_result, unique_file_paths};
use crate::tools::render;

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
///
/// The limit is a soft cap read at file granularity: a page stops after the
/// file that reaches it rather than cutting that file's findings in half. The
/// cursor names a finished file, so a mid-file cut would strand the rest of
/// that file's imports behind a cursor that has already moved past them.
const UNUSED_IMPORTS_DEFAULT_LIMIT: usize = 50;
const UNUSED_IMPORTS_MAX_LIMIT: usize = 500;

/// Files inspected in one call before the answer becomes a typed partial.
///
/// The scan reads and masks each candidate file's source, so an unbounded walk
/// over a large workspace never returns inside the caller's deadline. A file
/// budget keeps the call bounded and the response states the continuation
/// cursor rather than reporting a short list as the whole truth.
const UNUSED_IMPORTS_FILE_BUDGET: usize = 400;

/// Indexes every identifier in a masked source once, so each import's
/// reference check is a set lookup instead of a full-file scan.
#[hotpath::measure]
fn identifiers_in_source(source: &str) -> HashSet<String> {
    let mut identifiers = HashSet::new();
    for line in source.lines() {
        identifiers.extend(identifiers_in_line(line));
    }
    identifiers
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

/// Walks candidate files in path order, so `cursor` resumes the walk exactly
/// where the previous page stopped and `complete` reports whether the answer
/// covers the whole scope. `limit` cuts the walk between files rather than
/// between one file's findings, so a page can exceed it by the last file's
/// remainder and no finding is stranded behind the cursor.
#[hotpath::measure(future = true, label = "mcp.analysis.unused_imports.total")]
pub async fn handle_unused_imports(
    project_root: &Path,
    graph: &VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(UNUSED_IMPORTS_DEFAULT_LIMIT, |limit| {
            (limit as usize).clamp(1, UNUSED_IMPORTS_MAX_LIMIT)
        });
    let after_path = args
        .get("cursor")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let files = hotpath::measure_block!("mcp.analysis.unused_imports.graph", {
        verified_analysis_symbols(graph, scope_prefix)?
            .into_iter()
            .map(|symbol| symbol.path)
            .collect::<HashSet<_>>()
    });
    // Graph phase is done. Each candidate file is read and masked, so the
    // walk belongs on a blocking worker like the sibling analysis scans.
    let scan_project_root = project_root.to_path_buf();
    let (unused, touched, scanned_files, last_scanned, mut partial_reason) = hotpath::future!(
        tokio::task::spawn_blocking(move || -> Result<_> {
            let mut unused: Vec<Value> = Vec::new();
            let mut touched: Vec<String> = Vec::new();
            let mut scanned_files = 0usize;
            // The cursor is exclusive, so it must name the last file this call
            // finished. Advancing it past a file the call never inspected would
            // drop that file from every continuation.
            let mut last_scanned: Option<String> = None;
            let mut partial_reason: Option<&'static str> = None;
            let mut files = files.into_iter().collect::<Vec<_>>();
            files.sort();
            for file_path in files
                .into_iter()
                .filter(|path| after_path.as_ref().is_none_or(|after| path > after))
            {
                if scanned_files >= UNUSED_IMPORTS_FILE_BUDGET {
                    partial_reason = Some("file_budget_exhausted");
                    break;
                }
                scanned_files += 1;
                let file_unused = unused_imports_in_file(&scan_project_root, &file_path)?;
                if !file_unused.is_empty() {
                    touched.push(file_path.clone());
                }
                unused.extend(file_unused);
                last_scanned = Some(file_path);
                if unused.len() >= limit {
                    // The page may overshoot `limit`, and must: truncating here
                    // would drop the tail of the file `last_scanned` names, and
                    // the exclusive cursor would then skip that file forever.
                    // Whole files in, whole files out.
                    partial_reason = Some("limit_reached");
                    break;
                }
            }
            Ok((unused, touched, scanned_files, last_scanned, partial_reason))
        }),
        label = "mcp.analysis.unused_imports.scan"
    )
    .await
    .map_err(|join_error| TraceDecayError::Config {
        message: format!("tracedecay_unused_imports scan failed to join: {join_error}"),
    })??;
    // A partial answer without a resumable cursor would be a dead end, so a
    // stop that produced no inspected file is reported as complete coverage of
    // what the scope contains rather than a fabricated continuation.
    let next_cursor = partial_reason.and(last_scanned);
    if next_cursor.is_none() {
        partial_reason = None;
    }

    let touched_files = unique_file_paths(touched.iter().map(std::string::String::as_str));

    let output = hotpath::measure_block!(
        "mcp.analysis.unused_imports.assemble",
        json!({
            "unused_import_count": unused.len(),
            "imports": unused,
            "limit": limit,
            "scanned_files": scanned_files,
            "complete": partial_reason.is_none(),
            "partial_reason": partial_reason,
            "next_cursor": next_cursor,
        })
    );

    Ok(rendered_tool_result(
        Some(project_root),
        &args,
        &output,
        touched_files,
        || render::unused_imports_md(&output),
    ))
}

/// Reports the unused imports of one file.
///
/// The canonical Rust extractor discovers declarations directly from the
/// bounded source file, because a verified graph generation may omit `Use`
/// nodes. Comments and string/char literals are masked, then every parsed use
/// declaration is blanked before identifier indexing, so neither prose nor a
/// second import of the same name counts as a reference.
///
/// `pub use` re-exports are intentional public aliases and are never reported.
#[hotpath::measure]
fn unused_imports_in_file(project_root: &Path, file_path: &str) -> Result<Vec<Value>> {
    if !path_is_rust(file_path) {
        return Ok(Vec::new());
    }
    let Ok(source) = std::fs::read_to_string(project_root.join(file_path)) else {
        return Ok(Vec::new());
    };
    let extraction = RustExtractor::extract(file_path, &source);
    if let Some(error) = extraction.errors.first() {
        return Err(TraceDecayError::Config {
            message: format!("failed to parse {file_path} for unused imports: {error}"),
        });
    }
    let use_nodes = extraction
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Use)
        .collect::<Vec<_>>();
    if use_nodes.is_empty() {
        return Ok(Vec::new());
    }
    let masked = tracedecay_code_extraction::source_mask::masked_rust_source(&source);
    let referenced_identifiers =
        identifiers_in_source(&without_use_declarations(masked, &source, &use_nodes)?);

    let mut unused = Vec::new();
    for use_node in use_nodes
        .into_iter()
        .filter(|node| node.visibility == Visibility::Private)
    {
        // The Use node's `name` is the full import path as written. Three
        // shapes show up in real Rust code:
        //   - `foo::bar`           → single identifier `bar`
        //   - `foo::bar as baz`    → single identifier `baz`
        //   - `foo::{a, b as c}`   → grouped: identifiers `a`, `c`
        // Grouped imports must expand, otherwise an unused member inside a
        // partially-used group is missed and the literal `{a, b as c}` is
        // treated as one identifier that matches nothing.
        for identifier in identifiers_from_use_path(&use_node.name) {
            if !referenced_identifiers.contains(&identifier) {
                unused.push(json!({
                    "id": &use_node.id,
                    "name": &use_node.name,
                    "unused": identifier,
                    "file": file_path,
                    "line": use_node.start_line,
                }));
            }
        }
    }
    Ok(unused)
}

/// Blanks parser-confirmed use declarations while preserving line layout.
///
/// Tree-sitter columns are byte offsets, matching the UTF-8 source slices.
/// Exact ranges preserve real code after a same-line declaration.
#[hotpath::measure]
fn without_use_declarations(masked: String, source: &str, use_nodes: &[&Node]) -> Result<String> {
    let mut line_starts = vec![0usize];
    for (offset, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            line_starts.push(offset + 1);
        }
    }

    let mut bytes = masked.into_bytes();
    for node in use_nodes {
        let start = line_starts
            .get(node.start_line as usize)
            .and_then(|line| line.checked_add(node.start_column as usize));
        let end = line_starts
            .get(node.end_line as usize)
            .and_then(|line| line.checked_add(node.end_column as usize));
        let range = start
            .zip(end)
            .and_then(|(start, end)| bytes.get_mut(start..end))
            .ok_or_else(|| TraceDecayError::Config {
                message: format!(
                    "invalid parser range for unused import {} at {}:{}",
                    node.name, node.start_line, node.start_column
                ),
            })?;
        for byte in range {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(bytes).map_err(|error| TraceDecayError::Config {
        message: format!("unused-import source mask produced invalid UTF-8: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::unused_imports_in_file;

    #[test]
    fn source_scan_finds_grouped_aliases_without_graph_use_nodes() {
        let project = tempfile::tempdir().expect("temporary project");
        fs::create_dir_all(project.path().join("src")).expect("create source directory");
        fs::write(
            project.path().join("src/lib.rs"),
            r"use std::collections::{
    HashMap,
    HashSet as Set,
    BTreeMap,
};

// BTreeMap is only mentioned in this comment.
pub fn used() -> (HashMap<u32, u32>, Set<u32>) {
    (HashMap::new(), Set::new())
}
",
        )
        .expect("write source");

        let findings = unused_imports_in_file(project.path(), "src/lib.rs").expect("scan source");

        assert_eq!(findings.len(), 1, "findings: {findings:?}");
        assert_eq!(findings[0]["unused"], "BTreeMap");
        assert!(
            findings[0]["name"]
                .as_str()
                .is_some_and(|name| name.contains("HashSet as Set")),
            "grouped import text must be retained: {findings:?}"
        );
    }

    #[test]
    fn source_scan_ignores_public_reexports_and_reports_alias_line() {
        let project = tempfile::tempdir().expect("temporary project");
        fs::create_dir_all(project.path().join("src")).expect("create source directory");
        fs::write(
            project.path().join("src/lib.rs"),
            "pub use crate::api::PublicApi;\n\
             use crate::internal::PrivateApi as LocalApi;\n",
        )
        .expect("write source");

        let findings = unused_imports_in_file(project.path(), "src/lib.rs").expect("scan source");

        assert_eq!(findings.len(), 1, "findings: {findings:?}");
        assert_eq!(findings[0]["unused"], "LocalApi");
        assert_eq!(findings[0]["line"], 1);
        assert!(
            findings
                .iter()
                .all(|finding| finding["unused"] != "PublicApi"),
            "public re-exports are not private unused imports: {findings:?}"
        );
    }
}

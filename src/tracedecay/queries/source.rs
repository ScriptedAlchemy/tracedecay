use crate::errors::Result;
use crate::tracedecay::TraceDecay;
use crate::types::*;

impl TraceDecay {
    /// Returns the `#[derive(...)]` names attached to the given node.
    ///
    /// The graph's `DerivesMacro` edges are unreliable here: the resolver
    /// fuzzy-binds std-trait names like `Debug` to nonsense nodes (a `Debug`
    /// enum variant in an unrelated test fixture) and the resulting unique
    /// constraint on `(source, target, kind, line)` collapses multiple
    /// distinct derives on the same type onto a single edge — so a struct
    /// that derives `Debug, Clone, PartialEq, Eq, Hash` may surface only one
    /// of them. Instead we re-read the lines between `attrs_start_line` and
    /// `start_line` of the node, which the extractor already promises to
    /// cover the leading attribute block, and parse `#[derive(...)]`
    /// attributes directly. Bounded file I/O — one read per call.
    pub async fn get_derives_for_node(&self, node_id: &str) -> Result<Vec<String>> {
        let Some(node) = self.db.get_node_by_id(node_id).await? else {
            return Ok(Vec::new());
        };
        let file_path = self.project_root().join(&node.file_path);
        let Ok(content) = std::fs::read_to_string(&file_path) else {
            return Ok(Vec::new());
        };
        Ok(parse_derives_in_attr_block(
            &content,
            node.attrs_start_line,
            node.start_line,
        ))
    }

    /// Finds the most specific (smallest-span) node whose source range
    /// contains the given `(file, line)` location.
    ///
    /// Returns `None` when no indexed node covers the location — typically
    /// because the file isn't indexed, or the line is in a region the
    /// extractor didn't capture (e.g. inside a `use` block or top-of-file
    /// comment). Lines are 1-based to match `rustc` / `clippy` output;
    /// `Node.start_line` / `end_line` are 0-based internally so we subtract
    /// before comparing.
    ///
    /// Implementation loads every node in the file (cached at the index
    /// layer) and picks the smallest containing span. At the typical ~50
    /// nodes per file this is faster than a custom range-query and stays
    /// honest about overlap (impl blocks contain methods, etc.).
    pub async fn node_at_location(&self, file: &str, line_1based: u32) -> Result<Option<Node>> {
        if line_1based == 0 {
            return Ok(None);
        }
        let zero_based = line_1based - 1;
        let normalized = normalize_lookup_path(self.project_root(), file);
        let mut nodes = self.db.get_nodes_by_file(&normalized).await?;
        nodes.retain(|n| n.start_line <= zero_based && n.end_line >= zero_based);
        // Prefer the smallest containing span — that's the most specific
        // owner of the source location.
        nodes.sort_by_key(|n| (n.end_line - n.start_line, n.start_line));
        Ok(nodes.into_iter().next())
    }

    /// Returns the indexed size in bytes for a file path, or `0` if unknown.
    /// Used to estimate the token cost of expanding a file in responses.
    pub async fn get_file_size_bytes(&self, path: &str) -> u64 {
        match self.db.get_file(path).await {
            Ok(Some(rec)) => rec.size,
            _ => 0,
        }
    }
}

/// Parses every `#[derive(A, B, C)]` attribute appearing in `content`
/// between (0-based, inclusive) `start_line` and `end_line`. Multiple
/// derive attributes stack — `#[derive(Debug)]` and `#[derive(Clone)]` on
/// the same item both contribute. The returned list is de-duplicated and
/// preserves source order (Debug before Clone if that's how they're
/// written).
fn parse_derives_in_attr_block(content: &str, start_line: u32, end_line: u32) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let lines: Vec<&str> = content.lines().collect();
    let start = start_line as usize;
    let end = (end_line as usize).min(lines.len().saturating_sub(1));
    if start >= lines.len() {
        return out;
    }
    // Join the attribute block into a single string so multi-line
    // `#[derive(\n  Debug,\n  Clone,\n)]` (rustfmt's split form for long
    // derive lists) is handled uniformly with the single-line variant.
    let block = lines[start..=end].join("\n");
    let mut search_from = 0usize;
    while let Some(start_idx) = block[search_from..].find("#[derive(") {
        let abs_start = search_from + start_idx + "#[derive(".len();
        let Some(close_offset) = block[abs_start..].find(')') else {
            break;
        };
        let inner = &block[abs_start..abs_start + close_offset];
        for name in inner.split(',') {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            // Strip the path prefix on fully-qualified derives so callers
            // see `Serialize` not `serde::Serialize`. Matches the convention
            // the static derive table uses.
            let short = name.rsplit("::").next().unwrap_or(name).to_string();
            if seen.insert(short.clone()) {
                out.push(short);
            }
        }
        search_from = abs_start + close_offset + 1;
    }
    out
}

/// Normalises an external file path (typically from a `cargo check` /
/// `cargo clippy` diagnostic span) into the project-relative,
/// forward-slash form the index stores. Handles three real-world shapes:
///
/// - Absolute paths (cargo emits them when `--manifest-path` points at a
///   project root that differs from `cwd`): strip the `project_root`
///   prefix so `/abs/path/to/project/src/lib.rs` becomes `src/lib.rs`.
/// - Backslash paths (Windows cargo): convert `\` → `/`.
/// - Already-relative forward-slash paths: pass through unchanged.
///
/// Falls back to returning the input verbatim if no transformation
/// applies — `get_nodes_by_file` will then handle "no such file" the
/// same way it always does.
fn normalize_lookup_path(project_root: &std::path::Path, raw: &str) -> String {
    let forward = raw.replace('\\', "/");
    let path = std::path::Path::new(&forward);
    if path.is_absolute() {
        // Try canonicalising both sides; canonicalisation handles
        // symlinks, `..` segments, and trailing slashes uniformly. If
        // either fails (file doesn't exist on disk, project root
        // moved), fall back to a raw prefix strip.
        if let (Ok(abs), Ok(root)) = (path.canonicalize(), project_root.canonicalize())
            && let Ok(rel) = abs.strip_prefix(&root)
        {
            return rel.to_string_lossy().replace('\\', "/");
        }
        let root_str = project_root.to_string_lossy();
        if let Some(rel) = forward.strip_prefix(root_str.as_ref()) {
            return rel.trim_start_matches('/').to_string();
        }
    }
    forward
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod derive_parse_tests {
    use super::parse_derives_in_attr_block;

    #[test]
    fn parses_single_derive_block() {
        let src = "\
#[derive(Debug, Clone, PartialEq)]
pub struct Foo;
";
        let derives = parse_derives_in_attr_block(src, 0, 1);
        assert_eq!(derives, vec!["Debug", "Clone", "PartialEq"]);
    }

    #[test]
    fn stacks_multiple_derive_attributes() {
        let src = "\
#[derive(Debug)]
#[derive(Clone, Hash)]
pub enum K {}
";
        let derives = parse_derives_in_attr_block(src, 0, 2);
        assert_eq!(derives, vec!["Debug", "Clone", "Hash"]);
    }

    #[test]
    fn strips_path_prefix_on_qualified_derive() {
        let src = "#[derive(serde::Serialize, Debug)]\npub struct S;\n";
        let derives = parse_derives_in_attr_block(src, 0, 1);
        assert_eq!(derives, vec!["Serialize", "Debug"]);
    }

    #[test]
    fn ignores_non_derive_attributes() {
        let src = "\
#[cfg(feature = \"foo\")]
#[serde(rename = \"x\")]
#[derive(Debug)]
pub struct S;
";
        let derives = parse_derives_in_attr_block(src, 0, 3);
        assert_eq!(derives, vec!["Debug"]);
    }

    #[test]
    fn deduplicates_repeated_derives() {
        let src = "#[derive(Debug, Debug, Clone)]\npub struct S;\n";
        let derives = parse_derives_in_attr_block(src, 0, 1);
        assert_eq!(derives, vec!["Debug", "Clone"]);
    }

    /// Regression: rustfmt splits long derive lists across lines:
    ///   `#[derive(\n    Debug,\n    Clone,\n    PartialEq,\n)]`
    /// The previous line-bounded parser dropped all of these because it
    /// only matched `#[derive(...)]` when the closing `)` was on the
    /// same line. Production codebases with realistic-sized derive
    /// lists were getting empty `derives` output.
    #[test]
    fn parses_multiline_derive_attribute() {
        let src = "\
#[derive(
    Debug,
    Clone,
    PartialEq,
)]
pub struct Wide;
";
        let derives = parse_derives_in_attr_block(src, 0, 5);
        assert_eq!(derives, vec!["Debug", "Clone", "PartialEq"]);
    }

    #[test]
    fn parses_multiline_derive_mixed_with_single_line() {
        let src = "\
#[derive(Debug)]
#[derive(
    Clone,
    Hash,
)]
pub struct M;
";
        let derives = parse_derives_in_attr_block(src, 0, 5);
        assert_eq!(derives, vec!["Debug", "Clone", "Hash"]);
    }
}

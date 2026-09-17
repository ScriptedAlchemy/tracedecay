//! Helpers shared verbatim by multiple language extractors.
//!
//! Extractors whose traversal state is exactly the common shape use
//! [`ExtractionState`]; the rest keep a private state struct with their
//! language-specific fields, so the helpers below take the individual pieces
//! of state they need (source bytes, file path, unresolved-ref sink) instead
//! of a state struct. Bodies are moved here unchanged from the per-language
//! copies so extraction output stays byte-identical.

use std::time::{SystemTime, UNIX_EPOCH};

use tree_sitter::{Node as TsNode, Tree};

use crate::types::{
    Edge, EdgeKind, Node, NodeKind, UnresolvedRef, generate_node_id, generate_node_id_at,
};

/// Seconds since the Unix epoch, stamped on every emitted node as `updated_at`.
pub(crate) fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Traversal state for extractors that need nothing beyond the common shape.
pub(crate) struct ExtractionState<'s> {
    pub(crate) nodes: Vec<Node>,
    pub(crate) edges: Vec<Edge>,
    pub(crate) unresolved_refs: Vec<UnresolvedRef>,
    pub(crate) errors: Vec<String>,
    /// Stack of (name, `node_id`) for building qualified names and parent edges.
    pub(crate) node_stack: Vec<(String, String)>,
    pub(crate) file_path: String,
    pub(crate) source: &'s [u8],
    pub(crate) timestamp: u64,
}

impl<'s> ExtractionState<'s> {
    pub(crate) fn new(file_path: &str, source: &'s str) -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_refs: Vec::new(),
            errors: Vec::new(),
            node_stack: Vec::new(),
            file_path: file_path.to_string(),
            source: source.as_bytes(),
            timestamp: unix_timestamp_secs(),
        }
    }

    /// Returns the current qualified name prefix from the node stack.
    ///
    /// The file root is pushed onto `node_stack` as the first frame when
    /// extraction begins, so iterating the stack already yields the file
    /// path as the leading segment.
    pub(crate) fn qualified_prefix(&self) -> String {
        self.node_stack
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join("::")
    }

    /// Returns the current parent node ID, or None if at file root level.
    pub(crate) fn parent_node_id(&self) -> Option<&str> {
        self.node_stack.last().map(|(_, id)| id.as_str())
    }

    /// Gets the text of a tree-sitter node from the source.
    pub(crate) fn node_text(&self, node: TsNode<'_>) -> &'s str {
        node.utf8_text(self.source).unwrap_or("<invalid utf8>")
    }
}

/// Gets the text of a tree-sitter node from the source.
fn node_text(source: &[u8], node: TsNode<'_>) -> String {
    node.utf8_text(source)
        .unwrap_or("<invalid utf8>")
        .to_string()
}

/// `end_line` of the file root: the zero-based index of the file's last line
/// under the `str::lines` convention every extractor has always used, i.e.
/// `source.lines().count().saturating_sub(1)`. `lines` yields one line per
/// `\n` plus an unterminated tail, so the value is the newline count, minus one
/// when the source ends in `\n`.
///
/// The newline count is read off the parsed tree instead of rescanning the
/// source, which kept a changed-region walk of one tiny item paying for the
/// whole file. Tree-sitter advances a row only on `\n`, and the root node ends
/// at the EOF token, whose padding carries all trailing trivia, so
/// `root.end_position().row` is the file's newline count. Any bytes past the
/// root (none for a complete parse) are counted directly so the value is exact
/// even for a grammar whose root stops short. Composite adapters hand in their
/// mask, which preserves every `\n` byte and the length of the real source.
pub(crate) fn file_end_line(source: &str, tree: &Tree) -> u32 {
    let root = tree.root_node();
    let covered = root.end_byte().min(source.len());
    let newlines = root.end_position().row
        + source.as_bytes()[covered..]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count();
    newlines.saturating_sub(usize::from(source.ends_with('\n'))) as u32
}

/// [`file_end_line`] for a source Tree-sitter produced no tree for; the only
/// path left is the scan.
#[cfg(any(
    feature = "lang-lean",
    feature = "lang-markdown",
    feature = "lang-quint",
    feature = "lang-toml",
    test
))]
pub(crate) fn unparsed_file_end_line(source: &str) -> u32 {
    source.lines().count().saturating_sub(1) as u32
}

/// Mints the extraction-local ID for the construct rooted at `node`.
///
/// Every edge and unresolved reference an extractor emits while visiting the
/// construct is keyed by this ID, so two constructs must never share one. A
/// construct that begins its line (only blanks precede it) keeps the line-keyed
/// [`generate_node_id`], leaving indentation and one-construct lines
/// identity-neutral; a construct preceded by other source on its line also
/// carries its start column, which distinguishes `impl A { fn run() {} } impl
/// B { fn run() {} }` before relation endpoints are bound.
pub(crate) fn local_node_id(
    file_path: &str,
    source: &[u8],
    kind: &NodeKind,
    name: &str,
    node: TsNode<'_>,
) -> String {
    let start = node.start_position();
    let line = start.row as u32;
    let start_byte = node.start_byte().min(source.len());
    let line_start = source[..start_byte]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |newline| newline + 1);
    let begins_line = source[line_start..start_byte]
        .iter()
        .all(|byte| matches!(byte, b' ' | b'\t' | b'\r'));
    if begins_line {
        generate_node_id(file_path, kind, name, line)
    } else {
        generate_node_id_at(file_path, kind, name, line, start.column as u32)
    }
}

/// Strip comment markers from a single C-style comment text
/// (`//` line comments and `/* ... */` block comments).
pub(crate) fn clean_c_comment(comment: &str) -> String {
    clean_c_line_or_block_comment(comment, &["//"])
}

/// Strip comment markers from a single C-style comment text, including
/// `///` doc comments.
pub(crate) fn clean_c_doc_comment(comment: &str) -> String {
    // Longer prefixes first so `///` is not stripped as `//`.
    clean_c_line_or_block_comment(comment, &["///", "//"])
}

fn clean_c_line_or_block_comment(comment: &str, line_prefixes: &[&str]) -> String {
    let trimmed = comment.trim();
    for prefix in line_prefixes {
        if let Some(stripped) = trimmed.strip_prefix(prefix) {
            return stripped.strip_prefix(' ').unwrap_or(stripped).to_string();
        }
    }
    if trimmed.starts_with("/*") && trimmed.ends_with("*/") {
        let inner = &trimmed[2..trimmed.len() - 2];
        inner
            .lines()
            .map(|line| {
                let l = line.trim();
                l.strip_prefix("* ")
                    .or_else(|| l.strip_prefix('*'))
                    .unwrap_or(l)
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    } else {
        trimmed.to_string()
    }
}

/// Extract a docstring from the run of `comment` siblings immediately
/// preceding `node`, cleaning each comment with `clean`.
pub(crate) fn docstring_from_preceding_comments(
    source: &[u8],
    node: TsNode<'_>,
    clean: fn(&str) -> String,
) -> Option<String> {
    let mut comments = Vec::new();
    let mut current = node.prev_named_sibling();
    while let Some(sibling) = current {
        if sibling.kind() == "comment" {
            comments.push(node_text(source, sibling));
            current = sibling.prev_named_sibling();
        } else {
            break;
        }
    }
    if comments.is_empty() {
        return None;
    }
    // Comments are collected in reverse order (closest first).
    comments.reverse();
    let cleaned: Vec<String> = comments.iter().map(|c| clean(c)).collect();
    let result = cleaned.join("\n").trim().to_string();
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Extract a docstring from the run of `#` comment siblings immediately
/// preceding `node`.
#[cfg(any(feature = "lang-bash", feature = "lang-ruby"))]
pub(crate) fn docstring_from_hash_comments(source: &[u8], node: TsNode<'_>) -> Option<String> {
    let mut comments: Vec<String> = Vec::new();
    let mut prev = node.prev_named_sibling();
    while let Some(prev_node) = prev {
        if prev_node.kind() == "comment" {
            let text = node_text(source, prev_node);
            let stripped = text.trim_start_matches('#').trim().to_string();
            comments.push(stripped);
            prev = prev_node.prev_named_sibling();
        } else {
            break;
        }
    }
    if comments.is_empty() {
        return None;
    }
    // Comments were collected in reverse order; reverse them back.
    comments.reverse();
    Some(comments.join("\n"))
}

/// Recursively find `call_expression` nodes and create unresolved Calls
/// references, taking the callee name from the first named child.
pub(crate) fn extract_call_expression_sites(
    source: &[u8],
    file_path: &str,
    unresolved_refs: &mut Vec<UnresolvedRef>,
    node: TsNode<'_>,
    fn_node_id: &str,
) {
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.kind() == "call_expression"
                && let Some(callee) = child.named_child(0)
            {
                let callee_name = node_text(source, callee);
                unresolved_refs.push(UnresolvedRef {
                    from_node_id: fn_node_id.to_string(),
                    reference_name: callee_name,
                    reference_kind: EdgeKind::Calls,
                    line: child.start_position().row as u32,
                    column: child.start_position().column as u32,
                    file_path: file_path.to_string(),
                });
            }
            extract_call_expression_sites(source, file_path, unresolved_refs, child, fn_node_id);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tree_sitter::{Parser, Tree};

    use super::{file_end_line, unparsed_file_end_line};
    use crate::{LanguageRegistry, ts_provider};

    /// Sources on which a raw newline count and `str::lines` disagree: empty,
    /// terminated, unterminated, blank-only, repeated trailing blank lines,
    /// bare `\r`, CRLF, and multi-byte text.
    const LINE_ENDING_SHAPES: &[&str] = &[
        "",
        "\n",
        "\n\n",
        "\r",
        "\r\n",
        " \n\t\n",
        "x",
        "x\n",
        "x\n\n",
        "x\n\n\n",
        "x\ny",
        "x\ny\n",
        "x\r\ny",
        "x\r\ny\r\n",
        "x\r\ny\r\n\r\n",
        "x\ry\n",
        "é\n界",
        "é\n界\n",
        "é\r\n界\r\n",
        "x // é界\ny\n",
    ];

    /// Grammars with distinct end-of-input behaviour: plain lexers (`rust`,
    /// `cpp`), an indentation scanner that synthesizes tokens at EOF
    /// (`python`), the grammar composite adapters parse their masks with
    /// (`typescript`), the Markdown block grammar, and `cobol`, whose external
    /// scanner skips the sequence-number area and used to spin at EOF on every
    /// shape here shorter than six columns (#1104). Grammars a feature set
    /// does not link are skipped; the caller decides whether that is vacuous.
    /// Degenerate inputs are not fed to every bundled grammar because an
    /// external scanner that never terminates on them cannot be interrupted by
    /// any parse deadline.
    fn linked_grammar_keys() -> Vec<&'static str> {
        ["rust", "cpp", "python", "typescript", "markdown", "cobol"]
            .into_iter()
            .filter(|key| ts_provider::try_language(key).is_ok())
            .collect()
    }

    fn parse(grammar_key: &str, source: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&ts_provider::try_language(grammar_key).expect("registered grammar"))
            .expect("configure parser");
        parser.parse(source, None).expect("parse")
    }

    #[test]
    fn file_end_line_matches_the_lines_convention_for_every_line_ending_shape() {
        let keys = linked_grammar_keys();
        assert!(
            !keys.is_empty() || ts_provider::try_language("rust").is_err(),
            "a build with a grammar bundle must exercise at least one grammar"
        );
        for key in keys {
            for source in LINE_ENDING_SHAPES {
                let tree = parse(key, source);
                assert_eq!(
                    file_end_line(source, &tree),
                    unparsed_file_end_line(source),
                    "{key}: {source:?}"
                );
            }
        }
    }

    /// The tree-derived value must equal the source scan for every checked-in
    /// extraction fixture, through the grammar its extractor really selects,
    /// and the root must end at EOF so no trailing bytes are ever rescanned.
    /// Composite adapters are checked against both the real source and the
    /// mask their retained tree was parsed from.
    #[test]
    fn file_end_line_matches_the_lines_convention_for_every_checked_in_fixture() {
        let registry = LanguageRegistry::new();
        let mut checked = Vec::new();
        for directory in ["../../tests/fixtures", "fixtures"] {
            let mut entries = std::fs::read_dir(Path::new(directory))
                .unwrap_or_else(|error| panic!("{directory}: {error}"))
                .map(|entry| entry.expect("fixture entry").path())
                .filter(|path| path.is_file())
                .collect::<Vec<_>>();
            entries.sort();
            for path in entries {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("fixture file name");
                let Some(extractor) = registry.extractor_for_file(name) else {
                    continue;
                };
                let key = extractor.retained_grammar_key(name);
                if ts_provider::try_language(&key).is_err() {
                    continue;
                }
                let source = std::fs::read_to_string(&path).expect("read fixture");
                let mask = extractor.prepare_parse_source(&source);
                let tree = parse(&key, &mask);
                let expected = unparsed_file_end_line(&source);
                assert_eq!(
                    tree.root_node().end_byte(),
                    source.len(),
                    "{name}: the {key} root must end at EOF"
                );
                assert_eq!(file_end_line(&source, &tree), expected, "{name}");
                assert_eq!(file_end_line(&mask, &tree), expected, "{name} (mask)");
                checked.push(name.to_owned());
            }
        }
        assert!(
            !checked.is_empty() || ts_provider::try_language("rust").is_err(),
            "a build with a grammar bundle must exercise the fixtures"
        );
    }
}

//! Tree-sitter based Markdown source code extractor.
//!
//! Uses `tree-sitter-grammars/tree-sitter-markdown` (a split grammar):
//!
//!   * `block::LANGUAGE` parses block-level structure (sections, headings,
//!     paragraphs, lists). Inline content is left as opaque `(inline)` nodes.
//!   * `inline::LANGUAGE` is run over each `(inline)` node's byte range to
//!     produce links, emphasis, etc. We use `Parser::set_included_ranges`
//!     so the inline tree's byte/row positions stay in the original source.
//!
//! `atx_heading` / `setext_heading` nodes become `Module` nodes; `inline_link`
//! and reference-style links whose destination is a project-local source file
//! emit `Uses` edges. Frontmatter (`(minus_metadata)`, `(plus_metadata)`) is
//! skipped — the grammar makes it opaque, so we don't recurse into it.
use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tree_sitter::{Node as TsNode, Parser, Range, Tree};

use crate::types::{
    Edge, EdgeKind, ExtractionResult, Node, NodeKind, Visibility, generate_node_id,
};

/// Separator between path elements in a heading's qualified name. Markdown
/// section paths read as prose, so they use `" > "` rather than the `::` used
/// by namespaced programming languages.
pub const HEADING_PATH_SEPARATOR: &str = " > ";

pub struct MarkdownExtractor;

/// Byte bounds `(start, end)` of every line, `end` excluding the line
/// terminator. Mirrors `str::lines()` for non-empty input.
fn line_bounds(source: &[u8]) -> Vec<(usize, usize)> {
    let mut bounds = Vec::new();
    let mut start = 0usize;
    for (index, byte) in source.iter().enumerate() {
        if *byte == b'\n' {
            let mut end = index;
            if end > start && source[end - 1] == b'\r' {
                end -= 1;
            }
            bounds.push((start, end));
            start = index + 1;
        }
    }
    if start < source.len() {
        bounds.push((start, source.len()));
    }
    bounds
}

struct ExtractionState {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    file_path: String,
    source: Vec<u8>,
    timestamp: u64,
    /// (heading title, node id, level) — heading levels strictly increase
    /// going *down* the stack. Headings of equal or shallower level pop
    /// the stack so we always parent to the nearest ancestor heading.
    node_stack: Vec<(String, String, usize)>,
    /// One inline parser, lazily initialised, reused for every `(inline)`
    /// node we encounter to avoid re-creating it per heading/paragraph.
    inline_parser: Option<Parser>,
    /// `(index into nodes, heading level, heading start line)` in document
    /// order. Section end lines are resolved in one post-pass once every
    /// heading is known — a heading owns everything up to the next heading of
    /// the same or shallower level.
    headings: Vec<(usize, usize, u32)>,
    /// The supplied block tree cannot represent the inline grammar. Retained
    /// extraction disables this path and reports a typed reset.
    extract_inline_links: bool,
    /// CommonMark reference definitions, keyed by normalized label.
    /// Collected during the block walk so a definition that appears after
    /// its uses still resolves.
    reference_defs: HashMap<String, String>,
    /// Reference-style links waiting for [`Self::reference_defs`] to fill in.
    pending_refs: Vec<PendingReferenceLink>,
}

/// A `[text][label]` / `[text][]` / `[text]` link whose destination is
/// known only after every `link_reference_definition` has been seen.
struct PendingReferenceLink {
    parent_id: String,
    label: String,
    line: u32,
}

impl ExtractionState {
    fn new(file_path: &str, source: &str, extract_inline_links: bool) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            file_path: file_path.to_string(),
            source: source.as_bytes().to_vec(),
            timestamp,
            node_stack: Vec::new(),
            inline_parser: None,
            headings: Vec::new(),
            extract_inline_links,
            reference_defs: HashMap::new(),
            pending_refs: Vec::new(),
        }
    }

    fn node_text(&self, node: TsNode<'_>) -> String {
        node.utf8_text(&self.source)
            .unwrap_or("<invalid utf8>")
            .to_string()
    }

    /// Parse the byte range covered by `inline_node` with the inline grammar
    /// and return the resulting tree, or `None` if parsing fails. The tree's
    /// byte/row positions are anchored in the original source via
    /// `set_included_ranges`.
    fn parse_inline(&mut self, inline_node: TsNode<'_>) -> Option<Tree> {
        if !self.extract_inline_links {
            return None;
        }
        let parser = self.inline_parser.get_or_insert_with(|| {
            let mut p = Parser::new();
            let _ =
                p.set_language(&tracedecay_large_treesitters::markdown::inline::LANGUAGE.into());
            p
        });
        let range = Range {
            start_byte: inline_node.start_byte(),
            end_byte: inline_node.end_byte(),
            start_point: inline_node.start_position(),
            end_point: inline_node.end_position(),
        };
        if parser.set_included_ranges(&[range]).is_err() {
            return None;
        }
        parser.parse(&self.source, None)
    }
}

impl MarkdownExtractor {
    pub fn extract_markdown(file_path: &str, source: &str) -> ExtractionResult {
        let start = Instant::now();
        let mut state = ExtractionState::new(file_path, source, true);
        Self::add_file_node(&mut state, file_path, source);

        if let Ok(tree) = Self::parse(source) {
            Self::visit(&mut state, tree.root_node());
        }

        Self::build_result(state, start)
    }

    fn extract_supplied_tree(
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtraction {
        let start = Instant::now();
        let mut state = ExtractionState::new(file_path, source, false);
        Self::add_file_node(&mut state, file_path, source);

        let metrics = crate::parsed_extraction::visit_root_children(tree, scope, |child| {
            Self::visit(&mut state, child);
        });

        // Links require a second inline-language parse, which the supplied
        // block tree cannot represent. This output is therefore not a valid
        // incremental delta and must force a cold extraction upstream.
        crate::parsed_extraction::ParsedExtraction {
            result: Self::build_result(state, start),
            disposition: crate::parsed_extraction::ParsedExtractionDisposition::Reset {
                reason: crate::parsed_extraction::ParsedExtractionResetReason::CompositeGrammar,
            },
            metrics,
        }
    }

    fn add_file_node(state: &mut ExtractionState, file_path: &str, source: &str) {
        let file_node = Node {
            id: generate_node_id(file_path, &NodeKind::File, file_path, 0),
            kind: NodeKind::File,
            name: file_path.to_string(),
            qualified_name: file_path.to_string(),
            file_path: file_path.to_string(),
            start_line: 0,
            attrs_start_line: 0,
            end_line: source.lines().count().saturating_sub(1) as u32,
            start_column: 0,
            end_column: 0,
            signature: None,
            docstring: None,
            visibility: Visibility::Pub,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            updated_at: state.timestamp,
            parent_id: None,
        };
        let file_node_id = file_node.id.clone();
        state.nodes.push(file_node);
        state
            .node_stack
            .push((file_path.to_string(), file_node_id, 0));
    }

    /// Resolve every heading's end line to the end of its *section*, not the
    /// end of the heading line.
    ///
    /// A heading owns every line up to (but excluding) the next heading whose
    /// level is the same or shallower; the last heading owns the rest of the
    /// file. Trailing blank lines are trimmed so adjacent sections do not
    /// overlap on whitespace. This runs over the collected heading list rather
    /// than the parse tree, so fenced code blocks containing `#` lines can
    /// never widen or split a section: they were never admitted as headings.
    fn finalize_heading_spans(state: &mut ExtractionState) {
        if state.headings.is_empty() {
            return;
        }
        let bounds = line_bounds(&state.source);
        if bounds.is_empty() {
            return;
        }
        let last_line = (bounds.len() - 1) as u32;
        let headings = state.headings.clone();
        for (order, (index, level, start_line)) in headings.iter().enumerate() {
            let next_start = headings[order + 1..]
                .iter()
                .find(|(_, other_level, _)| other_level <= level)
                .map(|(_, _, other_start)| *other_start);
            let mut end_line = match next_start {
                Some(next) => next.saturating_sub(1),
                None => last_line,
            }
            .min(last_line)
            .max(*start_line);
            while end_line > *start_line {
                let (line_start, line_end) = bounds[end_line as usize];
                if state.source[line_start..line_end]
                    .iter()
                    .all(u8::is_ascii_whitespace)
                {
                    end_line -= 1;
                } else {
                    break;
                }
            }
            let (line_start, line_end) = bounds[end_line as usize];
            let node = &mut state.nodes[*index];
            node.end_line = end_line;
            node.end_column = (line_end - line_start) as u32;
        }
    }

    fn build_result(mut state: ExtractionState, start: Instant) -> ExtractionResult {
        Self::resolve_reference_links(&mut state);
        Self::finalize_heading_spans(&mut state);
        ExtractionResult {
            nodes: state.nodes,
            edges: state.edges,
            unresolved_refs: Vec::new(),
            errors: Vec::new(),
            duration_ms: start.elapsed().as_millis() as u64,
        }
    }

    fn parse(source: &str) -> Result<Tree, String> {
        let mut parser = Parser::new();
        parser
            .set_language(&tracedecay_large_treesitters::markdown::LANGUAGE.into())
            .map_err(|e| format!("failed to load markdown grammar: {e}"))?;
        parser
            .parse(source, None)
            .ok_or_else(|| "tree-sitter parse returned None".to_string())
    }

    /// Walks the block tree. `atx_heading` / `setext_heading` produce `Module`
    /// nodes; `(inline)` nodes are re-parsed with the inline grammar to find
    /// links. Frontmatter (`(minus_metadata)`, `(plus_metadata)`) is opaque
    /// per the grammar — we never descend into it.
    fn visit(state: &mut ExtractionState, node: TsNode<'_>) {
        match node.kind() {
            "atx_heading" | "setext_heading" => Self::visit_heading(state, node),
            "minus_metadata" | "plus_metadata" => {
                // Opaque YAML/TOML frontmatter — don't descend.
            }
            "link_reference_definition" => Self::visit_reference_definition(state, node),
            "inline" => Self::visit_inline(state, node),
            _ => Self::visit_children(state, node),
        }
    }

    fn visit_children(state: &mut ExtractionState, node: TsNode<'_>) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                Self::visit(state, cursor.node());
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    fn visit_heading(state: &mut ExtractionState, node: TsNode<'_>) {
        let level = Self::heading_level(node);

        // Block grammar exposes the heading text as the `heading_content`
        // field, which points to an `(inline)` node containing the text.
        let title = node
            .child_by_field_name("heading_content")
            .map(|n| state.node_text(n).trim().to_string())
            .unwrap_or_default();

        if title.is_empty() {
            // Still recurse into the heading body so any `(inline)` children
            // (which would be unusual but not impossible) get their links.
            Self::visit_children(state, node);
            return;
        }

        while state.node_stack.len() > 1 {
            let last_level = state.node_stack[state.node_stack.len() - 1].2;
            if last_level >= level {
                state.node_stack.pop();
            } else {
                break;
            }
        }

        let kind = NodeKind::Module;
        // Heading-path qualified name: the file path, then every enclosing
        // heading, then this heading — "docs/plans/x.md > H1 > H2". The stack
        // was popped to this heading's parent above, so it is exactly the
        // ancestor path. Duplicate titles under different parents therefore
        // stay distinguishable, and duplicate titles under the *same* parent
        // still get distinct node ids (the id hashes the start line).
        let qualified_name = state
            .node_stack
            .iter()
            .map(|(name, _, _)| name.as_str())
            .chain(std::iter::once(title.as_str()))
            .collect::<Vec<_>>()
            .join(HEADING_PATH_SEPARATOR);
        let id = generate_node_id(
            &state.file_path,
            &kind,
            &title,
            node.start_position().row as u32,
        );

        let node_obj = Node {
            id: id.clone(),
            kind,
            name: title.clone(),
            qualified_name,
            file_path: state.file_path.clone(),
            start_line: node.start_position().row as u32,
            attrs_start_line: node.start_position().row as u32,
            end_line: node.end_position().row as u32,
            start_column: node.start_position().column as u32,
            end_column: node.end_position().column as u32,
            signature: Some(format!("{} {}", "#".repeat(level), title)),
            docstring: None,
            visibility: Visibility::Pub,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            updated_at: state.timestamp,
            parent_id: None,
        };

        if let Some((_, parent_id, _)) = state.node_stack.last() {
            state.edges.push(Edge {
                source: parent_id.clone(),
                target: id.clone(),
                kind: EdgeKind::Contains,
                line: Some(node.start_position().row as u32),
            });
        }

        let heading_index = state.nodes.len();
        let heading_start_line = node.start_position().row as u32;
        state.nodes.push(node_obj);
        state
            .headings
            .push((heading_index, level, heading_start_line));
        state.node_stack.push((title, id, level));

        // Recurse so links inside the heading text (e.g.
        // `## See [main](src/main.rs)`) become `Uses` edges parented to
        // this heading.
        Self::visit_children(state, node);
    }

    /// Returns the ATX heading level (1-6) for `atx_heading` / `setext_heading`.
    /// ATX uses `atx_h{1..6}_marker` children; setext uses level-1 (`===`) or
    /// level-2 (`---`) underlines, identified by the marker child kind.
    fn heading_level(node: TsNode<'_>) -> usize {
        for child in node.children(&mut node.walk()) {
            let k = child.kind();
            if let Some(rest) = k.strip_prefix("atx_h")
                && let Some(d) = rest.strip_suffix("_marker")
                && let Ok(n) = d.parse::<usize>()
            {
                return n.clamp(1, 6);
            }
            if k == "setext_h1_underline" {
                return 1;
            }
            if k == "setext_h2_underline" {
                return 2;
            }
        }
        1
    }

    /// Re-parse the `(inline)` node's byte range with the inline grammar and
    /// collect any `inline_link` nodes as `Uses` edges.
    fn visit_inline(state: &mut ExtractionState, inline_node: TsNode<'_>) {
        let Some(inline_tree) = state.parse_inline(inline_node) else {
            return;
        };
        let root = inline_tree.root_node();
        Self::collect_links(state, root);
    }

    fn collect_links(state: &mut ExtractionState, node: TsNode<'_>) {
        match node.kind() {
            "inline_link" => Self::visit_link(state, node),
            "image" => {
                if child_of_kind(node, "link_destination").is_some() {
                    Self::visit_link(state, node);
                } else {
                    Self::queue_reference_link(state, node);
                }
            }
            "full_reference_link" | "collapsed_reference_link" | "shortcut_link" => {
                Self::queue_reference_link(state, node);
            }
            _ => {}
        }
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                Self::collect_links(state, cursor.node());
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    fn visit_reference_definition(state: &mut ExtractionState, node: TsNode<'_>) {
        let Some(label_node) = child_of_kind(node, "link_label") else {
            return;
        };
        let Some(dest_node) = child_of_kind(node, "link_destination") else {
            return;
        };
        let label = normalize_link_label(&strip_label_brackets(&state.node_text(label_node)));
        if label.is_empty() {
            return;
        }
        state
            .reference_defs
            .insert(label, clean_link_destination(&state.node_text(dest_node)));
    }

    fn queue_reference_link(state: &mut ExtractionState, node: TsNode<'_>) {
        let Some(parent_id) = state
            .node_stack
            .last()
            .map(|(_, parent_id, _)| parent_id.clone())
        else {
            return;
        };
        let label = match node.kind() {
            "full_reference_link" | "image" => child_of_kind(node, "link_label")
                .map(|n| strip_label_brackets(&state.node_text(n)))
                .or_else(|| child_of_kind(node, "link_text").map(|n| state.node_text(n))),
            _ => child_of_kind(node, "link_text").map(|n| state.node_text(n)),
        };
        let Some(label) = label.map(|raw| normalize_link_label(&raw)) else {
            return;
        };
        if label.is_empty() {
            return;
        }
        state.pending_refs.push(PendingReferenceLink {
            parent_id,
            label,
            line: node.start_position().row as u32,
        });
    }

    fn resolve_reference_links(state: &mut ExtractionState) {
        let pending = std::mem::take(&mut state.pending_refs);
        for link in pending {
            let Some(url) = state.reference_defs.get(&link.label).cloned() else {
                continue;
            };
            Self::emit_code_uses(state, &url, &link.parent_id, link.line);
        }
    }

    fn visit_link(state: &mut ExtractionState, node: TsNode<'_>) {
        let Some(url_node) = child_of_kind(node, "link_destination") else {
            return;
        };
        let Some(parent_id) = state
            .node_stack
            .last()
            .map(|(_, parent_id, _)| parent_id.clone())
        else {
            return;
        };
        let url = state.node_text(url_node);
        Self::emit_code_uses(state, &url, &parent_id, node.start_position().row as u32);
    }

    fn emit_code_uses(state: &mut ExtractionState, url: &str, parent_id: &str, line: u32) {
        let url = clean_link_destination(url);
        if url.starts_with("http://") || url.starts_with("https://") {
            return;
        }

        let target_path = url.trim_start_matches("file:");
        let target_ext = target_path.rsplit('.').next().unwrap_or("");
        if !is_code_extension(target_ext) {
            return;
        }

        let target_id = generate_node_id(target_path, &NodeKind::File, target_path, 0);
        state.edges.push(Edge {
            source: parent_id.to_string(),
            target: target_id,
            kind: EdgeKind::Uses,
            line: Some(line),
        });
    }
}

fn child_of_kind<'tree>(node: TsNode<'tree>, kind: &str) -> Option<TsNode<'tree>> {
    node.children(&mut node.walk())
        .find(|child| child.kind() == kind)
}

/// CommonMark label matching: trim, collapse whitespace, case-fold.
fn normalize_link_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn strip_label_brackets(raw: &str) -> String {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(trimmed)
        .to_string()
}

fn clean_link_destination(raw: &str) -> String {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .unwrap_or(trimmed)
        .to_string()
}

fn is_code_extension(ext: &str) -> bool {
    // Only include actual programming-language source files.
    // Config (yaml, toml, json), markup (html, css, markdown), and
    // notebook (ipynb) files are excluded to avoid low-signal edges.
    matches!(
        ext,
        "rs" | "py"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "go"
            | "java"
            | "c"
            | "cpp"
            | "h"
            | "hpp"
            | "cs"
            | "rb"
            | "php"
            | "swift"
            | "kt"
            | "scala"
            | "R"
            | "sh"
            | "bash"
            | "zsh"
            | "fish"
            | "ps1"
            | "ex"
            | "exs"
            | "erl"
            | "hrl"
            | "fs"
            | "fsx"
            | "ml"
            | "mli"
            | "hs"
            | "lhs"
            | "lua"
            | "pl"
            | "pm"
            | "t"
            | "nix"
            | "sql"
            | "proto"
            | "v"
            | "vhd"
            | "vhdl"
            | "sage"
            | "sagews"
    )
}

impl crate::LanguageExtractor for MarkdownExtractor {
    fn extensions(&self) -> &[&str] {
        &["md", "markdown"]
    }

    fn language_name(&self) -> &'static str {
        "Markdown"
    }

    fn extract(&self, file_path: &str, source: &str) -> ExtractionResult {
        Self::extract_markdown(file_path, source)
    }

    fn extract_parsed(
        &self,
        file_path: &str,
        source: &str,
        tree: &Tree,
        scope: crate::parsed_extraction::ParsedExtractionScope<'_>,
    ) -> crate::parsed_extraction::ParsedExtraction {
        Self::extract_supplied_tree(file_path, source, tree, scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(source: &str) -> ExtractionResult {
        MarkdownExtractor::extract_markdown("docs/plans/x.md", source)
    }

    fn modules(result: &ExtractionResult) -> Vec<&Node> {
        result
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Module)
            .collect()
    }

    #[test]
    fn atx_and_setext_headings_become_modules() {
        let source = "\
Setext One
==========

## ATX Two

Setext Two
----------
";
        let result = extract(source);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let names: Vec<&str> = modules(&result)
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(names, vec!["Setext One", "ATX Two", "Setext Two"]);
        assert_eq!(
            modules(&result)[1].qualified_name,
            format!(
                "docs/plans/x.md{HEADING_PATH_SEPARATOR}Setext One{HEADING_PATH_SEPARATOR}ATX Two"
            )
        );
    }

    #[test]
    fn heading_path_qualified_names_include_file_and_ancestors() {
        let source = "# H1\n\n## H2\n\n### H3\n";
        let result = extract(source);
        let qualified: Vec<&str> = modules(&result)
            .iter()
            .map(|node| node.qualified_name.as_str())
            .collect();
        assert_eq!(
            qualified,
            vec![
                "docs/plans/x.md > H1",
                "docs/plans/x.md > H1 > H2",
                "docs/plans/x.md > H1 > H2 > H3",
            ]
        );
    }

    #[test]
    fn section_span_covers_body_until_next_same_or_higher_heading() {
        let source = "\
# Alpha

alpha body

## Nested

nested body

# Beta

beta body
";
        let result = extract(source);
        let headings = modules(&result);
        let alpha = headings
            .iter()
            .find(|node| node.name == "Alpha")
            .expect("Alpha");
        let nested = headings
            .iter()
            .find(|node| node.name == "Nested")
            .expect("Nested");
        let beta = headings
            .iter()
            .find(|node| node.name == "Beta")
            .expect("Beta");
        assert_eq!(alpha.start_line, 0);
        assert_eq!(alpha.end_line, nested.end_line);
        assert!(alpha.end_line < beta.start_line);
        assert_eq!(nested.start_line, 4);
        assert!(nested.end_line < beta.start_line);
        assert_eq!(beta.start_line, 8);
        assert_eq!(beta.end_line, 10);
    }

    #[test]
    fn yaml_frontmatter_is_skipped_without_error() {
        let source = "\
---
title: skipped
---

# Real heading
";
        let result = extract(source);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let names: Vec<&str> = modules(&result)
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(names, vec!["Real heading"]);
    }

    #[test]
    fn fenced_code_hash_lines_are_not_headings() {
        let source = "\
# Real

```markdown
# Fake heading
```

## Still real
";
        let result = extract(source);
        let headings = modules(&result);
        let names: Vec<&str> = headings.iter().map(|node| node.name.as_str()).collect();
        assert_eq!(names, vec!["Real", "Still real"]);
        let real = headings
            .iter()
            .find(|node| node.name == "Real")
            .expect("Real");
        assert!(real.end_line >= 4, "fenced body stays inside the section");
    }

    #[test]
    fn duplicate_heading_titles_keep_unique_ids() {
        let source = "# Repeat\n\n## Child\n\n# Repeat\n";
        let result = extract(source);
        let repeats: Vec<&Node> = modules(&result)
            .into_iter()
            .filter(|node| node.name == "Repeat")
            .collect();
        assert_eq!(repeats.len(), 2);
        assert_ne!(repeats[0].id, repeats[1].id);
        assert_eq!(repeats[0].qualified_name, repeats[1].qualified_name);
    }

    #[test]
    fn reference_style_links_emit_uses_edges() {
        let source = "\
# Docs

See [entry][main] and [src/lib.rs][] plus [src/main.rs].

[main]: src/main.rs
[src/lib.rs]: src/lib.rs
[src/main.rs]: src/main.rs
";
        let result = extract(source);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let uses: Vec<&String> = result
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Uses)
            .map(|edge| &edge.target)
            .collect();
        let main_id = generate_node_id("src/main.rs", &NodeKind::File, "src/main.rs", 0);
        let lib_id = generate_node_id("src/lib.rs", &NodeKind::File, "src/lib.rs", 0);
        assert_eq!(
            uses.len(),
            3,
            "full, collapsed, and shortcut refs: {uses:?}"
        );
        assert_eq!(uses.iter().filter(|target| **target == &main_id).count(), 2);
        assert_eq!(uses.iter().filter(|target| **target == &lib_id).count(), 1);
    }
}

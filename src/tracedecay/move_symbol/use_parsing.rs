//! `use`-statement and identifier parsing for `move_symbol`'s dependency
//! analysis: which names a moved body references, and what each `use`
//! statement in the source file brings into scope. A tree-sitter walk
//! captures grouped/multi-line imports whole and never matches `use` tokens
//! inside comments or strings; a line-scan fallback covers the rare case the
//! Rust grammar cannot be loaded.

use std::collections::HashSet;

use tree_sitter::{Node as TsNode, Parser};

/// A single binding brought into scope by a `use` statement.
pub(super) struct UseLeaf {
    /// The name as referenced in code (the alias when `as` is present).
    pub(super) binding: String,
}

/// A parsed `use` statement from a source file.
pub(super) struct UseStatement {
    /// The full statement text (single physical line).
    pub(super) text: String,
    /// 1-based line of the statement.
    pub(super) line: u32,
    /// Whether the statement is a glob import (`use a::*;`).
    pub(super) glob: bool,
    pub(super) leaves: Vec<UseLeaf>,
}

/// Collects identifier tokens from source text (Rust identifier rules). Used to
/// test whether the moved body references a given symbol name.
pub(super) fn body_identifiers(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch == '_' || ch.is_ascii_alphanumeric() {
            cur.push(ch);
        } else if is_identifier(&cur) {
            out.insert(std::mem::take(&mut cur));
        } else {
            cur.clear();
        }
    }
    if is_identifier(&cur) {
        out.insert(cur);
    }
    out
}

/// True when `s` is a non-empty Rust-style identifier (does not start with a
/// digit).
fn is_identifier(s: &str) -> bool {
    matches!(s.chars().next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
}

/// Parses `use` statements from Rust source into their brought-in bindings.
/// Handles `use a::B;`, `use a::B as C;`, grouped `use a::{B, C};`, and
/// multi-line grouped imports. Uses a tree-sitter walk so grouped imports that
/// span several physical lines are captured whole and `use` tokens inside
/// comments or strings are never matched; falls back to a line scan only when
/// the Rust grammar is unavailable.
pub(super) fn parse_use_statements(source: &str) -> Vec<UseStatement> {
    match parse_use_statements_ts(source) {
        Some(stmts) => stmts,
        None => parse_use_statements_linescan(source),
    }
}

/// Return a destination-stable dependency import.
///
/// `self::` and `super::` are relative to the source module and would silently
/// change meaning after a move, so they require an explicit hint instead of
/// auto-insertion. A source `pub use` is reduced to a private `use`: the moved
/// body needs the binding, not a new public re-export from the destination.
pub(super) fn portable_dependency_import(statement: &str) -> Option<String> {
    let trimmed = statement.trim();
    let (path, was_public) = trimmed
        .strip_prefix("pub use ")
        .map(|path| (path, true))
        .or_else(|| trimmed.strip_prefix("use ").map(|path| (path, false)))?;
    let path = path.trim_start();
    if path == "self;"
        || path.starts_with("self::")
        || path == "super;"
        || path.starts_with("super::")
    {
        return None;
    }
    Some(if was_public {
        format!("use {path}")
    } else {
        trimmed.to_string()
    })
}

/// Tree-sitter path: collect every `use_declaration` node (top-level and inside
/// `mod` blocks) and parse its full byte-range text. Returns `None` when the
/// grammar cannot be loaded so the caller can fall back to the line scan.
fn parse_use_statements_ts(source: &str) -> Option<Vec<UseStatement>> {
    let mut parser = Parser::new();
    let language = tracedecay_code_extraction::ts_provider::try_language("rust").ok()?;
    parser.set_language(&language).ok()?;
    let tree = parser.parse(source, None)?;
    let mut out = Vec::new();
    collect_use_declarations(tree.root_node(), source.as_bytes(), &mut out);
    Some(out)
}

/// Recursive pre-order walk collecting `use_declaration` nodes into parsed
/// [`UseStatement`]s.
fn collect_use_declarations(node: TsNode<'_>, src: &[u8], out: &mut Vec<UseStatement>) {
    if node.kind() == "use_declaration" {
        if let Ok(text) = node.utf8_text(src)
            && let Some((glob, leaves)) = parse_use_bindings(text)
        {
            out.push(UseStatement {
                text: text.to_string(),
                line: node.start_position().row as u32 + 1,
                glob,
                leaves,
            });
        }
        // `use_declaration` nodes do not nest; no need to descend further.
        return;
    }
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            collect_use_declarations(cursor.node(), src, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Fallback line scan (grammar unavailable): one physical line per statement.
/// Misses multi-line grouped imports but never fails the move.
fn parse_use_statements_linescan(source: &str) -> Vec<UseStatement> {
    let mut out = Vec::new();
    for (idx, raw) in source.lines().enumerate() {
        if let Some((glob, leaves)) = parse_use_bindings(raw) {
            out.push(UseStatement {
                text: raw.to_string(),
                line: (idx + 1) as u32,
                glob,
                leaves,
            });
        }
    }
    out
}

/// Parses the bindings out of a single `use` statement's text (possibly
/// multi-line). Accepts `use ...;` and `pub use ...;`. Returns `None` for lines
/// that are not a complete `use` statement or that bring nothing into scope.
fn parse_use_bindings(stmt_text: &str) -> Option<(bool, Vec<UseLeaf>)> {
    let line = stmt_text.trim();
    let after_use = line
        .strip_prefix("pub use ")
        .or_else(|| line.strip_prefix("use "))?
        .trim();
    let body = after_use.strip_suffix(';')?;
    let glob = body.contains('*');
    let leaves: Vec<UseLeaf> = if let Some(open) = body.find('{') {
        let inner = body[open + 1..].trim_end().trim_end_matches('}');
        inner
            .split(',')
            .filter_map(|item| leaf_binding(item.trim()))
            .map(|binding| UseLeaf { binding })
            .collect()
    } else {
        leaf_binding(body)
            .map(|binding| vec![UseLeaf { binding }])
            .unwrap_or_default()
    };
    if leaves.is_empty() && !glob {
        return None;
    }
    Some((glob, leaves))
}

/// The in-code binding for a single `use` path segment (`a::b::C` -> `C`,
/// `a::C as D` -> `D`). Returns `None` for globs and empty items.
fn leaf_binding(item: &str) -> Option<String> {
    let item = item.trim();
    if item.is_empty() || item == "*" || item.ends_with("::*") {
        return None;
    }
    if let Some((_, alias)) = item.rsplit_once(" as ") {
        let alias = alias.trim();
        return (!alias.is_empty()).then(|| alias.to_string());
    }
    let last = item.rsplit("::").next()?.trim();
    (!last.is_empty() && last != "self").then(|| last.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use tracedecay_code_extraction::source_mask::{MaskOptions, masked_rust_source_with};

    use super::{body_identifiers, parse_use_statements, portable_dependency_import};

    #[test]
    fn body_identifiers_collects_names_not_numbers() {
        let ids = body_identifiers("let x = LineItem { unit_price: 3u64 };");
        assert!(ids.contains("LineItem"));
        assert!(ids.contains("unit_price"));
        assert!(ids.contains("let"));
        assert!(!ids.contains("3u64"));
        assert!(!ids.contains("3"));
    }

    #[test]
    fn parse_use_statements_handles_forms() {
        let src = "use std::collections::HashMap;\npub use crate::a::B as C;\nuse crate::x::{Y, Z};\nuse crate::glob::*;\n";
        let stmts = parse_use_statements(src);
        assert_eq!(stmts.len(), 4);
        assert_eq!(stmts[0].leaves[0].binding, "HashMap");
        assert!(!stmts[0].glob);
        assert_eq!(stmts[1].leaves[0].binding, "C");
        assert_eq!(stmts[2].leaves.len(), 2);
        assert_eq!(stmts[2].leaves[0].binding, "Y");
        assert_eq!(stmts[2].leaves[1].binding, "Z");
        assert!(stmts[3].glob);
    }

    #[test]
    fn portable_dependency_import_rejects_source_relative_paths() {
        assert_eq!(
            portable_dependency_import("use crate::shared::Thing;").as_deref(),
            Some("use crate::shared::Thing;")
        );
        assert_eq!(
            portable_dependency_import("use external_crate::Thing;").as_deref(),
            Some("use external_crate::Thing;")
        );
        assert_eq!(portable_dependency_import("use self::Thing;"), None);
        assert_eq!(portable_dependency_import("use super::Thing;"), None);
        assert_eq!(
            portable_dependency_import("use super::parent::Thing;"),
            None
        );
    }

    #[test]
    fn portable_dependency_import_does_not_create_a_new_reexport() {
        assert_eq!(
            portable_dependency_import("pub use crate::shared::Thing;").as_deref(),
            Some("use crate::shared::Thing;")
        );
    }

    /// Mirror `analyze_dependencies`' scan: mask comments/strings (preserving
    /// format captures) then collect identifier tokens.
    fn masked_idents(src: &str) -> HashSet<String> {
        body_identifiers(&masked_rust_source_with(src, MaskOptions::UNUSED_IMPORTS))
    }

    #[test]
    fn masked_scan_drops_doc_and_comment_mentions() {
        let src = "/// see [`compute_total`]\npub fn f(x: LineItem) -> u64 {\n    x.v // compute_total again\n}\n";
        let ids = masked_idents(src);
        assert!(ids.contains("LineItem"), "code idents kept: {ids:?}");
        assert!(
            !ids.contains("compute_total"),
            "doc/comment mention dropped: {ids:?}"
        );
    }

    #[test]
    fn masked_scan_handles_block_comments() {
        let ids = masked_idents("let a = 1; /* Foo bar */ let b = Baz;");
        assert!(ids.contains("Baz"));
        assert!(!ids.contains("Foo"));
    }

    #[test]
    fn masked_scan_drops_string_literal_identifiers() {
        // An identifier that appears only inside a string literal is not a
        // dependency and must not be counted.
        let ids = masked_idents(r#"fn f() { let s = "Widget in a string"; let x = Gadget; }"#);
        assert!(ids.contains("Gadget"), "real code ident kept: {ids:?}");
        assert!(
            !ids.contains("Widget"),
            "string-literal ident dropped: {ids:?}"
        );
    }

    #[test]
    fn masked_scan_keeps_format_captures() {
        // `format!("{helper_result}")`-style implicit captures ARE real uses.
        let ids = masked_idents(r#"fn f() -> String { format!("{helper_result}") }"#);
        assert!(
            ids.contains("helper_result"),
            "implicit format capture counted: {ids:?}"
        );
    }

    #[test]
    fn parse_use_statements_handles_multiline_grouped_import() {
        let src = "use crate::x::{\n    Alpha,\n    Beta,\n    Gamma,\n};\nfn f() {}\n";
        let stmts = parse_use_statements(src);
        assert_eq!(stmts.len(), 1, "one grouped statement: {}", stmts.len());
        let bindings: Vec<&str> = stmts[0].leaves.iter().map(|l| l.binding.as_str()).collect();
        assert_eq!(bindings, vec!["Alpha", "Beta", "Gamma"]);
        assert_eq!(stmts[0].line, 1);
        assert!(stmts[0].text.contains("Gamma"), "text is whole statement");
    }
}

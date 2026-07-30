//! Workspace compatibility crate for TraceDecay's patched Rust grammar.
//!
//! The canonical generated sources and queries ship in
//! `tracedecay-code-extraction`; this wrapper preserves the upstream
//! `tree-sitter-rust` API for workspace dependencies resolved through
//! `[patch.crates-io]`.

use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_rust() -> *const ();
}

/// The tree-sitter [`LanguageFn`] for the patched Rust grammar.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_rust) };

/// Generated node type metadata for the patched grammar.
pub const NODE_TYPES: &str = include_str!(
    "../../../../crates/tracedecay-code-extraction/vendor/tree-sitter-rust/src/node-types.json"
);
/// Syntax highlighting query for the patched grammar.
pub const HIGHLIGHTS_QUERY: &str = include_str!(
    "../../../../crates/tracedecay-code-extraction/vendor/tree-sitter-rust/queries/highlights.scm"
);
/// Injection query for the patched grammar.
pub const INJECTIONS_QUERY: &str = include_str!(
    "../../../../crates/tracedecay-code-extraction/vendor/tree-sitter-rust/queries/injections.scm"
);
/// Symbol tagging query for the patched grammar.
pub const TAGS_QUERY: &str = include_str!(
    "../../../../crates/tracedecay-code-extraction/vendor/tree-sitter-rust/queries/tags.scm"
);

#[cfg(test)]
mod tests {
    #[test]
    fn grammar_loads() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&super::LANGUAGE.into())
            .expect("load patched Rust grammar");
    }
}

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

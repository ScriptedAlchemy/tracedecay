//! TraceDecay's package-owned patched Rust grammar.

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

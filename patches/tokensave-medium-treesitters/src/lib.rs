//! Common tree-sitter grammars for tokensave.
//!
//! Tier: **medium** — languages above the lite tier (C++, C#, Ruby, Swift,
//! Scala, PHP, TSX, Bash, Lua, Dart).
//!
//! Kotlin is intentionally omitted: TraceDecay registers it from
//! `arborium-kotlin` so the large-bundle copy and a second medium-bundle copy
//! are never both linked.

pub use tokensave_lite_treesitters;
pub use tree_sitter;

pub mod languages {
    pub use tokensave_lite_treesitters::languages::*;
    pub use tree_sitter_bash;
    pub use tree_sitter_cpp;
    pub use tree_sitter_c_sharp;
    pub use tree_sitter_dart_orchard;
    pub use tree_sitter_lua;
    pub use tree_sitter_php;
    pub use tree_sitter_ruby;
    pub use tree_sitter_scala;
    pub use tree_sitter_swift;
}

/// Returns (name, language_fn) pairs for medium-tier languages.
pub fn all_languages() -> Vec<(&'static str, tree_sitter_language::LanguageFn)> {
    let mut langs = tokensave_lite_treesitters::all_languages();
    langs.extend([
        ("cpp", tree_sitter_cpp::LANGUAGE),
        ("c_sharp", tree_sitter_c_sharp::LANGUAGE),
        ("ruby", tree_sitter_ruby::LANGUAGE),
        ("swift", tree_sitter_swift::LANGUAGE),
        ("scala", tree_sitter_scala::LANGUAGE),
        ("php", tree_sitter_php::LANGUAGE_PHP),
        (
            "tsx",
            tokensave_lite_treesitters::languages::tree_sitter_typescript::LANGUAGE_TSX,
        ),
        ("bash", tree_sitter_bash::LANGUAGE),
        ("lua", tree_sitter_lua::LANGUAGE),
        ("dart", tree_sitter_dart_orchard::LANGUAGE),
    ]);
    langs
}

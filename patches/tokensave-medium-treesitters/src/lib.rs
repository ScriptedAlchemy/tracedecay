//! Common tree-sitter grammars for tokensave.
//!
//! Tier: **medium** — the lite tier (Rust, Python, JavaScript, TypeScript,
//! Go, Java, C) plus C++, C#, Ruby, Swift, Scala, PHP, TSX, Bash, Lua, Dart.
//!
//! Kotlin is intentionally omitted: TraceDecay registers it from
//! `arborium-kotlin` so the large-bundle copy and a second medium-bundle copy
//! are never both linked.

pub mod languages {
    pub use tree_sitter_bash;
    pub use tree_sitter_c;
    pub use tree_sitter_c_sharp;
    pub use tree_sitter_cpp;
    pub use tree_sitter_dart_orchard;
    pub use tree_sitter_go;
    pub use tree_sitter_java;
    pub use tree_sitter_javascript;
    pub use tree_sitter_lua;
    pub use tree_sitter_php;
    pub use tree_sitter_python;
    pub use tree_sitter_ruby;
    pub use tree_sitter_rust;
    pub use tree_sitter_scala;
    pub use tree_sitter_swift;
    pub use tree_sitter_typescript;
}

/// Returns (name, language_fn) pairs for medium-tier languages.
pub fn all_languages() -> Vec<(&'static str, tree_sitter_language::LanguageFn)> {
    vec![
        ("rust", tree_sitter_rust::LANGUAGE),
        ("python", tree_sitter_python::LANGUAGE),
        ("javascript", tree_sitter_javascript::LANGUAGE),
        ("typescript", tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
        ("go", tree_sitter_go::LANGUAGE),
        ("java", tree_sitter_java::LANGUAGE),
        ("c", tree_sitter_c::LANGUAGE),
        ("cpp", tree_sitter_cpp::LANGUAGE),
        ("c_sharp", tree_sitter_c_sharp::LANGUAGE),
        ("ruby", tree_sitter_ruby::LANGUAGE),
        ("swift", tree_sitter_swift::LANGUAGE),
        ("scala", tree_sitter_scala::LANGUAGE),
        ("php", tree_sitter_php::LANGUAGE_PHP),
        ("tsx", tree_sitter_typescript::LANGUAGE_TSX),
        ("bash", tree_sitter_bash::LANGUAGE),
        ("lua", tree_sitter_lua::LANGUAGE),
        ("dart", tree_sitter_dart_orchard::LANGUAGE),
    ]
}

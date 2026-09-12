//! Tree-sitter grammar provider.
//!
//! All grammars are served from the bundled tree-sitter crate via a
//! lazily-initialised lookup table.

use std::collections::HashMap;
use std::sync::LazyLock;
use tree_sitter::{Language, Parser, Tree};

/// Patched Rust grammar.
pub mod rust_grammar {
    /// The Rust grammar with struct-pattern field attribute support, served by
    /// the git-pinned `tree-sitter-rust` fork in the workspace
    /// `[patch.crates-io]` table.
    pub use tree_sitter_rust::LANGUAGE;
}

// tree-sitter-wgsl 0.0.6 was built against tree-sitter 0.20, whose Language
// type is not assignment-compatible with 0.26. Re-declare the raw C symbol so
// we can construct a LanguageFn with the correct pointer type directly.
#[cfg(feature = "lang-wgsl")]
mod wgsl_grammar {
    use tree_sitter_language::LanguageFn;

    // Grammar compiled from vendor/tree-sitter-wgsl/src/ via build.rs.
    unsafe extern "C" {
        fn tree_sitter_wgsl() -> *const ();
    }
    pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_wgsl) };
}

/// Markdown block and inline grammars. `tree-sitter-md` carries the same
/// generated parser/scanner sources the large bundle vendors, so every tier
/// parses Markdown identically without `lite` linking that bundle.
#[cfg(feature = "lang-markdown")]
pub(crate) mod markdown_grammar {
    pub use tree_sitter_md::{INLINE_LANGUAGE, LANGUAGE};
}

/// Whether any grammar bundle is linked; the patched Rust grammar is
/// registered alongside the bundles, never on its own.
fn has_grammar_bundle() -> bool {
    cfg!(any(feature = "medium-grammars", feature = "large-grammars"))
}

/// Cached map of language key -> `Language` built once from the enabled grammar tiers.
static LANGUAGES: LazyLock<HashMap<&'static str, Language>> =
    LazyLock::new(|| crate::hotpath_observe::measure_grammar_table_init(build_language_table));

/// Grammars served by another registration: Rust by the patched fork below,
/// Markdown by `markdown_grammar` (the large bundle's copy is never used).
/// Only bundle tiers have anything to filter.
#[cfg(any(feature = "medium-grammars", feature = "large-grammars"))]
fn is_bundle_only_grammar(name: &str) -> bool {
    !matches!(name, "rust" | "markdown")
}

fn build_language_table() -> HashMap<&'static str, Language> {
    let languages = std::iter::empty::<(&'static str, Language)>();

    #[cfg(feature = "medium-grammars")]
    let languages = languages.chain(
        tracedecay_medium_treesitters::all_languages()
            .into_iter()
            .filter(|(name, _)| is_bundle_only_grammar(name))
            .map(|(name, lang_fn)| (name, lang_fn.into())),
    );

    #[cfg(feature = "large-grammars")]
    let languages = languages.chain(
        tracedecay_large_treesitters::all_languages()
            .into_iter()
            .filter(|(name, _)| is_bundle_only_grammar(name))
            .map(|(name, lang_fn)| (name, lang_fn.into())),
    );

    let languages = languages.chain(
        std::iter::once(("rust", rust_grammar::LANGUAGE.into())).filter(|_| has_grammar_bundle()),
    );

    #[cfg(feature = "lang-markdown")]
    let languages = languages.chain(std::iter::once((
        "markdown",
        markdown_grammar::LANGUAGE.into(),
    )));

    #[cfg(feature = "lang-wgsl")]
    let languages = languages.chain(std::iter::once(("wgsl", wgsl_grammar::LANGUAGE.into())));

    // HLSL uses the newer LanguageFn API.
    #[cfg(feature = "lang-hlsl")]
    let languages = languages.chain(std::iter::once((
        "hlsl",
        tree_sitter_hlsl::LANGUAGE_HLSL.into(),
    )));

    languages.collect()
}

/// Returns the `tree_sitter::Language` for the given extractor language key.
pub fn try_language(key: &str) -> Result<Language, String> {
    LANGUAGES
        .get(key)
        .cloned()
        .ok_or_else(|| format!("ts_provider: unknown language key '{key}'"))
}

/// Backward-compatible fallible alias for extractor parser call sites.
pub fn language(key: &str) -> Result<Language, String> {
    try_language(key)
}

/// Parse one file with the shared grammar table.
///
/// Language load and `set_language` are the `code_extraction.language` phase.
/// The tree-sitter parse itself is the existing `code_extraction.parse_file`
/// span. Callers keep their grammar-key and error-label strings so failure
/// text stays byte-identical.
pub(crate) fn parse_extractor_source(
    language_key: &str,
    grammar_label: &str,
    source: &str,
) -> Result<Tree, String> {
    parse_extractor_source_inner(language_key, grammar_label, source, false)
}

pub(crate) fn parse_extractor_source_with_labeled_lookup(
    language_key: &str,
    grammar_label: &str,
    source: &str,
) -> Result<Tree, String> {
    parse_extractor_source_inner(language_key, grammar_label, source, true)
}

fn parse_extractor_source_inner(
    language_key: &str,
    grammar_label: &str,
    source: &str,
    label_lookup_error: bool,
) -> Result<Tree, String> {
    let mut parser = crate::hotpath_observe::measure_language(|| {
        let mut parser = Parser::new();
        let language = try_language(language_key).map_err(|error| {
            crate::hotpath_observe::record_grammar_lookup_miss();
            if label_lookup_error {
                format!("failed to load {grammar_label} grammar: {error}")
            } else {
                error
            }
        })?;
        parser.set_language(&language).map_err(|e| {
            crate::hotpath_observe::record_grammar_rejected();
            format!("failed to load {grammar_label} grammar: {e}")
        })?;
        Ok::<_, String>(parser)
    })?;
    crate::hotpath_observe::measure_parse_file(
        grammar_label,
        source.len(),
        || {
            parser
                .parse(source, None)
                .ok_or_else(|| "tree-sitter parse returned None".to_string())
        },
        |result| match result {
            Ok(tree) => {
                crate::hotpath_observe::ParseFileOutcome::from_parsed_root(tree.root_node())
            }
            Err(_) => crate::hotpath_observe::ParseFileOutcome::NoTree,
        },
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn try_language_reports_unknown_key() -> Result<(), String> {
        let Err(err) = super::try_language("definitely-not-registered") else {
            return Err("unknown key should return an error".to_string());
        };
        assert!(err.contains("unknown language key"));
        Ok(())
    }

    #[test]
    fn labeled_parser_preserves_the_extractor_lookup_error_context() {
        let error = super::parse_extractor_source_with_labeled_lookup(
            "definitely-not-registered",
            "fixture",
            "",
        )
        .expect_err("an unknown extractor grammar must fail before parsing");
        assert_eq!(
            error,
            "failed to load fixture grammar: ts_provider: unknown language key \
             'definitely-not-registered'"
        );
    }

    /// A build without the large bundle registers none of its grammars, so a
    /// `lite` build that only wants Markdown cannot reach them.
    #[test]
    #[cfg(not(feature = "large-grammars"))]
    fn large_bundle_keys_are_not_registered_without_the_bundle() -> Result<(), String> {
        for key in ["powershell", "cobol", "protobuf", "zig"] {
            let Err(err) = super::language(key) else {
                return Err(format!(
                    "grammar key '{key}' should not be registered when its bundle is disabled"
                ));
            };
            assert!(err.contains("unknown language key"));
        }
        Ok(())
    }

    #[test]
    #[cfg(not(feature = "lang-markdown"))]
    fn markdown_is_not_registered_without_its_feature() -> Result<(), String> {
        let Err(err) = super::language("markdown") else {
            return Err("markdown should not be registered when lang-markdown is disabled".into());
        };
        assert!(err.contains("unknown language key"));
        Ok(())
    }
}

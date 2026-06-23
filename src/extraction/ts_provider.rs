//! Tree-sitter grammar provider.
//!
//! All grammars are served from the bundled tree-sitter crate via a
//! lazily-initialised lookup table.

use std::collections::HashMap;
use std::sync::LazyLock;
use tree_sitter::Language;

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

/// Cached map of language key -> `Language` built once from the bundled crate.
static LANGUAGES: LazyLock<HashMap<&'static str, Language>> = LazyLock::new(|| {
    #[allow(unused_mut)]
    let mut map: HashMap<&'static str, Language> = tracedecay_large_treesitters::all_languages()
        .into_iter()
        .map(|(name, lang_fn)| (name, lang_fn.into()))
        .collect();

    #[cfg(feature = "lang-wgsl")]
    map.insert("wgsl", wgsl_grammar::LANGUAGE.into());

    // HLSL uses the newer LanguageFn API.
    #[cfg(feature = "lang-hlsl")]
    map.insert("hlsl", tree_sitter_hlsl::LANGUAGE_HLSL.into());

    map
});

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

#[cfg(test)]
mod tests {
    /// Every key that an extractor passes to `try_language()` must be present in the
    /// grammar table. Add new entries here whenever a new extractor is added.
    #[test]
    fn all_extractor_keys_are_registered() {
        #[rustfmt::skip]
        let keys = [
            "bash", "batch", "c", "c_sharp", "clojure", "cobol", "cpp", "dart",
            "dockerfile", "elixir", "erlang", "fortran", "fsharp", "glsl", "go",
            "gwbasic", "haskell", "java", "javascript", "julia", "kotlin", "lean", "lua",
            "msbasic2", "nix", "objc", "ocaml", "pascal", "perl", "php", "powershell",
            "protobuf", "python", "qbasic", "quint", "r", "ruby", "rust", "scala", "sql",
            "swift", "toml", "tsx", "typescript", "vbnet", "zig",
        ];
        // Keys provided by optional direct deps — checked separately so the test
        // is skipped when the feature is not enabled.
        #[cfg(feature = "lang-wgsl")]
        assert!(
            super::LANGUAGES.get("wgsl").is_some(),
            "wgsl grammar missing"
        );
        #[cfg(feature = "lang-hlsl")]
        assert!(
            super::LANGUAGES.get("hlsl").is_some(),
            "hlsl grammar missing"
        );
        let missing: Vec<&str> = keys
            .iter()
            .copied()
            .filter(|k| super::LANGUAGES.get(k).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "grammar keys missing from LANGUAGES: {missing:?}"
        );
    }

    #[test]
    fn language_reports_unknown_key() -> Result<(), String> {
        let Err(err) = super::language("definitely-not-registered") else {
            return Err("unknown key should return an error".to_string());
        };
        assert!(err.contains("unknown language key"));
        Ok(())
    }

    #[test]
    fn try_language_reports_unknown_key() -> Result<(), String> {
        let Err(err) = super::try_language("definitely-not-registered") else {
            return Err("unknown key should return an error".to_string());
        };
        assert!(err.contains("unknown language key"));
        Ok(())
    }
}

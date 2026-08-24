//! In-process structural (AST) search over the project working tree.
//!
//! This is the read-only sibling of the `ast_grep_rewrite` edit tool. Where the
//! rewrite path shells out to the host `ast-grep` binary, structural *search*
//! runs entirely in-process: the [`ast_grep_core`] pattern engine is generic
//! over a tree-sitter [`Language`], so we wire the repo's own bundled grammars
//! (served by [`tracedecay_code_extraction::ts_provider`]) into its `Language` trait.
//!
//! That means:
//!   * no external `ast-grep` CLI requirement (the tool registers
//!     unconditionally),
//!   * no additional tree-sitter grammar crates — the same 0.26 grammars the
//!     indexer builds against back the search,
//!   * pattern parsing/matching happen with zero subprocess spawn per file.
//!
//! The metavariable pre-processing (`$A`, `$$$`, expando chars per language)
//! mirrors `ast-grep-language`'s `pre_process_pattern`, which is the load-bearing
//! transform that lets tree-sitter parse a pattern whose `$` sigils are not valid
//! identifier characters in the target language.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;

use ast_grep_core::matcher::PatternBuilder;
use ast_grep_core::tree_sitter::{LanguageExt, StrDoc, TSLanguage};
use ast_grep_core::{AstGrep, Language, Pattern, PatternError};
use ignore::WalkBuilder;
use ignore::overrides::{Override, OverrideBuilder};

use tracedecay_code_extraction::ts_provider;

/// Bytes sniffed from the head of each file to classify it as binary.
const BINARY_SNIFF_BYTES: usize = 8_192;
/// Skip files larger than this; structural search of a multi-MB generated blob
/// is never what the caller wants and dominates the scan budget.
const MAX_FILE_BYTES: usize = 2_000_000;

/// A tree-sitter language wired into `ast_grep_core`'s pattern engine.
///
/// Wraps a [`TSLanguage`] (which is exactly `tree_sitter::Language`, the same
/// 0.26 type the repo's grammars produce) plus the expando char used to make
/// `$`-metavariable patterns parseable in that language.
#[derive(Clone)]
struct TdLang {
    ts: TSLanguage,
    /// Char substituted for `$` at parse time. `'$'` means "no substitution":
    /// the language accepts `$` in identifiers, so the pattern parses as-is.
    expando: char,
}

impl Language for TdLang {
    fn kind_to_id(&self, kind: &str) -> u16 {
        self.ts.id_for_node_kind(kind, /* named */ true)
    }

    fn field_to_id(&self, field: &str) -> Option<u16> {
        self.ts.field_id_for_name(field).map(|f| f.get())
    }

    fn expando_char(&self) -> char {
        self.expando
    }

    fn pre_process_pattern<'q>(&self, query: &'q str) -> Cow<'q, str> {
        if self.expando == '$' {
            Cow::Borrowed(query)
        } else {
            pre_process_pattern(self.expando, query)
        }
    }

    fn build_pattern(&self, builder: &PatternBuilder) -> Result<Pattern, PatternError> {
        builder.build(|src| StrDoc::try_new(src, self.clone()))
    }
}

impl LanguageExt for TdLang {
    fn get_ts_language(&self) -> TSLanguage {
        self.ts.clone()
    }
}

/// Rewrites the `$` metavariable sigils of an ast-grep pattern to the language's
/// expando char so tree-sitter can parse them as ordinary identifiers.
///
/// Ported verbatim from `ast-grep-language`'s `pre_process_pattern` (MIT).
/// `$A`/`$$A`/`$$$A` (named), `$_`, and `$$$` (anonymous multi) become expando
/// runs; positional/other `$` sequences are left untouched.
fn pre_process_pattern(expando: char, query: &str) -> Cow<'_, str> {
    let mut ret = Vec::with_capacity(query.len());
    let mut dollar_count = 0;
    for c in query.chars() {
        if c == '$' {
            dollar_count += 1;
            continue;
        }
        let need_replace = matches!(c, 'A'..='Z' | '_') // $A or $$A or $$$A
            || dollar_count == 3; // anonymous multiple
        let sigil = if need_replace { expando } else { '$' };
        ret.extend(std::iter::repeat_n(sigil, dollar_count));
        dollar_count = 0;
        ret.push(c);
    }
    // trailing anonymous multiple
    let sigil = if dollar_count == 3 { expando } else { '$' };
    ret.extend(std::iter::repeat_n(sigil, dollar_count));
    Cow::Owned(ret.into_iter().collect())
}

/// Expando char for a `ts_provider` language key, or `None` when the language
/// accepts `$` in identifiers (no substitution needed).
///
/// Values mirror `ast-grep-language`'s per-language `impl_lang_expando!`
/// declarations so pattern semantics match the upstream CLI.
fn expando_for_key(key: &str) -> Option<char> {
    match key {
        // Languages whose grammar rejects `$` as an identifier char: `µ`.
        "c_sharp" | "elixir" | "go" | "haskell" | "hcl" | "kotlin" | "php" | "python" | "ruby"
        | "rust" | "swift" => Some('µ'),
        // C/C++ use an astral-plane char (any letter is a valid identifier start).
        "c" | "cpp" => Some('𐀀'),
        // CSS / Nix use `_` (`$` is interpolation / at-rule syntax there).
        "css" | "nix" => Some('_'),
        // Everything else (js/ts/tsx, java, scala, bash, lua, json, yaml,
        // markdown, dart, solidity, …) parses `$` fine — no expando.
        _ => None,
    }
}

/// Maps a file extension to a `ts_provider` language key. Returns `None` for
/// extensions we do not confidently route; the caller then skips the file. The
/// key is still validated against the compiled grammar tier via
/// [`ts_provider::try_language`], so listing a key here that is not bundled is
/// harmless (the file is skipped).
fn lang_key_for_ext(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "rust",
        "go" => "go",
        "py" | "pyi" => "python",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "scala" | "sc" => "scala",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "c++" | "hpp" | "hh" | "hxx" => "cpp",
        "cs" => "c_sharp",
        "swift" => "swift",
        "rb" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "sh" | "bash" => "bash",
        "dart" => "dart",
        "ex" | "exs" => "elixir",
        "hs" => "haskell",
        "nix" => "nix",
        "css" => "css",
        _ => return None,
    })
}

/// One structural match, resolved to file + 1-based position and the source
/// line it starts on.
#[derive(Debug, Clone)]
pub struct AstGrepSearchMatch {
    pub file: String,
    /// 1-based line of the match start.
    pub line: u32,
    /// 1-based column (character offset) of the match start.
    pub column: u32,
    /// The matched AST node text, collapsed to a single line and length-capped
    /// for display.
    pub matched_text: String,
    /// The full source line the match starts on (trimmed of trailing newline).
    pub line_text: String,
    /// `ts_provider` language key the file was parsed with.
    pub lang: String,
}

/// Result of a structural-search sweep over the working tree.
#[derive(Debug, Clone, Default)]
pub struct AstGrepSearchResult {
    pub matches: Vec<AstGrepSearchMatch>,
    pub files_scanned: usize,
    pub truncated: bool,
}

/// Why a structural search could not run at all (as opposed to returning zero
/// matches, which is a normal empty [`AstGrepSearchResult`]).
#[derive(Debug, Clone)]
pub enum AstGrepSearchError {
    /// `pattern` was empty/whitespace.
    EmptyPattern,
    /// An explicit `lang` key is not a bundled grammar on this build.
    UnknownLang(String),
    /// The pattern failed to compile for the (explicit) language.
    InvalidPattern { lang: String, message: String },
    /// The `path_glob` was not a valid glob.
    InvalidGlob(String),
}

impl std::fmt::Display for AstGrepSearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AstGrepSearchError::EmptyPattern => write!(f, "pattern must not be empty"),
            AstGrepSearchError::UnknownLang(lang) => write!(
                f,
                "unknown or unbundled language '{lang}'. Omit `lang` to auto-detect per file, \
                 or pass a language compiled into this build."
            ),
            AstGrepSearchError::InvalidPattern { lang, message } => {
                write!(f, "invalid {lang} pattern: {message}")
            }
            AstGrepSearchError::InvalidGlob(glob) => write!(f, "invalid path_glob '{glob}'"),
        }
    }
}

/// Compiles a pattern for a language key, or returns the language wrapper +
/// compiled pattern. Cached by the caller.
fn build_lang_pattern(key: &str, pattern: &str) -> Option<Result<(TdLang, Pattern), String>> {
    let ts = ts_provider::try_language(key).ok()?;
    let lang = TdLang {
        ts,
        expando: expando_for_key(key).unwrap_or('$'),
    };
    let compiled = Pattern::try_new(pattern, lang.clone()).map_err(|err| format!("{err}"));
    Some(compiled.map(|pat| (lang, pat)))
}

/// Runs a structural search over `project_root`.
///
/// * `lang` — explicit `ts_provider` language key; when `None`, the language is
///   inferred per file from its extension.
/// * `path_glob` — optional `.gitignore`-style glob restricting the files
///   scanned.
/// * `max_results` — hard cap; one extra match past the cap is collected so
///   truncation can be reported honestly.
pub fn search_tree(
    project_root: &Path,
    pattern: &str,
    lang: Option<&str>,
    path_glob: Option<&str>,
    max_results: usize,
) -> Result<AstGrepSearchResult, AstGrepSearchError> {
    search_tree_scoped(project_root, pattern, lang, path_glob, max_results, None)
}

#[doc(hidden)]
pub fn search_tree_scoped(
    project_root: &Path,
    pattern: &str,
    lang: Option<&str>,
    path_glob: Option<&str>,
    max_results: usize,
    scope_prefix: Option<&str>,
) -> Result<AstGrepSearchResult, AstGrepSearchError> {
    search_tree_scoped_with_cancel(
        project_root,
        pattern,
        lang,
        path_glob,
        max_results,
        scope_prefix,
        || false,
    )
}

#[doc(hidden)]
pub fn search_tree_scoped_with_cancel<F>(
    project_root: &Path,
    pattern: &str,
    lang: Option<&str>,
    path_glob: Option<&str>,
    max_results: usize,
    scope_prefix: Option<&str>,
    is_cancelled: F,
) -> Result<AstGrepSearchResult, AstGrepSearchError>
where
    F: Fn() -> bool,
{
    if pattern.trim().is_empty() {
        return Err(AstGrepSearchError::EmptyPattern);
    }
    let max_results = max_results.max(1);

    // Per-language compiled-pattern cache. `None` = pattern did not compile for
    // that language (skip its files); `Some(_)` = ready to match.
    let mut cache: HashMap<String, Option<(TdLang, Pattern)>> = HashMap::new();

    // With an explicit language, compile up front so a bad pattern is a hard
    // error rather than a silent zero-result sweep.
    if let Some(key) = lang {
        match build_lang_pattern(key, pattern) {
            None => return Err(AstGrepSearchError::UnknownLang(key.to_string())),
            Some(Err(message)) => {
                return Err(AstGrepSearchError::InvalidPattern {
                    lang: key.to_string(),
                    message,
                });
            }
            Some(Ok(built)) => {
                cache.insert(key.to_string(), Some(built));
            }
        }
    }

    let overrides = build_overrides(project_root, path_glob)?;

    let mut builder = WalkBuilder::new(project_root);
    builder
        .follow_links(false)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true);
    if let Some(overrides) = overrides {
        builder.overrides(overrides);
    }

    let mut result = AstGrepSearchResult::default();

    for entry in builder.build() {
        if is_cancelled() {
            return Ok(result);
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();

        // An explicit language applies to every caller-selected file, including
        // extensionless names such as Dockerfile. Without one, infer by suffix.
        let key = match lang {
            Some(explicit) => explicit,
            None => match path
                .extension()
                .and_then(|extension| extension.to_str())
                .and_then(lang_key_for_ext)
            {
                Some(k) => k,
                None => continue,
            },
        };

        let Ok(rel) = path.strip_prefix(project_root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if !path_matches_scope(&rel_str, scope_prefix) {
            continue;
        }

        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if bytes.len() > MAX_FILE_BYTES || looks_binary(&bytes) {
            continue;
        }
        let Ok(source) = String::from_utf8(bytes) else {
            continue;
        };

        // Get-or-compile the pattern for this language.
        let entry = cache.entry(key.to_string()).or_insert_with(|| {
            match build_lang_pattern(key, pattern) {
                Some(Ok(built)) => Some(built),
                _ => None,
            }
        });
        let Some((td_lang, compiled)) = entry.as_ref() else {
            continue;
        };

        result.files_scanned += 1;
        let source_lines: Vec<&str> = source.lines().collect();

        let Ok(doc) = StrDoc::try_new(&source, td_lang.clone()) else {
            continue;
        };
        let ast = AstGrep::doc(doc);
        for node in ast.root().find_all(compiled) {
            if is_cancelled() {
                return Ok(result);
            }
            let start = node.start_pos();
            let line0 = start.line();
            let line_text = source_lines
                .get(line0)
                .map(|l| (*l).to_string())
                .unwrap_or_default();
            result.matches.push(AstGrepSearchMatch {
                file: rel_str.clone(),
                line: (line0 as u32) + 1,
                column: (start.column(&node) as u32) + 1,
                matched_text: collapse_snippet(&node.text()),
                line_text,
                lang: key.to_string(),
            });
            if result.matches.len() > max_results {
                result.truncated = true;
                result.matches.truncate(max_results);
                return Ok(result);
            }
        }
    }

    Ok(result)
}

fn build_overrides(
    project_root: &Path,
    path_glob: Option<&str>,
) -> Result<Option<Override>, AstGrepSearchError> {
    match path_glob {
        Some(raw) if !raw.trim().is_empty() => {
            let mut builder = OverrideBuilder::new(project_root);
            builder
                .add(raw)
                .map_err(|_| AstGrepSearchError::InvalidGlob(raw.to_string()))?;
            let overrides = builder
                .build()
                .map_err(|_| AstGrepSearchError::InvalidGlob(raw.to_string()))?;
            Ok(Some(overrides))
        }
        _ => Ok(None),
    }
}

fn path_matches_scope(path: &str, scope_prefix: Option<&str>) -> bool {
    scope_prefix.is_none_or(|prefix| {
        let with_slash = if prefix.ends_with('/') {
            prefix.to_string()
        } else {
            format!("{prefix}/")
        };
        path.starts_with(&with_slash) || path == prefix
    })
}

/// Collapses a (possibly multi-line) matched snippet to a single display line,
/// squeezing interior whitespace and capping length.
fn collapse_snippet(text: &str) -> String {
    const MAX: usize = 200;
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > MAX {
        let truncated: String = collapsed.chars().take(MAX).collect();
        format!("{truncated}…")
    } else {
        collapsed
    }
}

/// Classifies a byte buffer as binary when a NUL byte appears in the head — the
/// same heuristic `git` and `ripgrep` use.
fn looks_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn pre_process_rewrites_named_and_anonymous_metavars() {
        // Named single/double/triple -> expando; positional `$1` untouched.
        assert_eq!(pre_process_pattern('µ', "f($A)"), "f(µA)");
        assert_eq!(pre_process_pattern('µ', "f($$$)"), "f(µµµ)");
        assert_eq!(pre_process_pattern('µ', "f($1)"), "f($1)");
        assert_eq!(pre_process_pattern('µ', "f($_)"), "f(µ_)");
    }

    #[test]
    fn expando_matches_upstream_table() {
        assert_eq!(expando_for_key("rust"), Some('µ'));
        assert_eq!(expando_for_key("c"), Some('𐀀'));
        assert_eq!(expando_for_key("css"), Some('_'));
        assert_eq!(expando_for_key("javascript"), None);
    }

    #[test]
    fn finds_rust_call_shape_in_process() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "orders.rs",
            "fn place(wh: &mut W, o: &O) {\n    reserve_stock(wh, &item.sku, item.quantity);\n    let t = compute_total(&o.items);\n}\n",
        );
        // A file whose only mention is a comment must not match the call shape.
        write(
            dir.path(),
            "util.js",
            "// reserve_stock is called elsewhere\nconst note = 'reserve_stock(a, b, c)';\n",
        );

        let res = search_tree(dir.path(), "reserve_stock($$$)", None, None, 50).unwrap();
        assert_eq!(res.matches.len(), 1, "matches: {:?}", res.matches);
        let m = &res.matches[0];
        assert_eq!(m.file, "orders.rs");
        assert_eq!(m.line, 2);
        assert_eq!(m.lang, "rust");
        assert!(m.matched_text.contains("reserve_stock"));
    }

    #[test]
    fn cancelled_search_stops_before_scanning() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "calls.rs", "fn f() { target(1); }\n");

        let res = search_tree_scoped_with_cancel(
            dir.path(),
            "target($A)",
            Some("rust"),
            None,
            50,
            None,
            || true,
        )
        .unwrap();

        assert_eq!(res.files_scanned, 0);
        assert!(res.matches.is_empty());
    }

    #[test]
    fn explicit_lang_overrides_extension_inference_for_selected_files() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "fn f() { g(1); }\n");
        write(dir.path(), "b.py", "g(1)\n");
        let res = search_tree(dir.path(), "g($A)", Some("rust"), Some("b.py"), 50).unwrap();
        assert_eq!(res.matches.len(), 1);
        assert_eq!(res.matches[0].file, "b.py");
        assert_eq!(res.matches[0].lang, "rust");
    }

    #[test]
    fn explicit_lang_scans_extensionless_caller_selected_file() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Dockerfile", "FROM alpine\nRUN echo ready\n");

        let res = search_tree(
            dir.path(),
            "RUN echo ready",
            Some("dockerfile"),
            Some("Dockerfile"),
            50,
        )
        .unwrap();

        assert_eq!(res.files_scanned, 1);
        assert_eq!(res.matches.len(), 1, "matches: {:?}", res.matches);
        assert_eq!(res.matches[0].file, "Dockerfile");
    }

    #[test]
    fn path_glob_scopes_the_sweep() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::create_dir(dir.path().join("tests")).unwrap();
        write(
            dir.path().join("src").as_path(),
            "a.rs",
            "fn f() { g(1); }\n",
        );
        write(
            dir.path().join("tests").as_path(),
            "b.rs",
            "fn f() { g(2); }\n",
        );
        let res = search_tree(dir.path(), "g($A)", None, Some("src/**/*.rs"), 50).unwrap();
        assert_eq!(res.matches.len(), 1);
        assert_eq!(res.matches[0].file, "src/a.rs");
    }

    #[test]
    fn unknown_explicit_lang_is_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "fn f() {}\n");
        let err = search_tree(dir.path(), "g($A)", Some("klingon"), None, 50).unwrap_err();
        assert!(matches!(err, AstGrepSearchError::UnknownLang(_)));
    }

    #[test]
    fn empty_pattern_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            search_tree(dir.path(), "   ", None, None, 50).unwrap_err(),
            AstGrepSearchError::EmptyPattern
        ));
    }

    #[test]
    fn truncates_at_max_results() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "a.rs",
            "fn f() {\n g(1);\n g(2);\n g(3);\n g(4);\n}\n",
        );
        let res = search_tree(dir.path(), "g($A)", Some("rust"), None, 2).unwrap();
        assert_eq!(res.matches.len(), 2);
        assert!(res.truncated);
    }
}

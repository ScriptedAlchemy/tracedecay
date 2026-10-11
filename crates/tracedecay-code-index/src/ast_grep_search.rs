//! In-process structural (AST) search over the project working tree.
//!
//! This is the read-only sibling of the `ast_grep_rewrite` edit tool; both run
//! entirely in-process: the [`ast_grep_core`] pattern engine is generic
//! over a tree-sitter [`Language`], so we wire the repo's own bundled grammars
//! (served by [`tracedecay_code_extraction::ts_provider`]) into its `Language` trait.
//!
//! That means:
//!   * no external `ast-grep` CLI requirement (the tool registers
//!     unconditionally),
//!   * no additional tree-sitter grammar crates, the same 0.26 grammars the
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
use std::sync::Arc;

use ast_grep_core::matcher::PatternBuilder;
use ast_grep_core::tree_sitter::{LanguageExt, StrDoc, TSLanguage};
use ast_grep_core::{AstGrep, Language, Pattern, PatternError};

use tracedecay_code_extraction::{LanguageExtractor, LanguageRegistry, ts_provider};

use crate::grep_search::MAX_INTERACTIVE_SOURCE_BYTES;
use crate::source_walk::{forward_slash_relative, source_walk};
use tracedecay_domain::{IndexPathPolicyV1, repository_path_matches_scope};

/// Bytes sniffed from the head of each file to classify it as binary.
const BINARY_SNIFF_BYTES: usize = 8_192;

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
        self.ts.field_id_for_name(field).map(std::num::NonZero::get)
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
        // CSS / Nix / Zig use `_` (`$` is interpolation / sigil syntax there).
        "css" | "nix" | "zig" => Some('_'),
        // Everything else (js/ts/tsx, java, scala, bash, lua, json, yaml,
        // markdown, dart, solidity, …) parses `$` fine, no expando.
        _ => None,
    }
}

/// Grammar key, plus the extractor that prepared parse text.
/// No extractor means the grammar reads the file's own bytes.
struct StructuralRoute<'r> {
    key: String,
    extractor: Option<&'r dyn LanguageExtractor>,
}

/// Extractor dispatch, then extensions no extractor claims (`mjs`, `pyi`,
/// `c++`, …). [`ts_provider::try_language`] still decides whether this build
/// has the grammar.
fn structural_route<'r>(
    registry: &'r LanguageRegistry,
    rel_path: &str,
) -> Option<StructuralRoute<'r>> {
    if let Some(extractor) = registry.extractor_for_file(rel_path) {
        return Some(StructuralRoute {
            key: extractor.retained_grammar_key(rel_path),
            extractor: Some(extractor),
        });
    }
    let key = match rel_path.rsplit('.').next()? {
        "mjs" | "cjs" => "javascript",
        "mts" | "cts" => "typescript",
        "pyi" => "python",
        "c++" => "cpp",
        _ => return None,
    };
    Some(StructuralRoute {
        key: key.to_owned(),
        extractor: None,
    })
}

/// One structural match, resolved to file + 1-based position and the source
/// line it starts on.
#[derive(Debug, Clone)]
pub struct AstGrepSearchMatch {
    pub file: Arc<str>,
    /// Exact UTF-8 byte range minted by the bundled tree-sitter authority.
    pub start_byte: usize,
    pub end_byte: usize,
    /// Tree-sitter node kind. Refactoring planners use this to reject
    /// text-shaped matches that are not identifier syntax.
    pub node_kind: String,
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
    /// Line slices pulled from source text for display. Early caps must
    /// stop pulling; materializing every line first makes this equal the
    /// file's line count even when only a few matches are examined.
    pub lines_visited: usize,
    pub lines_examined: usize,
    pub truncated: bool,
    pub cancelled: bool,
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
/// * `lang`, explicit `ts_provider` language key; when `None`, the language is
///   inferred per file from its extension.
/// * `path_glob`, optional `.gitignore`-style glob restricting the files
///   scanned.
/// * `max_results`, hard cap; one extra match past the cap is collected so
///   truncation can be reported honestly.
#[tracing::instrument(name = "code_index.search.ast_grep", level = "trace", skip_all)]
pub fn search_tree(
    project_root: &Path,
    pattern: &str,
    lang: Option<&str>,
    path_glob: Option<&str>,
    max_results: usize,
    path_policy: &IndexPathPolicyV1,
) -> Result<AstGrepSearchResult, AstGrepSearchError> {
    search_tree_scoped_with_cancel(
        project_root,
        pattern,
        lang,
        path_glob,
        max_results,
        None,
        path_policy,
        || false,
    )
}

#[allow(clippy::too_many_arguments)]
#[tracing::instrument(name = "code_index.search.ast_grep_cancel", level = "trace", skip_all)]
pub fn search_tree_scoped_with_cancel<F>(
    project_root: &Path,
    pattern: &str,
    lang: Option<&str>,
    path_glob: Option<&str>,
    max_results: usize,
    scope_prefix: Option<&str>,
    path_policy: &IndexPathPolicyV1,
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

    let walker = source_walk(project_root, path_glob, path_policy)
        .map_err(|error| AstGrepSearchError::InvalidGlob(error.glob))?;

    // One registry per sweep. An explicit language skips inference.
    let registry = lang.is_none().then(LanguageRegistry::new);

    let mut result = AstGrepSearchResult::default();

    for entry in walker {
        if is_cancelled() {
            result.cancelled = true;
            return Ok(result);
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();

        let Ok(rel) = path.strip_prefix(project_root) else {
            continue;
        };
        let rel_str = forward_slash_relative(rel);
        if !repository_path_matches_scope(rel_str.as_ref(), scope_prefix) {
            continue;
        }

        // An explicit language applies to every selected file, including
        // extensionless names such as Dockerfile.
        let route = match lang {
            Some(explicit) => StructuralRoute {
                key: explicit.to_string(),
                extractor: None,
            },
            None => match registry
                .as_ref()
                .and_then(|registry| structural_route(registry, &rel_str))
            {
                Some(route) => route,
                None => continue,
            },
        };

        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > MAX_INTERACTIVE_SOURCE_BYTES {
            continue;
        }
        if is_cancelled() {
            result.cancelled = true;
            return Ok(result);
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        if looks_binary(&bytes) {
            continue;
        }
        let Ok(source) = String::from_utf8(bytes) else {
            continue;
        };

        // Prepared text keeps the file's offsets, so ranges index `source`.
        let parse_source = route.extractor.map_or_else(
            || Cow::Borrowed(source.as_str()),
            |extractor| extractor.prepare_parse_source(&source),
        );

        // Get-or-compile the pattern for this language.
        let entry = cache.entry(route.key.clone()).or_insert_with(|| {
            match build_lang_pattern(&route.key, pattern) {
                Some(Ok(built)) => Some(built),
                _ => None,
            }
        });
        let Some((td_lang, compiled)) = entry.as_ref() else {
            continue;
        };

        let text = AstGrepFileText {
            source: &source,
            parse_source: &parse_source,
        };
        let file = AstGrepFileIdentity {
            rel_str: &rel_str,
            key: &route.key,
        };
        let stop = if crate::observe::sample_hot_loop() {
            {
                let _span = tracing::trace_span!("code_index_ast_grep_file").entered();
                examine_ast_grep_file(
                    text,
                    td_lang,
                    compiled,
                    file,
                    &mut result,
                    max_results,
                    &is_cancelled,
                )
            }
        } else {
            examine_ast_grep_file(
                text,
                td_lang,
                compiled,
                file,
                &mut result,
                max_results,
                &is_cancelled,
            )
        };
        if stop {
            return Ok(result);
        }
    }

    Ok(result)
}

/// `Ok(None)` when nothing matches. `Err` when `path` has no grammar or the
/// pattern does not compile. The language is inferred from `path`.
#[tracing::instrument(name = "code_index.rewrite.ast_grep", level = "trace", skip_all)]
pub fn rewrite_source(
    path: &Path,
    source: &str,
    pattern: &str,
    rewrite: &str,
) -> Result<Option<String>, String> {
    if pattern.trim().is_empty() {
        return Err(AstGrepSearchError::EmptyPattern.to_string());
    }
    let refused = || {
        format!(
            "no structural grammar for '{}' in this build; use tracedecay_str_replace for text edits",
            path.display()
        )
    };
    let rel_str = forward_slash_relative(path);
    let registry = LanguageRegistry::new();
    let Some(route) = structural_route(&registry, &rel_str) else {
        return Err(refused());
    };
    let Some((lang, compiled)) = build_lang_pattern(&route.key, pattern).transpose()? else {
        return Err(refused());
    };
    let parse_source = route.extractor.map_or_else(
        || Cow::Borrowed(source),
        |extractor| extractor.prepare_parse_source(source),
    );
    let doc = StrDoc::try_new(parse_source.as_ref(), lang)
        .map_err(|error| format!("failed to parse '{}': {error}", path.display()))?;
    let edits = AstGrep::doc(doc).root().replace_all(&compiled, rewrite);
    if edits.is_empty() {
        return Ok(None);
    }
    let mut rewritten = Vec::with_capacity(source.len());
    let mut cursor = 0;
    for edit in edits {
        rewritten.extend_from_slice(&source.as_bytes()[cursor..edit.position]);
        rewritten.extend_from_slice(&edit.inserted_text);
        cursor = edit.position + edit.deleted_length;
    }
    rewritten.extend_from_slice(&source.as_bytes()[cursor..]);
    String::from_utf8(rewritten)
        .map(Some)
        .map_err(|error| format!("structural rewrite produced invalid UTF-8: {error}"))
}

/// Which file a match is attributed to: its project-relative path and the
/// language key. Both are only ever copied into the emitted match, and they
/// always describe the same file, so they travel as one value.
#[derive(Clone, Copy)]
struct AstGrepFileIdentity<'file> {
    rel_str: &'file Arc<str>,
    key: &'file str,
}

/// File bytes and parser input. Offsets match, so ranges index `source`.
#[derive(Clone, Copy)]
struct AstGrepFileText<'file> {
    source: &'file str,
    parse_source: &'file str,
}

fn examine_ast_grep_file<C: Fn() -> bool>(
    text: AstGrepFileText<'_>,
    td_lang: &TdLang,
    compiled: &Pattern,
    file: AstGrepFileIdentity<'_>,
    result: &mut AstGrepSearchResult,
    max_results: usize,
    is_cancelled: &C,
) -> bool {
    result.files_scanned += 1;
    let Ok(doc) = StrDoc::try_new(text.parse_source, td_lang.clone()) else {
        return false;
    };
    let ast = AstGrep::doc(doc);
    for node in ast.root().find_all(compiled) {
        if is_cancelled() {
            result.cancelled = true;
            return true;
        }
        let start = node.start_pos();
        let range = node.range();
        let line0 = start.line();
        result.lines_examined = result.lines_examined.saturating_add(1);
        result.lines_visited = result.lines_visited.saturating_add(1);
        let line_text = source_line_at_byte(text.source, range.start);
        result.matches.push(AstGrepSearchMatch {
            file: Arc::clone(file.rel_str),
            start_byte: range.start,
            end_byte: range.end,
            node_kind: node.kind().into_owned(),
            line: (line0 as u32) + 1,
            column: (start.column(&node) as u32) + 1,
            matched_text: collapse_snippet(
                text.source.get(range.start..range.end).unwrap_or_default(),
            ),
            line_text,
            lang: file.key.to_string(),
        });
        if result.matches.len() > max_results {
            result.truncated = true;
            result.matches.truncate(max_results);
            return true;
        }
    }
    false
}

fn source_line_at_byte(source: &str, byte_offset: usize) -> String {
    let Some(prefix) = source.get(..byte_offset) else {
        return String::new();
    };
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let suffix = source.get(byte_offset..).unwrap_or_default();
    let line_end = suffix
        .find('\n')
        .map_or(source.len(), |index| byte_offset + index);
    source
        .get(line_start..line_end)
        .unwrap_or_default()
        .trim_end_matches('\r')
        .to_owned()
}

/// Collapses a (possibly multi-line) matched snippet to a single display line,
/// squeezing interior whitespace and capping length.
fn collapse_snippet(text: &str) -> String {
    const MAX: usize = 200;
    // Single-pass collapse into one String, no intermediate Vec from
    // split_whitespace().collect().join(" "), and truncate while scanning so
    // long nodes never allocate a second full copy.
    let mut collapsed = String::new();
    let mut chars = 0_usize;
    let mut pending_space = false;
    let mut truncated = false;
    for piece in text.split_whitespace() {
        if piece.is_empty() {
            continue;
        }
        if pending_space {
            if chars >= MAX {
                truncated = true;
                break;
            }
            collapsed.push(' ');
            chars += 1;
        }
        for ch in piece.chars() {
            if chars >= MAX {
                truncated = true;
                break;
            }
            collapsed.push(ch);
            chars += 1;
        }
        if truncated {
            break;
        }
        pending_space = true;
    }
    if truncated {
        collapsed.push('…');
    }
    collapsed
}

/// Classifies a byte buffer as binary when a NUL byte appears in the head, the
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

    fn no_exclusions() -> IndexPathPolicyV1 {
        IndexPathPolicyV1::new(Vec::new(), Vec::new()).unwrap()
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
    fn rewrite_swaps_metavariables_in_code_and_leaves_comments_and_strings() {
        let source = "fn f() {\n    take(a, 1);\n    take(\n        b,\n        2,\n    );\n}\n\
                      // take(a, 1)\nconst S: &str = \"take(a, 1)\";\n";
        let rewritten = rewrite_source(
            Path::new("src/lib.rs"),
            source,
            "take($A, $B)",
            "take($B, $A)",
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            rewritten,
            "fn f() {\n    take(1, a);\n    take(2, b);\n}\n\
             // take(a, 1)\nconst S: &str = \"take(a, 1)\";\n"
        );
    }

    #[test]
    fn rewrite_without_a_match_or_grammar_does_not_rewrite() {
        assert_eq!(
            rewrite_source(Path::new("a.rs"), "fn f() {}\n", "missing($A)", "x"),
            Ok(None)
        );
        let refused =
            rewrite_source(Path::new("notes.txt"), "take(a)\n", "take($A)", "x").unwrap_err();
        assert_eq!(
            refused,
            "no structural grammar for 'notes.txt' in this build; \
             use tracedecay_str_replace for text edits"
        );
    }

    #[test]
    #[cfg(any(feature = "lite", feature = "lang-json"))]
    fn rewrite_applies_structural_edits_in_json() {
        let rewritten = rewrite_source(
            Path::new("config/settings.json"),
            "{\n  \"enabled\": true,\n  \"level\": 1\n}\n",
            "true",
            "false",
        )
        .unwrap()
        .unwrap();
        assert_eq!(rewritten, "{\n  \"enabled\": false,\n  \"level\": 1\n}\n");
    }

    #[test]
    #[cfg(any(feature = "lite", feature = "lang-json"))]
    fn inferred_language_search_scans_json_files() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "settings.json", "{\n  \"enabled\": true\n}\n");

        let res = search_tree(dir.path(), "true", None, None, 50, &no_exclusions()).unwrap();

        let hit = res
            .matches
            .iter()
            .find(|hit| hit.matched_text == "true")
            .expect("json `true` is a structural match");
        assert_eq!(hit.file.as_ref(), "settings.json");
        assert_eq!(hit.lang, "json");
    }

    #[test]
    fn svelte_search_matches_only_inside_the_script_block() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "App.svelte",
            "<style>\n  p { content: target(1); }\n</style>\n<script>\n  target(2);\n</script>\n",
        );

        let res = search_tree(dir.path(), "target($A)", None, None, 50, &no_exclusions()).unwrap();

        assert!(
            res.matches
                .iter()
                .all(|hit| hit.matched_text != "target(1)"),
            "style-block text is not a structural match: {:?}",
            res.matches
        );
        let hit = res
            .matches
            .iter()
            .find(|hit| hit.matched_text == "target(2)")
            .expect("script-block call is a structural match");
        assert_eq!(hit.file.as_ref(), "App.svelte");
        assert_eq!(hit.lang, "typescript");
        assert_eq!(hit.line, 5);
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
            &no_exclusions(),
            || true,
        )
        .unwrap();

        assert!(res.cancelled);
        assert_eq!(res.files_scanned, 0);
        assert!(res.matches.is_empty());
    }

    #[test]
    fn files_above_two_mibibytes_are_not_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let mut oversized = b"fn oversized() { target(1); }\n".to_vec();
        oversized.resize(MAX_INTERACTIVE_SOURCE_BYTES as usize + 1, b' ');
        fs::write(dir.path().join("oversized.rs"), oversized).unwrap();
        write(dir.path(), "tracked.rs", "fn tracked() { target(2); }\n");

        let result = search_tree(
            dir.path(),
            "target($A)",
            Some("rust"),
            None,
            50,
            &no_exclusions(),
        )
        .unwrap();

        assert_eq!(result.files_scanned, 1);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].file.as_ref(), "tracked.rs");
    }

    #[test]
    fn unknown_explicit_lang_is_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "fn f() {}\n");
        let err = search_tree(
            dir.path(),
            "g($A)",
            Some("klingon"),
            None,
            50,
            &no_exclusions(),
        )
        .unwrap_err();
        assert!(matches!(err, AstGrepSearchError::UnknownLang(_)));
    }

    #[test]
    fn empty_pattern_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            search_tree(dir.path(), "   ", None, None, 50, &no_exclusions()).unwrap_err(),
            AstGrepSearchError::EmptyPattern
        ));
    }

    #[test]
    fn early_result_cap_does_not_visit_unexamined_lines() {
        let dir = tempfile::tempdir().unwrap();
        const TOTAL_LINES: usize = 8_192;
        let mut body = String::from("fn f() {\n");
        for index in 0..TOTAL_LINES {
            body.push_str(&format!("    target({index});\n"));
        }
        body.push_str("}\n");
        write(dir.path(), "dense.rs", &body);

        let result = search_tree(
            dir.path(),
            "target($A)",
            Some("rust"),
            None,
            1,
            &no_exclusions(),
        )
        .unwrap();

        assert_eq!(result.matches.len(), 1);
        assert!(result.truncated);
        assert_eq!(result.lines_examined, 2);
        assert!(
            result.lines_visited <= result.lines_examined,
            "visited {} lines after examining {}; full collect materializes every line",
            result.lines_visited,
            result.lines_examined
        );
        assert!(result.lines_visited < TOTAL_LINES);
    }
}

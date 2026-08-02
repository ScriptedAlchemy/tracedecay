//! Tree-sitter driven source masking for the text-scanning analysis handlers.
//!
//! Several analysis scanners (`unused_imports`, the recursion self-call probe,
//! and `unsafe_patterns`) search Rust source *text* for tokens. A naive search
//! treats an identifier or keyword that appears only inside a comment or a
//! string/char literal as a real occurrence — a false positive (or, for
//! unused-imports, a false negative). This module blanks the byte ranges of
//! comment and string/char literal nodes reported by the existing tree-sitter
//! Rust grammar, replacing their bytes with spaces while preserving newlines
//! and total byte length so 1-based line indexing stays valid.
//!
//! Masking is opt-in per node set (`mask_comments` / `mask_strings`) because
//! `tracedecay_todos` deliberately scans comment text and must never be masked.
//!
//! ## Format-capture carry-over
//! Rust's formatting macros accept implicit captures — `println!("{name}")`
//! references the binding `name`, so `name` is a real use of that identifier.
//! When `preserve_format_captures` is set, the contents of the format-string
//! argument of a standard formatting macro are blanked *except* for those
//! `{identifier}` capture spans. Tree-sitter supplies the comment and
//! string/char spans (the job the old hand-rolled lexer did); a tiny linear
//! delimiter walk over the remaining *code* bytes tracks which macro and which
//! argument position each string sits at, so that only the real format-string
//! literal earns the capture exception. Supported macros and their format-arg
//! position (in top-level commas) are listed in [`format_macro_argument_index`].

use tree_sitter::{Node as TsNode, Parser};

/// Which node sets a masking pass blanks, and whether implicit format captures
/// survive string masking.
#[derive(Clone, Copy, Debug)]
pub struct MaskOptions {
    /// Blank `line_comment` / `block_comment` node ranges.
    pub mask_comments: bool,
    /// Blank `string_literal` / `raw_string_literal` / `char_literal` ranges.
    pub mask_strings: bool,
    /// Keep `{identifier}` captures inside a formatting macro's format string.
    /// Only meaningful when `mask_strings` is set.
    pub preserve_format_captures: bool,
}

impl MaskOptions {
    /// Mask comments and string/char literals, preserving implicit format
    /// captures. This is the behaviour the `unused_imports` scan depends on.
    pub const UNUSED_IMPORTS: Self = Self {
        mask_comments: true,
        mask_strings: true,
        preserve_format_captures: true,
    };

    /// Mask comments and string/char literals for code-token scanners (bare
    /// call and unsafe-block detection). Captures are irrelevant here, so they
    /// are blanked with the rest of the string.
    pub const CODE_SCAN: Self = Self {
        mask_comments: true,
        mask_strings: true,
        preserve_format_captures: false,
    };
}

/// Returns a copy of `source` with comment and string/char literal contents
/// blanked, preserving implicit format captures. Equivalent to
/// [`masked_rust_source_with`] using [`MaskOptions::UNUSED_IMPORTS`].
pub fn masked_rust_source(source: &str) -> String {
    masked_rust_source_with(source, MaskOptions::UNUSED_IMPORTS)
}

/// Returns a copy of `source` with the node sets selected by `opts` blanked.
/// Blanked bytes become ASCII spaces; newlines and total byte length are
/// preserved so line/byte indexing over the result stays valid.
///
/// If the source cannot be parsed (grammar unavailable), the original source is
/// returned unmasked — a defensive fallback that only trades masking for the
/// pre-existing false-positive risk on that one file.
pub fn masked_rust_source_with(source: &str, opts: MaskOptions) -> String {
    if !opts.mask_comments && !opts.mask_strings {
        return source.to_string();
    }
    let Some(tree) = parse(source) else {
        return source.to_string();
    };
    let src = source.as_bytes();
    let mut spans = Vec::new();
    collect_spans(tree.root_node(), src, opts, &mut spans);
    spans.sort_by_key(|span| span.start);

    let mut out = src.to_vec();
    if opts.preserve_format_captures && opts.mask_strings {
        blank_with_format_captures(src, &mut out, &spans);
    } else {
        for span in &spans {
            blank_range(&mut out, span.start, span.end);
        }
    }
    // Every replacement is an ASCII space and every untouched byte is the
    // original, so the result is always valid UTF-8; fall back defensively.
    String::from_utf8(out).unwrap_or_else(|_| source.to_string())
}

fn parse(source: &str) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    let language = crate::ts_provider::try_language("rust").ok()?;
    parser.set_language(&language).ok()?;
    parser.parse(source, None)
}

/// A byte range to blank, tagged with the classification needed to decide
/// whether it can carry format captures.
#[derive(Clone, Copy)]
struct MaskSpan {
    start: usize,
    end: usize,
    /// A string/raw-string literal that is not byte-prefixed — the only kind
    /// that can be a formatting macro's format-string argument. Char literals
    /// and byte strings are `false`.
    format_string_candidate: bool,
}

/// Pre-order walk collecting comment and string/char literal spans selected by
/// `opts`. Matched nodes are not descended into: their whole range is recorded
/// and their children (`string_content`, `doc_comment`, …) must not be split
/// out separately.
fn collect_spans(node: TsNode<'_>, src: &[u8], opts: MaskOptions, out: &mut Vec<MaskSpan>) {
    let kind = node.kind();
    let is_comment = matches!(kind, "line_comment" | "block_comment");
    let is_string = matches!(
        kind,
        "string_literal" | "raw_string_literal" | "char_literal"
    );

    if (is_comment && opts.mask_comments) || (is_string && opts.mask_strings) {
        let start = node.start_byte();
        let end = node.end_byte().min(src.len());
        if start < end {
            let format_string_candidate = matches!(kind, "string_literal" | "raw_string_literal")
                && src.get(start) != Some(&b'b');
            out.push(MaskSpan {
                start,
                end,
                format_string_candidate,
            });
        }
        return;
    }

    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            collect_spans(cursor.node(), src, opts, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Replace every non-newline byte in `out[start..end]` with a space.
fn blank_range(out: &mut [u8], start: usize, end: usize) {
    for byte in &mut out[start..end] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}

/// Tracks, per open delimiter, whether the enclosing call is a formatting macro
/// and which argument slot its format string occupies.
#[derive(Clone, Copy)]
struct DelimiterContext {
    closing: u8,
    /// The top-level comma count at which this macro's format string sits, or
    /// `None` when the delimiter does not open a supported formatting macro.
    format_arg_min: Option<usize>,
    top_level_commas: usize,
    format_literal_seen: bool,
}

/// Blank comment and string spans while preserving `{identifier}` captures in
/// the one string literal that is a formatting macro's format-string argument.
///
/// Tree-sitter has already told us where every comment and string/char span is
/// (`spans`, sorted by start). We walk the source once, skipping wholesale over
/// those spans (they never affect delimiter nesting), and maintain a delimiter
/// stack over the remaining code bytes so that reaching a string span we can
/// ask whether it is the active macro's format literal.
fn blank_with_format_captures(src: &[u8], out: &mut [u8], spans: &[MaskSpan]) {
    let mut stack: Vec<DelimiterContext> = Vec::new();
    let mut next_span = 0usize;
    let mut i = 0usize;
    while i < src.len() {
        if let Some(span) = spans.get(next_span)
            && span.start == i
        {
            blank_range(out, span.start, span.end);
            // A byte string never earns the capture exception, so it also does
            // not consume the macro's format-literal slot (matching the short
            // circuit in the reference lexer).
            if span.format_string_candidate && take_format_literal_context(&mut stack) {
                restore_format_captures(src, out, span.start, span.end);
            }
            i = span.end;
            next_span += 1;
            continue;
        }

        match src[i] {
            b @ (b'(' | b'[' | b'{') => stack.push(DelimiterContext {
                closing: match b {
                    b'(' => b')',
                    b'[' => b']',
                    _ => b'}',
                },
                format_arg_min: format_macro_argument_index(&out[..i]),
                top_level_commas: 0,
                format_literal_seen: false,
            }),
            b',' => {
                if let Some(context) = stack.last_mut() {
                    context.top_level_commas += 1;
                }
            }
            b @ (b')' | b']' | b'}')
                if stack.last().is_some_and(|context| context.closing == b) =>
            {
                stack.pop();
            }
            _ => {}
        }
        i += 1;
    }
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Return the top-level comma count at which the format string sits for a
/// standard formatting macro whose opening delimiter immediately follows
/// `prefix` (the already-masked source up to that delimiter), or `None` when
/// `prefix` does not end in a supported `macro!` name.
fn format_macro_argument_index(prefix: &[u8]) -> Option<usize> {
    let mut i = prefix.len();
    while i > 0 && prefix[i - 1].is_ascii_whitespace() {
        i -= 1;
    }
    if i == 0 || prefix[i - 1] != b'!' {
        return None;
    }
    i -= 1;
    let name_end = i;
    while i > 0 && (is_ident_byte(prefix[i - 1]) || prefix[i - 1] == b':') {
        i -= 1;
    }
    let path = std::str::from_utf8(&prefix[i..name_end]).ok()?;
    match path.rsplit("::").next().unwrap_or(path) {
        "format" | "format_args" | "format_args_nl" | "print" | "println" | "eprint"
        | "eprintln" | "panic" | "todo" | "unimplemented" | "unreachable" => Some(0),
        "assert" | "debug_assert" | "write" | "writeln" => Some(1),
        "assert_eq" | "assert_ne" | "debug_assert_eq" | "debug_assert_ne" => Some(2),
        _ => None,
    }
}

/// Mark and accept the first string at the format-argument position of the
/// innermost active formatting macro. Later value arguments remain masked.
fn take_format_literal_context(stack: &mut [DelimiterContext]) -> bool {
    let Some(context) = stack.last_mut() else {
        return false;
    };
    let Some(minimum) = context.format_arg_min else {
        return false;
    };
    if context.format_literal_seen || context.top_level_commas < minimum {
        return false;
    }
    context.format_literal_seen = true;
    true
}

/// Copy `{identifier}` capture bytes from `src` back into an already-blanked
/// string range so they remain visible as real identifier uses. Escaped braces
/// (`{{`) are skipped, matching format-macro semantics.
fn restore_format_captures(src: &[u8], out: &mut [u8], start: usize, end: usize) {
    let mut i = start;
    while i < end {
        if src[i] == b'{' && src.get(i + 1) == Some(&b'{') {
            // `{{` is an escaped literal brace, not a capture opener.
            i += 2;
            continue;
        }
        if src[i] == b'{'
            && let Some(cap_end) = format_capture_identifier_end(src, i + 1)
        {
            let cap_end = cap_end.min(end);
            out[i + 1..cap_end].copy_from_slice(&src[i + 1..cap_end]);
            i = cap_end;
            continue;
        }
        i += 1;
    }
}

/// Return the end of an implicit `{identifier}` capture starting at `start`.
/// A following `:` also counts (`{identifier:?}`); escaped `{{...}}` is handled
/// by the caller before reaching this helper.
fn format_capture_identifier_end(bytes: &[u8], start: usize) -> Option<usize> {
    let first = *bytes.get(start)?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    let mut end = start + 1;
    while bytes.get(end).is_some_and(|byte| is_ident_byte(*byte)) {
        end += 1;
    }
    matches!(bytes.get(end), Some(b'}' | b':')).then_some(end)
}

#[cfg(test)]
mod tests {
    use super::{MaskOptions, masked_rust_source, masked_rust_source_with};

    /// Whole-token match used by the scanners: does `identifier` appear as a
    /// real token (non-identifier boundaries) anywhere on `line`? Mirrors
    /// `has_identifier_match` in the analysis handlers.
    fn contains_token(line: &str, identifier: &str) -> bool {
        let bytes = line.as_bytes();
        let id = identifier.as_bytes();
        if bytes.len() < id.len() || id.is_empty() {
            return false;
        }
        let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let mut i = 0;
        while i + id.len() <= bytes.len() {
            if &bytes[i..i + id.len()] == id {
                let before = i == 0 || !is_ident(bytes[i - 1]);
                let after = i + id.len() == bytes.len() || !is_ident(bytes[i + id.len()]);
                if before && after {
                    return true;
                }
            }
            i += 1;
        }
        false
    }

    /// Does `identifier` survive default (unused-imports) masking of `source`?
    fn referenced(source: &str, identifier: &str) -> bool {
        masked_rust_source(source)
            .lines()
            .any(|line| contains_token(line, identifier))
    }

    #[test]
    fn line_comment_mention_is_masked() {
        // The exact false-negative from the audit fixture: the import is named
        // only in the comment above it.
        let src = "// Planted unused import: BTreeMap is referenced nowhere.\nuse std::collections::BTreeMap;\npub fn f() {}\n";
        assert!(!referenced("// BTreeMap\npub fn f() {}", "BTreeMap"));
        // Sanity: raw (unmasked) text would wrongly see the comment mention.
        assert!(src.contains("BTreeMap"));
    }

    #[test]
    fn block_and_doc_comments_are_masked() {
        assert!(!referenced("/* uses HashMap here */\nfn f() {}", "HashMap"));
        assert!(!referenced(
            "/// doc mentions HashMap\nfn f() {}",
            "HashMap"
        ));
        assert!(!referenced(
            "/* outer /* nested HashMap */ still comment */\nfn f() {}",
            "HashMap"
        ));
    }

    #[test]
    fn string_and_char_literals_are_masked() {
        assert!(!referenced(
            r#"fn f() { let s = "HashMap in a string"; }"#,
            "HashMap"
        ));
        assert!(!referenced(r#"fn f() { let s = "{HashMap}"; }"#, "HashMap"));
        assert!(!referenced(
            r#"fn f() { println!("{{HashMap}}"); }"#,
            "HashMap"
        ));
        assert!(referenced(
            r#"fn f() { println!("{HashMap}"); }"#,
            "HashMap"
        ));
        // A `//` inside a string must NOT start a comment and swallow real code.
        assert!(referenced(
            "fn f() { let url = \"http://x\"; let m = HashMap::new(); }",
            "HashMap"
        ));
        // Char literal containing a quote must not desync the mask.
        assert!(referenced(
            "fn f() { let q = '\"'; let m = HashMap::new(); }",
            "HashMap"
        ));
    }

    #[test]
    fn implicit_captures_survive_supported_format_macro_positions() {
        for source in [
            r#"fn f() { panic!("{HashMap}"); }"#,
            r#"fn f() { assert!(ready, "{HashMap}"); }"#,
            r#"fn f() { assert_eq!(left, right, "{HashMap}"); }"#,
            r#"fn f() { debug_assert_ne!(left, right, "{HashMap}"); }"#,
            r#"fn f() { write!(&mut output, "{HashMap}"); }"#,
            r#"fn f() { writeln!(&mut output, "{HashMap}"); }"#,
            r##"fn f() { write!(&mut output, r#"{HashMap}"#); }"##,
            r#"fn f() { todo!("{HashMap}"); }"#,
            r#"fn f() { unimplemented!("{HashMap}"); }"#,
            r#"fn f() { unreachable!("{HashMap}"); }"#,
            r#"fn f() { std::println! /* comment */ ("{HashMap}"); }"#,
        ] {
            assert!(referenced(source, "HashMap"), "source: {source}");
        }

        for source in [
            r#"fn f() { assert_eq!("{HashMap}", "other"); }"#,
            r#"fn f() { assert!(message.contains("{HashMap}")); }"#,
            r#"fn f() { format!("{value}", "{HashMap}"); }"#,
        ] {
            assert!(!referenced(source, "HashMap"), "source: {source}");
        }
    }

    #[test]
    fn raw_strings_are_masked_without_desync() {
        assert!(!referenced(
            r##"fn f() { let s = r#"HashMap "quoted" //"#; }"##,
            "HashMap"
        ));
        // Real usage after a raw string on the next line survives.
        assert!(referenced(
            "fn f() {\n    let s = r\"raw //\";\n    let m = HashMap::new();\n}",
            "HashMap"
        ));
        assert!(referenced(
            r##"fn f() { println!(r#"{HashMap}"#); }"##,
            "HashMap"
        ));
    }

    #[test]
    fn real_usage_survives_masking() {
        assert!(referenced(
            "use std::collections::HashMap;\nfn f() -> HashMap<u32, u32> { HashMap::new() }",
            "HashMap"
        ));
        // `r`/`b` starting an identifier must not be read as a raw/byte string.
        assert!(referenced(
            "fn f() { for radius in bounds { let _ = radius; } }",
            "radius"
        ));
    }

    #[test]
    fn byte_strings_and_byte_chars_are_masked() {
        assert!(!referenced(
            r#"fn f() { let s = b"HashMap bytes"; }"#,
            "HashMap"
        ));
        assert!(!referenced(
            r"fn f() { let c = b'x'; let HashMap = 1; }",
            "bytes"
        ));
    }

    #[test]
    fn byte_string_format_capture_is_not_preserved() {
        // Byte strings are not valid format arguments; a `{Ident}` inside one is
        // literal bytes, not a capture, so it must be masked away.
        assert!(!referenced(
            r#"fn f() { let _ = b"{HashMap}"; }"#,
            "HashMap"
        ));
    }

    #[test]
    fn code_scan_options_blank_captures_too() {
        // The code-scan preset does not preserve captures, so even a real
        // format capture is blanked (call/unsafe scanners never need it).
        let masked = masked_rust_source_with(
            r#"fn f() { println!("{HashMap}"); }"#,
            MaskOptions::CODE_SCAN,
        );
        assert!(!masked.lines().any(|line| contains_token(line, "HashMap")));
    }

    #[test]
    fn masking_preserves_line_count_and_length() {
        let src = "fn f() {\n    // comment HashMap\n    let s = \"str\";\n}\n";
        let masked = masked_rust_source(src);
        assert_eq!(masked.len(), src.len());
        assert_eq!(masked.lines().count(), src.lines().count());
    }
}

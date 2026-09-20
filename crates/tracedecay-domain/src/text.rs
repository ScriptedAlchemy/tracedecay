//! UTF-8, whitespace, and path-text cuts shared by crates that sit below the
//! runtime kernel.
//!
//! Boundary behavior is the contract: a byte budget that lands inside a
//! multibyte character walks back, and an empty budget or an empty string
//! stays empty. Callers that trim, mark truncation, or refuse a mid-character
//! budget still do that themselves.

/// Replace `\` with `/`.
///
/// Trailing separators stay. `\` becomes `/`, `foo\` becomes `foo/`, and
/// `foo/` is unchanged. An empty string stays empty. This does not trim,
/// lowercase a drive letter, or strip a `\\?\` prefix.
#[must_use]
pub fn forward_slash_text(text: &str) -> String {
    text.replace('\\', "/")
}

/// [`forward_slash_text`] of a path's lossy display form.
///
/// A trailing separator on the path is kept. A non-UTF-8 component is the
/// usual `U+FFFD` replacement, the same spelling `to_string_lossy` already
/// produced at the deleted call sites.
#[must_use]
pub fn forward_slash_path(path: &std::path::Path) -> String {
    forward_slash_text(&path.to_string_lossy())
}

/// Join Unicode whitespace-separated pieces with a single ASCII space.
///
/// Leading, trailing, and repeated whitespace disappear. An empty or
/// all-whitespace input stays empty. Newlines become spaces. Characters
/// that are not whitespace, including a trailing `/`, are preserved.
#[must_use]
pub fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Longest prefix of `text` whose byte length is at most `max_bytes`.
///
/// A cut inside a multibyte character walks back to the previous char
/// boundary. `max_bytes == 0`, and a leading multibyte character whose
/// budget cannot hold it, both yield an empty prefix. An index past the
/// end returns the whole string.
#[must_use]
pub fn utf8_prefix_at_or_before(text: &str, max_bytes: usize) -> &str {
    &text[..text.floor_char_boundary(max_bytes)]
}

#[cfg(test)]
mod tests {
    use super::{collapse_whitespace, forward_slash_text, utf8_prefix_at_or_before};

    #[test]
    fn forward_slashes_keep_trailing_separators_and_an_empty_string() {
        assert_eq!(forward_slash_text(""), "");
        assert_eq!(forward_slash_text(r"\"), "/");
        assert_eq!(forward_slash_text(r"foo\"), "foo/");
        assert_eq!(forward_slash_text("foo/"), "foo/");
        assert_eq!(forward_slash_text(r"C:\repo\\"), "C:/repo//");
        assert_eq!(forward_slash_text(r"\\?\C:\repo"), "//?/C:/repo");
        assert_eq!(
            super::forward_slash_path(std::path::Path::new(r"foo\bar\")),
            "foo/bar/"
        );
    }

    #[test]
    fn collapse_whitespace_keeps_non_space_and_drops_only_whitespace() {
        assert_eq!(collapse_whitespace("  a \n\t b  "), "a b");
        assert_eq!(collapse_whitespace("   "), "");
        assert_eq!(collapse_whitespace(""), "");
        assert_eq!(collapse_whitespace("src/"), "src/");
    }

    #[test]
    fn walks_back_when_the_cut_lands_inside_a_multibyte_char() {
        let text = format!("{}é", "a".repeat(20));
        assert_eq!(utf8_prefix_at_or_before(&text, 21), "a".repeat(20));
    }

    #[test]
    fn empty_budget_on_a_leading_multibyte_char_is_empty() {
        assert_eq!(utf8_prefix_at_or_before("🦀tail", 2), "");
        assert_eq!(utf8_prefix_at_or_before("🦀tail", 0), "");
    }

    #[test]
    fn budget_past_the_end_returns_the_whole_string() {
        assert_eq!(utf8_prefix_at_or_before("abc", 10), "abc");
        assert_eq!(utf8_prefix_at_or_before("", 4), "");
    }
}

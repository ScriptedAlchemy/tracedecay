//! UTF-8, whitespace, and path-text cuts shared by crates that sit below the
//! runtime kernel.
//!
//! Boundary behavior is the contract: a byte budget that lands inside a
//! multibyte character walks back, and an empty budget or an empty string
//! stays empty. Callers that trim, mark truncation, or refuse a mid-character
//! budget still do that themselves.

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
    use super::utf8_prefix_at_or_before;

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

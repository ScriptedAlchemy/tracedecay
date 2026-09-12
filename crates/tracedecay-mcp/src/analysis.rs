//! Byte-level scanning primitives shared by the hand-rolled Rust readers in
//! the MCP analysis handler tree (`constructors`, `field_sites`, `imports`,
//! `recursion`).
//!
//! Everything here is deliberately tiny and total: no allocation, no panics on
//! out-of-range indices. Callers scan `&[u8]` and convert back to line numbers
//! only when they emit a finding. Offsets past the end of the input clamp to
//! the end, so an out-of-range offset reports the last line / end of input
//! rather than aborting the scan.

/// True for the bytes that may appear inside a Rust identifier. Used to give
/// substring matches word boundaries so `read` does not match `spread`.
pub fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The 1-based line number containing byte offset `byte`.
///
/// Counted over the raw bytes, so an offset inside a multibyte character maps
/// to that character's line; an offset at or past the end of `source` reports
/// the last line. A source with more lines than `u32` can name reports
/// `u32::MAX`.
pub fn line_number_at(source: &str, byte: usize) -> u32 {
    let bytes = source.as_bytes();
    let prefix = &bytes[..byte.min(bytes.len())];
    // Splitting on newlines yields one segment more than there are newlines,
    // which is exactly the 1-based line number.
    let line = prefix.split(|c| *c == b'\n').count();
    u32::try_from(line).unwrap_or(u32::MAX)
}

/// The first index at or after `from` that is not ASCII whitespace, or
/// `bytes.len()` when the rest of the input is whitespace or `from` is
/// already at or past the end.
pub fn skip_ascii_whitespace(bytes: &[u8], from: usize) -> usize {
    let from = from.min(bytes.len());
    from + bytes[from..]
        .iter()
        .take_while(|b| b.is_ascii_whitespace())
        .count()
}

#[cfg(test)]
mod tests {
    use super::{line_number_at, skip_ascii_whitespace};

    #[test]
    fn line_numbers_are_one_based_and_count_newlines_before_the_offset() {
        let source = "a\nbb\n\nccc";
        assert_eq!(line_number_at(source, 0), 1);
        assert_eq!(
            line_number_at(source, 1),
            1,
            "the newline byte is on its line"
        );
        assert_eq!(line_number_at(source, 2), 2);
        assert_eq!(line_number_at(source, 5), 3);
        assert_eq!(line_number_at(source, 6), 4);
        assert_eq!(
            line_number_at(source, source.len()),
            4,
            "EOF is on the last line"
        );
    }

    #[test]
    fn whitespace_skipping_clamps_to_the_input_length() {
        let bytes = b"  \t\nx  ";
        assert_eq!(skip_ascii_whitespace(bytes, 0), 4);
        assert_eq!(skip_ascii_whitespace(bytes, 4), 4);
        assert_eq!(skip_ascii_whitespace(bytes, 5), bytes.len());
        assert_eq!(skip_ascii_whitespace(bytes, bytes.len()), bytes.len());
        assert_eq!(skip_ascii_whitespace(bytes, bytes.len() + 10), bytes.len());
        assert_eq!(skip_ascii_whitespace(b"", 3), 0);
    }
}

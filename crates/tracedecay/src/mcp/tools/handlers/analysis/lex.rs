//! Byte-level scanning primitives shared by the hand-rolled Rust readers in
//! this module tree (`constructors`, `field_sites`, `imports`, `recursion`).
//!
//! Everything here is deliberately tiny and total: no allocation, no panics on
//! out-of-range indices. Callers scan `&[u8]` and convert back to line numbers
//! only when they emit a finding.

/// True for the bytes that may appear inside a Rust identifier. Used to give
/// substring matches word boundaries so `read` does not match `spread`.
pub(super) fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The 1-based line number containing `byte`.
pub(super) fn line_number_at(source: &str, byte: usize) -> u32 {
    source[..byte].bytes().filter(|c| *c == b'\n').count() as u32 + 1
}

/// The first index at or after `from` that is not ASCII whitespace, or
/// `bytes.len()` when the rest of the input is whitespace.
pub(super) fn skip_ascii_whitespace(bytes: &[u8], from: usize) -> usize {
    let mut probe = from;
    while let Some(b) = bytes.get(probe) {
        if b.is_ascii_whitespace() {
            probe += 1;
        } else {
            break;
        }
    }
    probe
}

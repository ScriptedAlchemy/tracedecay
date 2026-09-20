//! Shared fixture constructors for tests in every crate that already depends
//! on this one.
//!
//! The values are not hashes of anything. Suites used to each re-roll the same
//! `sha256:` spelling.

use crate::ManifestDigest;

/// `sha256:` plus `digit` repeated 64 times.
///
/// Not a hash. One lowercase hex digit parses as a manifest digest and stays
/// distinct from its siblings.
pub fn repeated_sha256_text(digit: char) -> String {
    format!("sha256:{}", digit.to_string().repeat(64))
}

/// [`repeated_sha256_text`] parsed as a manifest digest.
///
/// An illegal digit is a broken fixture, so this fails immediately rather than
/// handing each suite its own `unwrap`.
pub fn digest(digit: char) -> ManifestDigest {
    ManifestDigest::new(repeated_sha256_text(digit)).expect("fixture digest is canonical")
}

#[cfg(test)]
mod tests {
    use super::{digest, repeated_sha256_text};

    #[test]
    fn repeated_hex_digest_matches_the_shared_spelling() {
        let spelled = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        assert_eq!(repeated_sha256_text('a'), spelled);
        assert_eq!(digest('a').as_str(), spelled);
        assert_eq!(
            repeated_sha256_text('0'),
            crate::ManifestDigest::zero().unwrap().as_str()
        );
    }
}

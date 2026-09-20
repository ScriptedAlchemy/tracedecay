//! Shared fixture constructors for tests in every crate that already depends
//! on this one.
//!
//! The values are not hashes of anything. Suites used to each re-roll the same
//! `sha256:` spelling and the same `TryFrom<String>` identity parse.

use std::fmt::Debug;

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

/// Parses a fixture identity. The value must already be canonical for `T`.
pub fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: Debug,
{
    T::try_from(value.to_owned()).expect("fixture id is canonical")
}

#[cfg(test)]
mod tests {
    use super::{digest, id, repeated_sha256_text};

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

    #[test]
    fn fixture_id_accepts_a_canonical_project_id() {
        let project: crate::ProjectId = id("project.fixture");
        assert_eq!(project.as_str(), "project.fixture");
    }
}

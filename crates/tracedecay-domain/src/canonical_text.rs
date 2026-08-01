//! The one canonical-text predicate shared by every bounded identity, label,
//! and free-text value in the domain and store contracts.
//!
//! A canonical string is non-empty, already trimmed, and free of control
//! characters. Callers add their own byte bound and, more importantly, their
//! own rejection mapping: some contracts distinguish an empty value from a
//! merely non-canonical one, others collapse both into a single rejection.
//! Only the predicate is shared — never the error, so no contract's
//! accept/reject reporting changes by reusing it.

use sha2::{Digest, Sha256};

use crate::research::DomainError;

/// Byte bound shared by canonical identities and labels across the contracts.
pub const CANONICAL_TEXT_MAX_BYTES: usize = 512;

/// Non-empty, already trimmed, and free of control characters.
///
/// Unbounded on purpose: contracts that carry a byte bound state it through
/// [`is_canonical_text_within`] so the bound stays visible at the call site.
#[must_use]
pub fn is_canonical_text(value: &str) -> bool {
    !value.is_empty() && value.trim() == value && !value.chars().any(char::is_control)
}

/// [`is_canonical_text`] plus an explicit byte bound.
#[must_use]
pub fn is_canonical_text_within(value: &str, max_bytes: usize) -> bool {
    value.len() <= max_bytes && is_canonical_text(value)
}

/// Exactly `length` characters of lowercase hex.
///
/// The digest encodings in these contracts are always lowercase; an uppercase
/// or mixed-case digest is not a different spelling of the same value, it is a
/// rejected one.
#[must_use]
pub fn is_lowercase_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// An algorithm tag such as `"sha256:"` followed by lowercase hex of exactly
/// `length` characters. The tag must include its separator.
#[must_use]
pub fn is_tagged_lowercase_hex(value: &str, tag: &str, length: usize) -> bool {
    value
        .strip_prefix(tag)
        .is_some_and(|encoded| is_lowercase_hex(encoded, length))
}

/// A native Git object id: lowercase hex at SHA-1 (40) or SHA-256 (64) width.
///
/// Git object ids are the one identity in these contracts that is legitimately
/// two widths, so the pair is stated once here rather than at each validator.
#[must_use]
pub fn is_git_object_id(value: &str) -> bool {
    is_lowercase_hex(value, 40) || is_lowercase_hex(value, 64)
}

/// Lowercase hex encoding of `bytes`, the inverse of [`is_lowercase_hex`].
#[must_use]
pub fn encode_lowercase_hex(bytes: &[u8]) -> String {
    encode_tagged_lowercase_hex("", bytes)
}

/// `tag` followed by the lowercase hex encoding of `bytes`. The tag must
/// include its separator, e.g. `"sha256:"`.
#[must_use]
pub fn encode_tagged_lowercase_hex(tag: &str, bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(tag.len() + bytes.len() * 2);
    encoded.push_str(tag);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

/// Length-prefixed SHA-256 over a domain separator and an ordered list of
/// parts, encoded as lowercase hex.
///
/// Every frame — the domain tag included — is preceded by its big-endian
/// `u64` byte length, so no two different splits of the same concatenated
/// bytes can collide. This is an identity primitive: derived ids already
/// written to disk depend on the exact framing, so the byte layout must never
/// change.
#[must_use]
pub fn canonical_framed_sha256(domain: &[u8], parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    hasher.update((domain.len() as u64).to_be_bytes());
    hasher.update(domain);
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    encode_lowercase_hex(&hasher.finalize())
}

/// Canonical bounded string that reports an empty value distinctly from a
/// non-canonical one.
///
/// This is the shared body behind the identically-specified per-module
/// validators (canonical identities, configuration labels, feedback labels).
pub(crate) fn validate_canonical_string(
    value: &str,
    field: &'static str,
) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::Empty { field });
    }
    if !is_canonical_text_within(value, CANONICAL_TEXT_MAX_BYTES) {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

/// The hex body of a `sha256:`-tagged digest, without the algorithm tag.
///
/// Identities that embed a digest under their own namespace all need the
/// encoding alone, and all reject an untagged digest as non-canonical under
/// their own field name — so only the stripping is shared, not the field.
pub(crate) fn sha256_hex_body<'a>(
    value: &'a str,
    field: &'static str,
) -> Result<&'a str, DomainError> {
    value
        .strip_prefix("sha256:")
        .ok_or(DomainError::NonCanonical { field })
}

/// A native Git object id, rejected as non-canonical at any other shape.
///
/// This is the shared body behind the identically-specified per-module Git
/// object-id validators (repository state, retrieval anchors).
pub(crate) fn validate_git_object_id(value: &str, field: &'static str) -> Result<(), DomainError> {
    if is_git_object_id(value) {
        Ok(())
    } else {
        Err(DomainError::NonCanonical { field })
    }
}

/// Canonical bounded string that reports every rejection, empty included, as
/// non-canonical.
pub(crate) fn validate_canonical_identity(
    value: &str,
    field: &'static str,
) -> Result<(), DomainError> {
    if is_canonical_text_within(value, CANONICAL_TEXT_MAX_BYTES) {
        Ok(())
    } else {
        Err(DomainError::NonCanonical { field })
    }
}

/// Declare `#[serde(transparent)]` string-identity newtypes that share one
/// surface: `new`, `as_str`, `validate`, validating `Deserialize`,
/// `TryFrom<String>`, and `Display`.
///
/// Every identity family in this crate emitted exactly this code and differed
/// only in three axes, so those are the parameters: whether the type carries a
/// JSON schema, which error the family rejects with, and which validator it
/// runs. The rejection `field` is the type name unless the family spells out a
/// label with `=>`; both forms exist because both are already on the wire in
/// error messages.
///
/// The expansion expects `Serialize`, `Deserialize`, `Deserializer`, `fmt`,
/// and (for `schema`) `JsonSchema` in scope at the invocation site, matching
/// the per-module macros this replaces.
macro_rules! validated_string_newtype {
    (@body $name:ident, $error:ty, $validate:path, $field:expr) => {
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, $error> {
                let value = value.into();
                $validate(&value, $field)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn validate(&self) -> Result<(), $error> {
                $validate(&self.0, $field)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }

        impl TryFrom<String> for $name {
            type Error = $error;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };

    (schema, $error:ty, $validate:path; $($name:ident => $field:literal),+ $(,)?) => {$(
        #[doc = concat!("Strongly typed canonical identity: `", stringify!($name), "`.")]
        #[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        $crate::canonical_text::validated_string_newtype!(@body $name, $error, $validate, $field);
    )+};

    (plain, $error:ty, $validate:path; $($name:ident => $field:literal),+ $(,)?) => {$(
        #[doc = concat!("Strongly typed canonical identity: `", stringify!($name), "`.")]
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        $crate::canonical_text::validated_string_newtype!(@body $name, $error, $validate, $field);
    )+};

    (schema, $error:ty, $validate:path; $($name:ident),+ $(,)?) => {$(
        #[doc = concat!("Strongly typed canonical identity: `", stringify!($name), "`.")]
        #[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        $crate::canonical_text::validated_string_newtype!(@body $name, $error, $validate, stringify!($name));
    )+};

    (plain, $error:ty, $validate:path; $($name:ident),+ $(,)?) => {$(
        #[doc = concat!("Strongly typed canonical identity: `", stringify!($name), "`.")]
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        $crate::canonical_text::validated_string_newtype!(@body $name, $error, $validate, stringify!($name));
    )+};
}

pub(crate) use validated_string_newtype;

#[cfg(test)]
mod tests {
    use super::*;

    /// The predicate is the exact conjunction the per-module copies spelled
    /// out inline, so replacing them cannot move any accept/reject boundary.
    #[test]
    fn predicate_matches_the_inlined_conjunction() {
        for value in [
            "",
            " ",
            "ok",
            " lead",
            "trail ",
            "in\tner",
            "in\nner",
            "\u{7f}",
            "unicode-é",
            &"x".repeat(512),
            &"x".repeat(513),
        ] {
            let inlined = !(value.is_empty()
                || value.trim() != value
                || value.len() > CANONICAL_TEXT_MAX_BYTES
                || value.chars().any(char::is_control));
            assert_eq!(
                is_canonical_text_within(value, CANONICAL_TEXT_MAX_BYTES),
                inlined,
                "canonical predicate diverged for {value:?}"
            );
        }
    }

    /// `trim().is_empty()` and `is_empty()` reject the same set once the
    /// already-trimmed requirement is also applied.
    #[test]
    fn blank_and_empty_reject_identically() {
        for value in ["", " ", "\t", "   \n "] {
            assert!(!is_canonical_text(value));
        }
    }

    /// The hex predicate is the exact conjunction the per-module copies
    /// spelled out, including the lowercase-only byte range.
    #[test]
    fn hex_predicate_matches_the_inlined_conjunction() {
        let hex64 = "a".repeat(64);
        for value in [
            "",
            hex64.as_str(),
            &"A".repeat(64),
            &"f".repeat(64),
            &"g".repeat(64),
            &"0".repeat(64),
            &"a".repeat(63),
            &"a".repeat(65),
        ] {
            let inlined = value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
            assert_eq!(
                is_lowercase_hex(value, 64),
                inlined,
                "hex predicate diverged for {value:?}"
            );
        }
    }

    /// The shared encoder is the exact loop the per-module copies spelled out,
    /// over every byte value and over a full-width digest. The four call sites
    /// this replaced fed it unchanged digest material, so byte-identical
    /// encoding here is byte-identical derived identities there.
    #[test]
    fn encoder_matches_the_inlined_loop() {
        fn inlined(tag: &str, bytes: &[u8]) -> String {
            use std::fmt::Write as _;

            let mut encoded = String::with_capacity(tag.len() + bytes.len() * 2);
            encoded.push_str(tag);
            for byte in bytes {
                write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
            }
            encoded
        }

        let every_byte: Vec<u8> = (0..=u8::MAX).collect();
        for bytes in [&[][..], &[0][..], &[0xff][..], &every_byte[..]] {
            for tag in ["", "sha256:", "blake3:"] {
                assert_eq!(
                    encode_tagged_lowercase_hex(tag, bytes),
                    inlined(tag, bytes),
                    "encoder diverged for tag {tag:?}"
                );
            }
            assert_eq!(encode_lowercase_hex(bytes), inlined("", bytes));
        }
    }

    /// Encoding and the acceptance predicate are inverses: every digest the
    /// encoder produces is one the validators accept.
    #[test]
    fn encoded_digests_satisfy_the_predicate() {
        let digest = [0xabu8; 32];
        assert!(is_lowercase_hex(&encode_lowercase_hex(&digest), 64));
        assert!(is_tagged_lowercase_hex(
            &encode_tagged_lowercase_hex("sha256:", &digest),
            "sha256:",
            64
        ));
    }

    /// The Git object-id predicate is the exact conjunction the per-module
    /// copies spelled out, including both accepted widths.
    #[test]
    fn git_object_id_matches_the_inlined_conjunction() {
        for value in [
            "",
            &"a".repeat(39),
            &"a".repeat(40),
            &"a".repeat(41),
            &"a".repeat(63),
            &"a".repeat(64),
            &"a".repeat(65),
            &"A".repeat(40),
            &"g".repeat(40),
            &"0".repeat(64),
        ] {
            let inlined = matches!(value.len(), 40 | 64)
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
            assert_eq!(
                is_git_object_id(value),
                inlined,
                "git object id predicate diverged for {value:?}"
            );
        }
    }

    #[test]
    fn tagged_hex_requires_the_exact_tag() {
        let digest = format!("sha256:{}", "a".repeat(64));
        assert!(is_tagged_lowercase_hex(&digest, "sha256:", 64));
        assert!(!is_tagged_lowercase_hex(&digest, "blake3:", 64));
        assert!(!is_tagged_lowercase_hex(&"a".repeat(64), "sha256:", 64));
        assert!(!is_tagged_lowercase_hex(&digest, "sha256:", 128));
    }

    #[test]
    fn empty_is_reported_distinctly_only_where_specified() {
        assert_eq!(
            validate_canonical_string("", "field"),
            Err(DomainError::Empty { field: "field" })
        );
        assert_eq!(
            validate_canonical_identity("", "field"),
            Err(DomainError::NonCanonical { field: "field" })
        );
    }
}

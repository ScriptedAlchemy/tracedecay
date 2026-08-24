use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::canonical_serializer;
use super::canonical_sink::BufferedSink;
use super::canonical_value::write_canonical;
use super::error::DomainError;
use super::id::ManifestDigest;
use crate::canonical_text::encode_tagged_lowercase_hex;

impl ManifestDigest {
    /// Canonical `sha256:`-tagged encoding of raw SHA-256 digest bytes — the
    /// one constructor for digest material, so call sites never re-roll their
    /// own `format!`/hex loop over an already-computed hash.
    pub fn from_sha256_bytes(digest: &[u8]) -> Result<Self, DomainError> {
        Self::new(encode_tagged_lowercase_hex("sha256:", digest))
    }

    /// All-zero SHA-256 digest (`sha256:` followed by 64 `0` digits).
    ///
    /// Used as the unsigned placeholder while sealing a digest-bearing record.
    pub fn zero() -> Result<Self, DomainError> {
        Self::from_sha256_bytes(&[0u8; 32])
    }
}

pub(super) type CanonicalError = serde_json::Error;
pub(super) type CanonicalResult<T = ()> = Result<T, CanonicalError>;
pub(super) const SERDE_JSON_PRIVATE_TOKEN_PREFIX: &str = "$serde_json::private::";

/// Serialize any domain value to the crate's canonical JSON byte form.
pub fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, DomainError> {
    let mut output = Vec::new();
    canonical_serializer::serialize_canonical(value, &mut output)?;
    Ok(output)
}

/// Serialize a JSON value with recursively lexicographic object keys and no
/// insignificant whitespace.
pub fn canonical_json_value(value: &Value) -> Result<String, DomainError> {
    let mut output = String::new();
    write_canonical(value, &mut output);
    Ok(output)
}

/// Compute the canonical SHA-256 digest encoding used by domain manifests.
///
/// The value is streamed straight into the hasher through a buffered sink; no
/// intermediate `serde_json::Value` tree is materialized, which matters for
/// the six-figure element sets the code index digests on every publish.
pub fn canonical_sha256<T: Serialize>(value: &T) -> Result<ManifestDigest, DomainError> {
    let mut sink = BufferedSink::new(Sha256::new());
    canonical_serializer::serialize_canonical(value, &mut sink)?;
    let digest = sink.finish().finalize();
    ManifestDigest::from_sha256_bytes(&digest)
}

/// Serialize once to canonical JSON and return those bytes with their canonical
/// manifest digest.
///
/// Callers that must persist the canonical bytes avoid traversing large values
/// a second time solely to compute the same digest.
pub fn canonical_json_bytes_and_sha256<T: Serialize>(
    value: &T,
) -> Result<(Vec<u8>, ManifestDigest), DomainError> {
    let bytes = canonical_json_bytes(value)?;
    let digest_bytes = Sha256::digest(&bytes);
    let digest = ManifestDigest::from_sha256_bytes(&digest_bytes)?;
    Ok((bytes, digest))
}

#[cfg(test)]
#[path = "canonical_tests.rs"]
mod tests;

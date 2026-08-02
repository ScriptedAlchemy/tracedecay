use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::canonical_serializer;
use super::canonical_sink::BufferedSink;
use super::canonical_value::write_canonical;
use super::error::DomainError;
use super::id::ManifestDigest;

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
    let mut encoded = String::with_capacity("sha256:".len() + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}")
            .map_err(|error| DomainError::CanonicalSerialization(error.to_string()))?;
    }
    ManifestDigest::new(encoded)
}

#[cfg(test)]
#[path = "canonical_tests.rs"]
mod tests;

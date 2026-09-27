//! Binding a continuation cursor to the request that minted it.
//!
//! Every operation that hands a caller a continuation builds one
//! [`CursorBindingV1`] from its operation name and every parameter that shapes
//! its result set, embeds the binding's [`CursorBindingStampV1`] in the cursor,
//! and checks a presented cursor against the binding the new request builds.
//! A cursor redeemed on another operation, or with any bound parameter changed,
//! is refused rather than paging a result set it was not minted for.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical_text::encode_lowercase_hex;
use crate::research::{DomainError, ManifestDigest, canonical_json_bytes, canonical_sha256};

/// Hex digits of a parameter's digest a cursor carries: enough to name the
/// parameter that changed, while the full binding digest decides the match.
const PARAMETER_FINGERPRINT_HEX_LEN: usize = 8;
const BOUND_CURSOR_PREFIX: &str = "bc1.";

/// Problem code of a cursor presented with a bound parameter changed.
pub const CURSOR_PARAMETER_CHANGED_CODE: &str = "cursor.parameter_changed";
/// Problem code of a cursor this operation did not issue.
pub const CURSOR_INVALID_CODE: &str = "cursor.invalid";

/// Why a presented cursor cannot page the request it was presented with.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum CursorBindingMismatchV1 {
    /// Malformed, or issued by another operation.
    #[error("the cursor was not issued by this operation")]
    Foreign,
    /// Issued by this operation for a request with a different `parameter`.
    #[error("the cursor was issued with a different `{parameter}`")]
    ParameterChanged { parameter: &'static str },
}

impl CursorBindingMismatchV1 {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Foreign => CURSOR_INVALID_CODE,
            Self::ParameterChanged { .. } => CURSOR_PARAMETER_CHANGED_CODE,
        }
    }

    /// The caller-facing explanation, naming the changed parameter.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Foreign => {
                "The cursor was not issued by this operation. Restart without it.".to_owned()
            }
            Self::ParameterChanged { parameter } => format!(
                "The cursor was issued for a request with a different `{parameter}`. Repeat the \
                 request with the parameters that returned the cursor, or restart without it."
            ),
        }
    }
}

/// The operation and named result-shaping parameters a cursor is minted for.
///
/// `digest` covers the operation and every parameter and alone decides
/// whether a cursor matches. The per-parameter fingerprints only name the
/// parameter that changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorBindingV1 {
    operation: String,
    digest: ManifestDigest,
    parameters: Vec<(&'static str, String)>,
}

/// The part of a [`CursorBindingV1`] a cursor carries.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CursorBindingStampV1 {
    operation: String,
    digest: ManifestDigest,
    fingerprints: Vec<String>,
}

/// Accumulates the parameters of a [`CursorBindingV1`] in the order the
/// operation fixes.
#[derive(Debug)]
pub struct CursorBindingBuilderV1 {
    operation: String,
    parameters: Result<Vec<(&'static str, ManifestDigest)>, DomainError>,
}

impl CursorBindingBuilderV1 {
    /// Binds `name`, the caller-visible request field, to `value`.
    #[must_use]
    pub fn parameter<T: Serialize + ?Sized>(mut self, name: &'static str, value: &T) -> Self {
        if let Ok(parameters) = &mut self.parameters {
            match canonical_sha256(&value) {
                Ok(digest) => parameters.push((name, digest)),
                Err(error) => self.parameters = Err(error),
            }
        }
        self
    }

    pub fn build(self) -> Result<CursorBindingV1, DomainError> {
        CursorBindingV1::new(self.operation, self.parameters?)
    }
}

impl CursorBindingV1 {
    #[must_use]
    pub fn builder(operation: impl Into<String>) -> CursorBindingBuilderV1 {
        CursorBindingBuilderV1 {
            operation: operation.into(),
            parameters: Ok(Vec::new()),
        }
    }

    /// `parameters` pairs each caller-visible parameter name with the digest
    /// of its value, in an order fixed by the operation.
    pub fn new(
        operation: impl Into<String>,
        parameters: Vec<(&'static str, ManifestDigest)>,
    ) -> Result<Self, DomainError> {
        let operation = operation.into();
        let digest = canonical_sha256(&(&operation, &parameters))?;
        let parameters = parameters
            .into_iter()
            .map(|(name, digest)| (name, parameter_fingerprint(&digest)))
            .collect();
        Ok(Self {
            operation,
            digest,
            parameters,
        })
    }

    #[must_use]
    pub fn operation(&self) -> &str {
        &self.operation
    }

    #[must_use]
    pub fn stamp(&self) -> CursorBindingStampV1 {
        CursorBindingStampV1 {
            operation: self.operation.clone(),
            digest: self.digest.clone(),
            fingerprints: self
                .parameters
                .iter()
                .map(|(_, fingerprint)| fingerprint.clone())
                .collect(),
        }
    }

    /// Whether a cursor carrying `stamp` may page this request.
    pub fn check(&self, stamp: &CursorBindingStampV1) -> Result<(), CursorBindingMismatchV1> {
        if stamp.operation != self.operation {
            return Err(CursorBindingMismatchV1::Foreign);
        }
        if stamp.digest == self.digest {
            return Ok(());
        }
        if stamp.fingerprints.len() != self.parameters.len() {
            return Err(CursorBindingMismatchV1::Foreign);
        }
        Err(self
            .parameters
            .iter()
            .zip(&stamp.fingerprints)
            .find(|((_, presented), minted)| presented != *minted)
            .map_or(CursorBindingMismatchV1::Foreign, |((parameter, _), _)| {
                CursorBindingMismatchV1::ParameterChanged { parameter }
            }))
    }
}

fn parameter_fingerprint(digest: &ManifestDigest) -> String {
    digest
        .as_str()
        .trim_start_matches("sha256:")
        .chars()
        .take(PARAMETER_FINGERPRINT_HEX_LEN)
        .collect()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundCursorV1<P> {
    binding: CursorBindingStampV1,
    position: P,
}

/// A cursor naming `position` within the result set `binding` describes.
///
/// For operations whose continuation is only a position; codecs that also
/// authenticate or bind snapshot state embed [`CursorBindingV1::stamp`] in
/// their own payload instead.
pub fn encode_bound_cursor<P: Serialize>(
    binding: &CursorBindingV1,
    position: &P,
) -> Result<String, DomainError> {
    let bytes = canonical_json_bytes(&BoundCursorV1 {
        binding: binding.stamp(),
        position,
    })?;
    Ok(format!(
        "{BOUND_CURSOR_PREFIX}{}",
        encode_lowercase_hex(&bytes)
    ))
}

/// The position a cursor from [`encode_bound_cursor`] names, when it was
/// minted for `binding`.
pub fn decode_bound_cursor<P: DeserializeOwned>(
    binding: &CursorBindingV1,
    encoded: &str,
) -> Result<P, CursorBindingMismatchV1> {
    let bytes = encoded
        .strip_prefix(BOUND_CURSOR_PREFIX)
        .and_then(decode_lowercase_hex)
        .ok_or(CursorBindingMismatchV1::Foreign)?;
    let cursor: BoundCursorV1<P> =
        serde_json::from_slice(&bytes).map_err(|_| CursorBindingMismatchV1::Foreign)?;
    binding.check(&cursor.binding)?;
    Ok(cursor.position)
}

fn decode_lowercase_hex(encoded: &str) -> Option<Vec<u8>> {
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    let encoded = encoded.as_bytes();
    if encoded.len() % 2 != 0 {
        return None;
    }
    encoded
        .chunks_exact(2)
        .map(|pair| Some((digit(pair[0])? << 4) | digit(pair[1])?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn callers(node_id: &str, depth: u32) -> CursorBindingV1 {
        CursorBindingV1::builder("callers")
            .parameter("node_id", node_id)
            .parameter("maximum_depth", &depth)
            .build()
            .unwrap()
    }

    #[test]
    fn a_bound_cursor_pages_only_the_request_that_minted_it() {
        let cursor = encode_bound_cursor(&callers("a", 1), &10_u32).unwrap();

        assert_eq!(
            decode_bound_cursor::<u32>(&callers("a", 1), &cursor),
            Ok(10)
        );
        assert_eq!(
            decode_bound_cursor::<u32>(&callers("b", 1), &cursor),
            Err(CursorBindingMismatchV1::ParameterChanged {
                parameter: "node_id"
            })
        );
        assert_eq!(
            decode_bound_cursor::<u32>(&callers("a", 2), &cursor),
            Err(CursorBindingMismatchV1::ParameterChanged {
                parameter: "maximum_depth"
            })
        );
        let callees = CursorBindingV1::builder("callees")
            .parameter("node_id", "a")
            .parameter("maximum_depth", &1_u32)
            .build()
            .unwrap();
        assert_eq!(
            decode_bound_cursor::<u32>(&callees, &cursor),
            Err(CursorBindingMismatchV1::Foreign)
        );
        assert_eq!(
            decode_bound_cursor::<u32>(&callers("a", 1), "bc1.zz"),
            Err(CursorBindingMismatchV1::Foreign)
        );
    }
}

//! Authenticated preparation and pagination for native query results.
//!
//! Adapters remain responsible for resolving storage generations and
//! translating native query records. This authority owns the stable cursor
//! wire, query authentication, owner binding, candidate-set freezing, expiry,
//! and pagination.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, QueryDigest, RetrievalCursorKeyId, RetrievalRequest,
    UtcMicros, canonical_sha256,
};

use super::fusion::{
    PREPARED_QUERY_CURSOR_MAC_DOMAIN_V1, QueryDigestAuthenticationError,
};
use super::{Pr9QueryAuthorityErrorV1, Pr9QueryAuthorityV1};

const PREPARED_QUERY_CURSOR_PREFIX_V1: &str = "ccq1.";
const PREPARED_QUERY_CURSOR_REVISION_V1: u16 = 1;
const PREPARED_QUERY_CURSOR_TTL_MICROS_V1: i64 = 15 * 60 * 1_000_000;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PreparedQueryErrorV1 {
    #[error("prepared query cursor is invalid")]
    Invalid,
    #[error("prepared query cursor is stale")]
    Stale,
    #[error("prepared query authority is unavailable")]
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedQueryBindingsV1 {
    operation: String,
    scope_digest: ManifestDigest,
    generation: CodeGenerationId,
    query_binding_digest: ManifestDigest,
}

impl PreparedQueryBindingsV1 {
    pub fn new(
        operation: impl Into<String>,
        scope_digest: ManifestDigest,
        generation: CodeGenerationId,
        query_binding_digest: ManifestDigest,
    ) -> Result<Self, PreparedQueryErrorV1> {
        let operation = operation.into();
        if operation.is_empty() {
            return Err(PreparedQueryErrorV1::Invalid);
        }
        scope_digest
            .validate()
            .map_err(|_| PreparedQueryErrorV1::Invalid)?;
        generation
            .validate()
            .map_err(|_| PreparedQueryErrorV1::Invalid)?;
        query_binding_digest
            .validate()
            .map_err(|_| PreparedQueryErrorV1::Invalid)?;
        Ok(Self {
            operation,
            scope_digest,
            generation,
            query_binding_digest,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedQueryCursorRoutingV1 {
    pub generation: CodeGenerationId,
    pub expires_at: UtcMicros,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedQueryPageV1<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PreparedQueryCursorPayloadV1 {
    revision: u16,
    operation: String,
    scope_digest: ManifestDigest,
    generation: CodeGenerationId,
    query_binding_digest: ManifestDigest,
    candidate_set_digest: ManifestDigest,
    authentication_key_id: RetrievalCursorKeyId,
    next_offset: u32,
    expires_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AuthenticatedPreparedQueryCursorV1 {
    payload: PreparedQueryCursorPayloadV1,
    authentication: QueryDigest,
}

pub struct PreparedQueryV1 {
    authority: Arc<Pr9QueryAuthorityV1>,
    request: RetrievalRequest,
    cursor: Option<AuthenticatedPreparedQueryCursorV1>,
}

impl PreparedQueryV1 {
    pub fn prepare(
        authority: Arc<Pr9QueryAuthorityV1>,
        request: RetrievalRequest,
        cursor: Option<&str>,
    ) -> Result<Self, PreparedQueryErrorV1> {
        let cursor = cursor
            .map(|encoded| authenticate_cursor(authority.as_ref(), &request, encoded))
            .transpose()?;
        Ok(Self {
            authority,
            request,
            cursor,
        })
    }

    pub fn request(&self) -> &RetrievalRequest {
        &self.request
    }

    pub fn paginate<T>(
        &self,
        bindings: &PreparedQueryBindingsV1,
        items: Vec<T>,
        page_size: u32,
        now: UtcMicros,
    ) -> Result<PreparedQueryPageV1<T>, PreparedQueryErrorV1>
    where
        T: Serialize,
    {
        if page_size == 0 {
            return Err(PreparedQueryErrorV1::Invalid);
        }
        let total = u64::try_from(items.len()).map_err(|_| PreparedQueryErrorV1::Unavailable)?;
        let candidate_set_digest =
            canonical_sha256(&items).map_err(|_| PreparedQueryErrorV1::Unavailable)?;
        let start = match &self.cursor {
            Some(cursor) => {
                require_unexpired(cursor, now)?;
                if cursor.payload.generation != bindings.generation
                    || cursor.payload.candidate_set_digest != candidate_set_digest
                {
                    return Err(PreparedQueryErrorV1::Stale);
                }
                if cursor.payload.operation != bindings.operation
                    || cursor.payload.scope_digest != bindings.scope_digest
                    || cursor.payload.query_binding_digest != bindings.query_binding_digest
                {
                    return Err(PreparedQueryErrorV1::Invalid);
                }
                usize::try_from(cursor.payload.next_offset)
                    .map_err(|_| PreparedQueryErrorV1::Invalid)?
            }
            None => 0,
        };
        if start > items.len() {
            return Err(PreparedQueryErrorV1::Invalid);
        }
        let page_size = usize::try_from(page_size).map_err(|_| PreparedQueryErrorV1::Invalid)?;
        let end = start.saturating_add(page_size).min(items.len());
        let next_cursor = if end < items.len() {
            let expires_at = match &self.cursor {
                Some(cursor) => cursor.payload.expires_at,
                None => UtcMicros(
                    now.0
                        .checked_add(PREPARED_QUERY_CURSOR_TTL_MICROS_V1)
                        .ok_or(PreparedQueryErrorV1::Unavailable)?,
                ),
            };
            let payload = PreparedQueryCursorPayloadV1 {
                revision: PREPARED_QUERY_CURSOR_REVISION_V1,
                operation: bindings.operation.clone(),
                scope_digest: bindings.scope_digest.clone(),
                generation: bindings.generation.clone(),
                query_binding_digest: bindings.query_binding_digest.clone(),
                candidate_set_digest,
                authentication_key_id: self.authority.active_query_key_id(),
                next_offset: u32::try_from(end).map_err(|_| PreparedQueryErrorV1::Unavailable)?,
                expires_at,
            };
            let authentication = self
                .authority
                .authenticate_prepared_cursor_payload(
                    &self.request,
                    &cursor_authentication_payload_bytes(&payload)?,
                )
                .map_err(map_authority_error)?;
            Some(encode_cursor(payload, authentication)?)
        } else {
            None
        };
        Ok(PreparedQueryPageV1 {
            items: items.into_iter().skip(start).take(end - start).collect(),
            total,
            next_cursor,
        })
    }
}

pub fn inspect_prepared_query_cursor(
    encoded: &str,
) -> Result<PreparedQueryCursorRoutingV1, PreparedQueryErrorV1> {
    let cursor = decode_cursor(encoded)?;
    Ok(PreparedQueryCursorRoutingV1 {
        generation: cursor.payload.generation,
        expires_at: cursor.payload.expires_at,
    })
}

fn authenticate_cursor(
    authority: &Pr9QueryAuthorityV1,
    request: &RetrievalRequest,
    encoded: &str,
) -> Result<AuthenticatedPreparedQueryCursorV1, PreparedQueryErrorV1> {
    let cursor = decode_cursor(encoded)?;
    authority
        .verify_prepared_cursor_payload(
            &cursor.payload.authentication_key_id,
            request,
            &cursor_authentication_payload_bytes(&cursor.payload)?,
            &cursor.authentication,
        )
        .map_err(map_authority_error)?;
    Ok(cursor)
}

fn map_authority_error(error: Pr9QueryAuthorityErrorV1) -> PreparedQueryErrorV1 {
    match error {
        Pr9QueryAuthorityErrorV1::QueryAuthentication(
            QueryDigestAuthenticationError::KeyRevoked,
        ) => PreparedQueryErrorV1::Stale,
        Pr9QueryAuthorityErrorV1::QueryAuthentication(
            QueryDigestAuthenticationError::KeyUnavailable,
        )
        | Pr9QueryAuthorityErrorV1::AuthorityUnavailable => PreparedQueryErrorV1::Unavailable,
        _ => PreparedQueryErrorV1::Invalid,
    }
}

fn cursor_authentication_payload_bytes(
    payload: &PreparedQueryCursorPayloadV1,
) -> Result<Vec<u8>, PreparedQueryErrorV1> {
    #[derive(Serialize)]
    struct PreparedQueryCursorAuthenticationPayloadV1<'a> {
        domain: &'static str,
        revision: u16,
        operation: &'a str,
        scope_digest: &'a ManifestDigest,
        generation: &'a CodeGenerationId,
        query_binding_digest: &'a ManifestDigest,
        candidate_set_digest: &'a ManifestDigest,
        next_offset: u32,
        expires_at: UtcMicros,
    }
    serde_json::to_vec(&PreparedQueryCursorAuthenticationPayloadV1 {
        domain: PREPARED_QUERY_CURSOR_MAC_DOMAIN_V1,
        revision: payload.revision,
        operation: &payload.operation,
        scope_digest: &payload.scope_digest,
        generation: &payload.generation,
        query_binding_digest: &payload.query_binding_digest,
        candidate_set_digest: &payload.candidate_set_digest,
        next_offset: payload.next_offset,
        expires_at: payload.expires_at,
    })
    .map_err(|_| PreparedQueryErrorV1::Unavailable)
}

fn encode_cursor(
    payload: PreparedQueryCursorPayloadV1,
    authentication: QueryDigest,
) -> Result<String, PreparedQueryErrorV1> {
    let bytes = serde_json::to_vec(&AuthenticatedPreparedQueryCursorV1 {
        payload,
        authentication,
    })
    .map_err(|_| PreparedQueryErrorV1::Unavailable)?;
    Ok(format!(
        "{PREPARED_QUERY_CURSOR_PREFIX_V1}{}",
        hex::encode(bytes)
    ))
}

fn decode_cursor(
    encoded: &str,
) -> Result<AuthenticatedPreparedQueryCursorV1, PreparedQueryErrorV1> {
    let encoded = encoded
        .strip_prefix(PREPARED_QUERY_CURSOR_PREFIX_V1)
        .ok_or(PreparedQueryErrorV1::Invalid)?;
    let bytes = hex::decode(encoded).map_err(|_| PreparedQueryErrorV1::Invalid)?;
    if hex::encode(&bytes) != encoded {
        return Err(PreparedQueryErrorV1::Invalid);
    }
    let cursor: AuthenticatedPreparedQueryCursorV1 =
        serde_json::from_slice(&bytes).map_err(|_| PreparedQueryErrorV1::Invalid)?;
    if serde_json::to_vec(&cursor).map_err(|_| PreparedQueryErrorV1::Invalid)? != bytes
        || cursor.payload.revision != PREPARED_QUERY_CURSOR_REVISION_V1
    {
        return Err(PreparedQueryErrorV1::Invalid);
    }
    Ok(cursor)
}

fn require_unexpired(
    cursor: &AuthenticatedPreparedQueryCursorV1,
    now: UtcMicros,
) -> Result<(), PreparedQueryErrorV1> {
    if now.0 >= cursor.payload.expires_at.0 {
        Err(PreparedQueryErrorV1::Stale)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(label: &str) -> ManifestDigest {
        canonical_sha256(&label).expect("fixture digest")
    }

    fn cursor(next_offset: u32, expires_at: UtcMicros) -> String {
        encode_cursor(
            PreparedQueryCursorPayloadV1 {
                revision: PREPARED_QUERY_CURSOR_REVISION_V1,
                operation: "code_exact_occurrence".to_owned(),
                scope_digest: digest("scope"),
                generation: CodeGenerationId::new("generation.callable-page").expect("generation"),
                query_binding_digest: digest("query"),
                candidate_set_digest: digest("candidates"),
                authentication_key_id: RetrievalCursorKeyId::new("cursor-key.callable-page")
                    .expect("cursor key"),
                next_offset,
                expires_at,
            },
            QueryDigest::new(
                tracedecay_domain::PrivacyDomainId::new("privacy.callable-page")
                    .expect("privacy domain"),
                1,
                tracedecay_domain::QueryMac::new(format!("hmac-sha256:{}", "7".repeat(64)))
                    .expect("query MAC"),
            ),
        )
        .expect("cursor")
    }

    #[test]
    fn cursor_routing_preserves_generation_and_expiry() {
        let encoded = cursor(2, UtcMicros(1_000));
        let routing = inspect_prepared_query_cursor(&encoded).expect("canonical cursor");
        assert_eq!(
            routing.generation,
            CodeGenerationId::new("generation.callable-page").expect("generation")
        );
        assert_eq!(routing.expires_at, UtcMicros(1_000));
    }

    #[test]
    fn cursor_rejects_noncanonical_or_tampered_wire_values() {
        let encoded = cursor(2, UtcMicros(1_000));
        let decoded = decode_cursor(&encoded).expect("canonical cursor");
        assert_eq!(
            require_unexpired(&decoded, UtcMicros(1_000)),
            Err(PreparedQueryErrorV1::Stale)
        );

        let mut tampered = encoded.clone();
        tampered.push('0');
        assert_eq!(decode_cursor(&tampered), Err(PreparedQueryErrorV1::Invalid));

        let uppercase = format!(
            "{PREPARED_QUERY_CURSOR_PREFIX_V1}{}",
            encoded
                .trim_start_matches(PREPARED_QUERY_CURSOR_PREFIX_V1)
                .to_ascii_uppercase()
        );
        assert_eq!(
            decode_cursor(&uppercase),
            Err(PreparedQueryErrorV1::Invalid)
        );
    }

    #[test]
    fn equivalent_prepared_cursors_have_identical_bytes() {
        let first = cursor(2, UtcMicros(1_000));
        let second = cursor(2, UtcMicros(1_000));

        assert_eq!(first, second);
        assert_eq!(
            first,
            "ccq1.7b227061796c6f6164223a7b227265766973696f6e223a312c226f7065726174696f6e223a22636f64655f65786163745f6f6363757272656e6365222c2273636f70655f646967657374223a227368613235363a32653534336135303264393764333038306466363631313337386438633532633336623933633731653762663064373862313961313234353166313432363031222c2267656e65726174696f6e223a2267656e65726174696f6e2e63616c6c61626c652d70616765222c2271756572795f62696e64696e675f646967657374223a227368613235363a34633466323861306663396134663239353030323964353230373734343530366362316136633261383336626534366264643132366366366666353037333039222c2263616e6469646174655f7365745f646967657374223a227368613235363a66623464373630353138396533313032646463623665323431656531336566353530313739373333616130306266313834663732633133653565333263323065222c2261757468656e7469636174696f6e5f6b65795f6964223a22637572736f722d6b65792e63616c6c61626c652d70616765222c226e6578745f6f6666736574223a322c22657870697265735f6174223a313030307d2c2261757468656e7469636174696f6e223a7b22707269766163795f646f6d61696e223a22707269766163792e63616c6c61626c652d70616765222c226b65795f65706f6368223a312c226d6163223a22686d61632d7368613235363a37373737373737373737373737373737373737373737373737373737373737373737373737373737373737373737373737373737373737373737373737373737227d7d"
        );
        let first_bytes = hex::decode(
            first
                .strip_prefix(PREPARED_QUERY_CURSOR_PREFIX_V1)
                .expect("prepared cursor prefix"),
        )
        .expect("prepared cursor bytes");
        let decoded = decode_cursor(&first).expect("canonical cursor");
        assert_eq!(
            serde_json::to_vec(&decoded).expect("canonical cursor serialization"),
            first_bytes
        );
    }

    #[test]
    fn prepared_cursor_mac_does_not_route_through_query_sanitizer_revisions() {
        // The authentication payload is domain-separated for prepared cursors and
        // must never embed query sanitizer/normalization revision strings.
        let payload = PreparedQueryCursorPayloadV1 {
            revision: PREPARED_QUERY_CURSOR_REVISION_V1,
            operation: "code_exact_occurrence".to_owned(),
            scope_digest: digest("scope"),
            generation: CodeGenerationId::new("generation.callable-page").expect("generation"),
            query_binding_digest: digest("query"),
            candidate_set_digest: digest("candidates"),
            authentication_key_id: RetrievalCursorKeyId::new("cursor-key.callable-page")
                .expect("cursor key"),
            next_offset: 2,
            expires_at: UtcMicros(1_000),
        };
        let bytes = cursor_authentication_payload_bytes(&payload).expect("payload bytes");
        let text = String::from_utf8(bytes).expect("utf8 payload");
        assert!(text.contains(PREPARED_QUERY_CURSOR_MAC_DOMAIN_V1));
        assert!(!text.contains("query-sanitizer"));
        assert!(!text.contains("query-normalization"));
        assert!(!text.contains("EphemeralSanitizedQueryView"));
    }
}

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
    CodeGenerationId, EphemeralSanitizedQueryViewV1, ManifestDigest, QueryDigest,
    RetrievalCursorKeyId, RetrievalRequest, UtcMicros, canonical_sha256,
};

use super::fusion::QueryDigestAuthenticationError;
use super::{
    PR9_QUERY_NORMALIZATION_REVISION_V1, PR9_QUERY_SANITIZER_REVISION_V1, Pr9QueryAuthorityErrorV1,
    Pr9QueryAuthorityV1,
};

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
            let expires_at = UtcMicros(
                now.0
                    .checked_add(PREPARED_QUERY_CURSOR_TTL_MICROS_V1)
                    .ok_or(PreparedQueryErrorV1::Unavailable)?,
            );
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
            let view = cursor_authentication_view(&payload)?;
            let authentication = self
                .authority
                .authenticate_query(&self.request, &view)
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
    let view = cursor_authentication_view(&cursor.payload)?;
    authority
        .verify_authenticated_query(
            &cursor.payload.authentication_key_id,
            request,
            &view,
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

fn cursor_authentication_view(
    payload: &PreparedQueryCursorPayloadV1,
) -> Result<EphemeralSanitizedQueryViewV1, PreparedQueryErrorV1> {
    let canonical =
        serde_json::to_string(payload).map_err(|_| PreparedQueryErrorV1::Unavailable)?;
    EphemeralSanitizedQueryViewV1::sanitize(
        canonical,
        tracedecay_domain::SanitizerRevision::new(PR9_QUERY_SANITIZER_REVISION_V1)
            .map_err(|_| PreparedQueryErrorV1::Unavailable)?,
        tracedecay_domain::QueryNormalizationRevision::new(PR9_QUERY_NORMALIZATION_REVISION_V1)
            .map_err(|_| PreparedQueryErrorV1::Unavailable)?,
    )
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
}

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
    AuthorizationRevision, CodeGenerationId, FreshnessVectorDigest, ManifestDigest, PrincipalId,
    QueryDigest, RetrievalBudget, RetrievalCursorKeyId, RetrievalRequest, RetrievalScope,
    RetrievalSnapshot, SingleRootScopeV1, TemporalModeV1, UtcMicros, VectorWatermark,
    canonical_sha256,
};

use super::fusion::{PREPARED_QUERY_CURSOR_MAC_DOMAIN_V1, QueryDigestAuthenticationError};
use super::{QueryAuthorityErrorV1, QueryAuthorityV1};

const PREPARED_QUERY_CURSOR_PREFIX_V2: &str = "ccq2.";
const PREPARED_QUERY_CURSOR_REVISION_V2: u16 = 2;
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
pub struct PreparedQueryRoutingBindingsV1 {
    pub operation: String,
    pub scope_digest: ManifestDigest,
    pub principal: PrincipalId,
    pub root: SingleRootScopeV1,
    pub temporal_mode: TemporalModeV1,
    pub query_binding_digest: ManifestDigest,
    pub page_size: u32,
    pub authorization_revision: AuthorizationRevision,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedQueryPageV1<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub next_cursor: Option<String>,
    pub expires_at: Option<UtcMicros>,
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
    page_size: u32,
    expires_at: UtcMicros,
    request_binding: PreparedQueryRequestBindingV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct PreparedQueryRequestBindingV1 {
    freshness_digest: FreshnessVectorDigest,
}

impl PreparedQueryRequestBindingV1 {
    fn from_request(request: &RetrievalRequest) -> Self {
        Self {
            freshness_digest: request.snapshot.freshness_digest.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AuthenticatedPreparedQueryCursorV1 {
    payload: PreparedQueryCursorPayloadV1,
    authentication: QueryDigest,
}

pub struct PreparedQueryV1 {
    authority: Arc<QueryAuthorityV1>,
    request: RetrievalRequest,
    cursor: Option<AuthenticatedPreparedQueryCursorV1>,
}

impl PreparedQueryV1 {
    pub fn prepare(
        authority: Arc<QueryAuthorityV1>,
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
                    || cursor.payload.page_size != page_size
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
        let (next_cursor, expires_at) = if end < items.len() {
            let expires_at = match &self.cursor {
                Some(cursor) => cursor.payload.expires_at,
                None => UtcMicros(
                    now.0
                        .checked_add(PREPARED_QUERY_CURSOR_TTL_MICROS_V1)
                        .ok_or(PreparedQueryErrorV1::Unavailable)?,
                ),
            };
            let payload = PreparedQueryCursorPayloadV1 {
                revision: PREPARED_QUERY_CURSOR_REVISION_V2,
                operation: bindings.operation.clone(),
                scope_digest: bindings.scope_digest.clone(),
                generation: bindings.generation.clone(),
                query_binding_digest: bindings.query_binding_digest.clone(),
                candidate_set_digest,
                authentication_key_id: self.authority.active_query_key_id(),
                next_offset: u32::try_from(end).map_err(|_| PreparedQueryErrorV1::Unavailable)?,
                page_size: u32::try_from(page_size)
                    .map_err(|_| PreparedQueryErrorV1::Unavailable)?,
                expires_at,
                request_binding: PreparedQueryRequestBindingV1::from_request(&self.request),
            };
            let authentication = self
                .authority
                .authenticate_prepared_cursor_payload(
                    &self.request,
                    &cursor_authentication_payload_bytes(&payload)?,
                )
                .map_err(map_authority_error)?;
            (
                Some(encode_cursor(payload, authentication)?),
                Some(expires_at),
            )
        } else {
            (None, None)
        };
        Ok(PreparedQueryPageV1 {
            items: items.into_iter().skip(start).take(end - start).collect(),
            total,
            next_cursor,
            expires_at,
        })
    }
}

pub fn authenticate_prepared_query_cursor_for_routing(
    authority: &QueryAuthorityV1,
    bindings: &PreparedQueryRoutingBindingsV1,
    encoded: &str,
    now: UtcMicros,
) -> Result<PreparedQueryCursorRoutingV1, PreparedQueryErrorV1> {
    let cursor = decode_cursor(encoded)?;
    let request = routing_request(
        &cursor.payload.request_binding,
        &cursor.authentication,
        bindings,
        &authority.profile().profile_id,
        authority.profile().retrieval_budget,
    );
    authority
        .verify_prepared_cursor_payload(
            &cursor.payload.authentication_key_id,
            &request,
            &cursor_authentication_payload_bytes(&cursor.payload)?,
            &cursor.authentication,
        )
        .map_err(map_authority_error)?;
    require_unexpired(&cursor, now)?;
    if cursor.payload.operation != bindings.operation
        || cursor.payload.scope_digest != bindings.scope_digest
        || cursor.payload.query_binding_digest != bindings.query_binding_digest
        || cursor.payload.page_size != bindings.page_size
    {
        return Err(PreparedQueryErrorV1::Invalid);
    }
    Ok(PreparedQueryCursorRoutingV1 {
        generation: cursor.payload.generation,
        expires_at: cursor.payload.expires_at,
    })
}

pub fn route_authenticated_prepared_query_cursor<F, Fut>(
    authority: &QueryAuthorityV1,
    bindings: &PreparedQueryRoutingBindingsV1,
    encoded: &str,
    now: UtcMicros,
    expected_generation: Option<&CodeGenerationId>,
    effect: F,
) -> Result<Fut, PreparedQueryErrorV1>
where
    F: FnOnce(CodeGenerationId) -> Fut,
{
    let routing =
        authenticate_prepared_query_cursor_for_routing(authority, bindings, encoded, now)?;
    if expected_generation.is_some_and(|expected| expected != &routing.generation) {
        return Err(PreparedQueryErrorV1::Invalid);
    }
    Ok(effect(routing.generation))
}

fn authenticate_cursor(
    authority: &QueryAuthorityV1,
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
    if cursor.payload.request_binding != PreparedQueryRequestBindingV1::from_request(request) {
        return Err(PreparedQueryErrorV1::Invalid);
    }
    Ok(cursor)
}

fn routing_request(
    request_binding: &PreparedQueryRequestBindingV1,
    authentication: &QueryDigest,
    bindings: &PreparedQueryRoutingBindingsV1,
    profile_id: &tracedecay_domain::FusionProfileId,
    budget: RetrievalBudget,
) -> RetrievalRequest {
    RetrievalRequest {
        principal: bindings.principal.clone(),
        scope: RetrievalScope {
            privacy_domain: authentication.privacy_domain.clone(),
            root: bindings.root.clone(),
        },
        temporal_mode: bindings.temporal_mode,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: request_binding.freshness_digest.clone(),
            authorization_revision: bindings.authorization_revision.clone(),
            captured_at: UtcMicros(0),
        },
        profile_id: profile_id.clone(),
        budget,
    }
}

fn map_authority_error(error: QueryAuthorityErrorV1) -> PreparedQueryErrorV1 {
    match error {
        QueryAuthorityErrorV1::QueryAuthentication(QueryDigestAuthenticationError::KeyRevoked) => {
            PreparedQueryErrorV1::Stale
        }
        QueryAuthorityErrorV1::QueryAuthentication(
            QueryDigestAuthenticationError::KeyUnavailable,
        )
        | QueryAuthorityErrorV1::AuthorityUnavailable => PreparedQueryErrorV1::Unavailable,
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
        authentication_key_id: &'a RetrievalCursorKeyId,
        next_offset: u32,
        page_size: u32,
        expires_at: UtcMicros,
        request_binding: &'a PreparedQueryRequestBindingV1,
    }
    serde_json::to_vec(&PreparedQueryCursorAuthenticationPayloadV1 {
        domain: PREPARED_QUERY_CURSOR_MAC_DOMAIN_V1,
        revision: payload.revision,
        operation: &payload.operation,
        scope_digest: &payload.scope_digest,
        generation: &payload.generation,
        query_binding_digest: &payload.query_binding_digest,
        candidate_set_digest: &payload.candidate_set_digest,
        authentication_key_id: &payload.authentication_key_id,
        next_offset: payload.next_offset,
        page_size: payload.page_size,
        expires_at: payload.expires_at,
        request_binding: &payload.request_binding,
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
        "{PREPARED_QUERY_CURSOR_PREFIX_V2}{}",
        hex::encode(bytes)
    ))
}

fn decode_cursor(
    encoded: &str,
) -> Result<AuthenticatedPreparedQueryCursorV1, PreparedQueryErrorV1> {
    let encoded = encoded
        .strip_prefix(PREPARED_QUERY_CURSOR_PREFIX_V2)
        .ok_or(PreparedQueryErrorV1::Invalid)?;
    let bytes = hex::decode(encoded).map_err(|_| PreparedQueryErrorV1::Invalid)?;
    if hex::encode(&bytes) != encoded {
        return Err(PreparedQueryErrorV1::Invalid);
    }
    let cursor: AuthenticatedPreparedQueryCursorV1 =
        serde_json::from_slice(&bytes).map_err(|_| PreparedQueryErrorV1::Invalid)?;
    if serde_json::to_vec(&cursor).map_err(|_| PreparedQueryErrorV1::Invalid)? != bytes
        || cursor.payload.revision != PREPARED_QUERY_CURSOR_REVISION_V2
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

    fn request() -> RetrievalRequest {
        RetrievalRequest {
            principal: PrincipalId::new("principal.callable-page").expect("principal"),
            scope: tracedecay_domain::RetrievalScope {
                privacy_domain: tracedecay_domain::PrivacyDomainId::new("privacy.callable-page")
                    .expect("privacy domain"),
                root: SingleRootScopeV1 {
                    repository: tracedecay_domain::RepositoryId::new("repository.callable-page")
                        .expect("repository"),
                    worktree: Some(
                        tracedecay_domain::WorktreeId::new("worktree.callable-page")
                            .expect("worktree"),
                    ),
                    reference: None,
                },
            },
            temporal_mode: TemporalModeV1::Current,
            snapshot: tracedecay_domain::RetrievalSnapshot {
                watermarks: tracedecay_domain::VectorWatermark::default(),
                freshness_digest: tracedecay_domain::FreshnessVectorDigest::new(
                    digest("freshness.callable-page").as_str(),
                )
                .expect("freshness"),
                authorization_revision: AuthorizationRevision::new("authorization.callable-page")
                    .expect("authorization"),
                captured_at: UtcMicros(1),
            },
            profile_id: tracedecay_domain::FusionProfileId::new("profile.callable-page")
                .expect("profile"),
            budget: tracedecay_domain::RetrievalBudget {
                max_candidates_per_lane: 1,
                max_fused_candidates: 1,
                max_hydrated_results: 1,
                max_hydration_bytes: 1,
                deadline_micros: None,
            },
        }
    }

    fn cursor(next_offset: u32, expires_at: UtcMicros) -> String {
        encode_cursor(
            PreparedQueryCursorPayloadV1 {
                revision: PREPARED_QUERY_CURSOR_REVISION_V2,
                operation: "code_exact_occurrence".to_owned(),
                scope_digest: digest("scope"),
                generation: CodeGenerationId::new("generation.callable-page").expect("generation"),
                query_binding_digest: digest("query"),
                candidate_set_digest: digest("candidates"),
                authentication_key_id: RetrievalCursorKeyId::new("cursor-key.callable-page")
                    .expect("cursor key"),
                next_offset,
                page_size: 1,
                expires_at,
                request_binding: PreparedQueryRequestBindingV1::from_request(&request()),
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
    fn raw_cursor_decode_is_explicitly_untrusted() {
        let encoded = cursor(2, UtcMicros(1_000));
        let routing = decode_cursor(&encoded).expect("canonical cursor");
        assert_eq!(
            routing.payload.generation,
            CodeGenerationId::new("generation.callable-page").expect("generation")
        );
        assert_eq!(routing.payload.expires_at, UtcMicros(1_000));
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
            "{PREPARED_QUERY_CURSOR_PREFIX_V2}{}",
            encoded
                .trim_start_matches(PREPARED_QUERY_CURSOR_PREFIX_V2)
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
                .strip_prefix(PREPARED_QUERY_CURSOR_PREFIX_V2)
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
            revision: PREPARED_QUERY_CURSOR_REVISION_V2,
            operation: "code_exact_occurrence".to_owned(),
            scope_digest: digest("scope"),
            generation: CodeGenerationId::new("generation.callable-page").expect("generation"),
            query_binding_digest: digest("query"),
            candidate_set_digest: digest("candidates"),
            authentication_key_id: RetrievalCursorKeyId::new("cursor-key.callable-page")
                .expect("cursor key"),
            next_offset: 2,
            page_size: 1,
            expires_at: UtcMicros(1_000),
            request_binding: PreparedQueryRequestBindingV1::from_request(&request()),
        };
        let bytes = cursor_authentication_payload_bytes(&payload).expect("payload bytes");
        let text = String::from_utf8(bytes).expect("utf8 payload");
        assert!(text.contains(PREPARED_QUERY_CURSOR_MAC_DOMAIN_V1));
        assert!(!text.contains("query-sanitizer"));
        assert!(!text.contains("query-normalization"));
        assert!(!text.contains("EphemeralSanitizedQueryView"));
    }

    #[test]
    fn compact_request_binding_stays_within_application_cursor_envelope() {
        let encoded = cursor(2, UtcMicros(1_000));
        let mut decoded = decode_cursor(&encoded).expect("canonical cursor");
        decoded.authentication.privacy_domain = tracedecay_domain::PrivacyDomainId::new(format!(
            "privacy.{}",
            "p".repeat(512 - "privacy.".len())
        ))
        .expect("maximum-sized privacy identity");
        decoded.payload.generation = CodeGenerationId::new(format!(
            "generation.{}",
            "g".repeat(512 - "generation.".len())
        ))
        .expect("maximum-sized generation identity");
        decoded.payload.authentication_key_id = RetrievalCursorKeyId::new(format!(
            "cursor-key.{}",
            "k".repeat(512 - "cursor-key.".len())
        ))
        .expect("maximum-sized cursor-key identity");

        let encoded =
            encode_cursor(decoded.payload, decoded.authentication).expect("bounded cursor");
        assert!(
            encoded.len() <= 5_120,
            "prepared cursor must fit the application opaque-cursor envelope: {} bytes",
            encoded.len()
        );
    }
}

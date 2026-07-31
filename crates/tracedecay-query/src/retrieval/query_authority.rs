//! Authenticated production authority for the canonical query fallback.
//!
//! Construction requires an already accepted immutable evaluation profile and
//! a daemon-owned query/cursor keyring. This module does not choose weights,
//! mint calibration identities, or generate key material.

use std::collections::BTreeSet;
use std::sync::Arc;

use thiserror::Error;
use tracedecay_domain::{
    ComponentRevision, DiversityPolicy, EphemeralSanitizedQueryViewV1, FusionProfile, QueryDigest,
    QueryFallbackSubpayload, RetrievalContractError, RetrievalCursor, RetrievalCursorKeyId,
    RetrievalError, RetrievalRequest, RetrieverKind,
};

use super::fusion::{
    CompositionKernel, CompositionLaneInput, CompositionOutputV1, FusionStageError,
    FusionStageInput, QueryDigestAuthenticationError, RetrievalCursorKeyringV1,
};

/// Immutable comparator/ranking revision shared by the query evaluator,
/// production authority, and cursor validation.
pub const QUERY_RANKING_REVISION_V1: &str = "ranking.candidate.v1";
/// Versioned request-local cursor lifetime for the canonical query authority.
pub const QUERY_CURSOR_TTL_MICROS_V1: u64 = 15 * 60 * 1_000_000;

/// Complete authenticated query composition retained for semantic augmentation
/// and server-side audit. The fallback payload is independently canonical and
/// cannot be changed by later optional lanes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedQueryFallbackV1 {
    pub query_digest: QueryDigest,
    pub fallback: Arc<QueryFallbackSubpayload>,
    pub composition: CompositionOutputV1,
    /// Exact compact lane inputs retained for one optional-stage recomposition.
    /// Already-ranked fallback candidates must never be treated as a lane.
    pub fallback_lanes: Vec<CompositionLaneInput>,
    pub page_size: usize,
    /// Authenticated client continuation supplied for this page. It remains
    /// outside the canonical fallback subpayload.
    pub request_cursor: Option<RetrievalCursor>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum QueryAuthorityErrorV1 {
    #[error("query authority is unavailable for the admitted scope")]
    AuthorityUnavailable,
    #[error("query authority rejected its immutable profile: {0}")]
    InvalidAuthority(String),
    #[error("query does not match the accepted profile or budget")]
    RequestProfileMismatch,
    #[error("query composition requires exact, lexical, and graph exactly once")]
    LaneSetMismatch,
    #[error(transparent)]
    QueryAuthentication(#[from] QueryDigestAuthenticationError),
    #[error(transparent)]
    Composition(#[from] FusionStageError),
    #[error(transparent)]
    Retrieval(#[from] RetrievalError),
    #[error(transparent)]
    Contract(#[from] RetrievalContractError),
}

/// One production query profile/key authority.
///
/// The configuration owner may mount this only after the profile's evaluation
/// anchor has been accepted. The durable provider owns key lifecycle; only an
/// authenticated [`QueryDigest`] and signed cursor leave this owner.
pub struct QueryAuthorityV1 {
    profile: FusionProfile,
    diversity: DiversityPolicy,
    kernel: CompositionKernel,
    keyring: RetrievalCursorKeyringV1,
}

impl QueryAuthorityV1 {
    pub fn new(
        profile: FusionProfile,
        diversity: DiversityPolicy,
        ranking_revision: ComponentRevision,
        keyring: RetrievalCursorKeyringV1,
    ) -> Result<Self, QueryAuthorityErrorV1> {
        profile.retrieval_budget.validate()?;
        let expected_lanes = BTreeSet::from(RetrieverKind::QUERY_FALLBACK_LANES);
        let calibration_lanes = profile
            .calibrations
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        let weight_lanes = profile
            .weights_micros
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        if calibration_lanes != expected_lanes
            || weight_lanes != expected_lanes
            || profile.rerank_policy_id.is_some()
        {
            return Err(QueryAuthorityErrorV1::InvalidAuthority(
                "profile is not an exact query fallback profile".to_owned(),
            ));
        }
        if diversity.policy_id != profile.diversity_policy_id
            || diversity.evaluation_result_anchor.as_ref()
                != Some(&profile.evaluation_result_anchor)
        {
            return Err(QueryAuthorityErrorV1::InvalidAuthority(
                "diversity policy is not bound to the accepted evaluation".to_owned(),
            ));
        }
        Ok(Self {
            profile,
            diversity,
            kernel: CompositionKernel::new(ranking_revision),
            keyring,
        })
    }

    pub fn profile(&self) -> &FusionProfile {
        &self.profile
    }

    /// Authenticate one ephemeral sanitized query with the daemon-owned key.
    pub fn authenticate_query(
        &self,
        request: &RetrievalRequest,
        query_view: &EphemeralSanitizedQueryViewV1,
    ) -> Result<QueryDigest, QueryAuthorityErrorV1> {
        self.validate_request(request)?;
        Ok(self.keyring.digest_active_query(request, query_view)?)
    }

    /// Authenticate one prepared-query cursor payload with the daemon-owned key.
    pub fn authenticate_prepared_cursor_payload(
        &self,
        request: &RetrievalRequest,
        payload_bytes: &[u8],
    ) -> Result<QueryDigest, QueryAuthorityErrorV1> {
        self.validate_request(request)?;
        Ok(self
            .keyring
            .digest_active_prepared_cursor_payload(request, payload_bytes)?)
    }

    pub fn active_query_key_id(&self) -> RetrievalCursorKeyId {
        self.keyring.active_query_key_id()
    }

    pub fn verify_authenticated_query(
        &self,
        key_id: &RetrievalCursorKeyId,
        request: &RetrievalRequest,
        query_view: &EphemeralSanitizedQueryViewV1,
        digest: &QueryDigest,
    ) -> Result<(), QueryAuthorityErrorV1> {
        self.validate_request(request)?;
        self.keyring
            .verify_query_digest_for(key_id, request, query_view, digest)?;
        Ok(())
    }

    pub fn verify_prepared_cursor_payload(
        &self,
        key_id: &RetrievalCursorKeyId,
        request: &RetrievalRequest,
        payload_bytes: &[u8],
        digest: &QueryDigest,
    ) -> Result<(), QueryAuthorityErrorV1> {
        self.validate_request(request)?;
        self.keyring
            .verify_prepared_cursor_payload_for(key_id, request, payload_bytes, digest)?;
        Ok(())
    }

    /// Compose and page the exact query lanes under the accepted immutable
    /// profile, returning the authenticated query identity and canonical
    /// fallback subpayload together.
    pub fn compose(
        &self,
        request: &RetrievalRequest,
        query_view: &EphemeralSanitizedQueryViewV1,
        lanes: Vec<CompositionLaneInput>,
        page_size: usize,
        cursor: Option<&RetrievalCursor>,
    ) -> Result<AuthorizedQueryFallbackV1, QueryAuthorityErrorV1> {
        self.validate_request(request)?;
        validate_lane_set(&lanes)?;
        let query_digest = self.keyring.digest_active_query(request, query_view)?;
        let fallback_lanes = lanes.clone();
        let composition = self.kernel.compose(
            &FusionStageInput {
                profile: self.profile.clone(),
                lanes,
            },
            &self.diversity,
        )?;
        let page = self.kernel.paginate_at(
            request,
            query_view,
            &self.keyring,
            &composition,
            page_size,
            cursor,
            request.snapshot.captured_at,
        )?;
        let fallback = QueryFallbackSubpayload::new(
            composition.profile_id.clone(),
            page.ranked_candidates,
            composition
                .public_lane_statuses
                .iter()
                .filter(|(lane, _)| lane.is_query_fallback_lane())
                .map(|(lane, status)| (*lane, *status))
                .collect(),
            composition.freshness.clone(),
            page.cursor,
        )?;
        Ok(AuthorizedQueryFallbackV1 {
            query_digest,
            fallback: Arc::new(fallback),
            composition,
            fallback_lanes,
            page_size,
            request_cursor: cursor.cloned(),
        })
    }

    pub fn continuation_cursor_at(
        &self,
        request: &RetrievalRequest,
        query_view: &EphemeralSanitizedQueryViewV1,
        composition: &CompositionOutputV1,
        next_ordinal: usize,
    ) -> Result<RetrievalCursor, QueryAuthorityErrorV1> {
        self.validate_request(request)?;
        Ok(self.kernel.cursor_at(
            request,
            query_view,
            &self.keyring,
            composition,
            next_ordinal,
            request.snapshot.captured_at,
        )?)
    }

    pub fn bind_semantic_continuation(
        &self,
        cursor: &mut RetrievalCursor,
        semantic: tracedecay_domain::SemanticRetrievalContinuationV1,
    ) -> Result<(), QueryAuthorityErrorV1> {
        cursor.semantic = Some(semantic);
        self.keyring.resign_cursor(cursor)?;
        Ok(())
    }

    fn validate_request(&self, request: &RetrievalRequest) -> Result<(), QueryAuthorityErrorV1> {
        request.budget.validate()?;
        if request.profile_id != self.profile.profile_id
            || request.budget != self.profile.retrieval_budget
        {
            return Err(QueryAuthorityErrorV1::RequestProfileMismatch);
        }
        Ok(())
    }
}

fn validate_lane_set(lanes: &[CompositionLaneInput]) -> Result<(), QueryAuthorityErrorV1> {
    let actual = lanes.iter().map(|lane| lane.lane).collect::<BTreeSet<_>>();
    let expected = BTreeSet::from(RetrieverKind::QUERY_FALLBACK_LANES);
    if lanes.len() != expected.len() || actual != expected {
        return Err(QueryAuthorityErrorV1::LaneSetMismatch);
    }
    Ok(())
}

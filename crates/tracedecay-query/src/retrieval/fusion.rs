//! Deterministic fixed-point fusion stage contracts (Plan 15 pipeline steps
//! 4-8; Plan 25: `src/query/retrieval/fusion.rs` operates on compact
//! candidates with deterministic fixed-point contributions, complete
//! comparator provenance, and source/file caps).
//!
//! RRF may be evaluated as a profile candidate inside this generic
//! fixed-point framework; no constant or weight is production authority
//! before Plan 15 accepts it.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{
    CandidateContribution, CandidateSetDigest, CompactCandidate, ComponentRevision,
    EphemeralSanitizedQueryViewV1, ExactClass, FixedPointScore, FusedCandidate, FusionProfile,
    LogicalEvidenceId, OccurrenceProvenance, PrivacyDomainId, PublicRetrieverStatus, QueryDigest,
    QueryMac, QueryNormalizationRevision, RankedCandidate, RankingDecision, RankingDecisionKind,
    RetrievalAnchorId, RetrievalContractError, RetrievalCursor, RetrievalCursorKeyId,
    RetrievalError, RetrievalRequest, RetrieverBatch, RetrieverContinuation, RetrieverKind,
    RetrieverOutcome, SanitizerRevision, SourceFreshness, SourceOccurrenceId, UtcMicros,
};
use zeroize::Zeroizing;

use super::dedupe::{DedupeDecisionV1, DeterministicDedupe};
use super::diversity::{DeterministicDiversity, DiversityDecisionV1, DiversityStageError};
use super::ordering::{
    compare_fused, exact_class_rank, ordered_occurrence_ids, source_validity_rank,
};

const QUERY_DIGEST_MAC_DOMAIN: &str = "tracedecay.retrieval-query-mac.v1";
const RETRIEVAL_CURSOR_MAC_DOMAIN: &str = "tracedecay.retrieval-cursor-mac.v1";
/// Domain separator for authenticated prepared-query continuation cursors.
/// Distinct from query-view MAC so cursor bytes never route through query
/// sanitizer revisions.
pub const PREPARED_QUERY_CURSOR_MAC_DOMAIN_V1: &str = "tracedecay.prepared-query-cursor-mac.v1";
const MIN_QUERY_MAC_SECRET_BYTES: usize = 32;
const MAX_QUERY_MAC_SECRET_BYTES: usize = 256;

/// Failure to authenticate a request-local query view for cursor identity.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum QueryDigestAuthenticationError {
    #[error("query MAC key material is invalid")]
    InvalidKeyMaterial,
    #[error("query MAC authentication failed")]
    AuthenticationFailed,
    #[error("query MAC key is unavailable")]
    KeyUnavailable,
    #[error("query MAC key was revoked")]
    KeyRevoked,
    #[error("query MAC privacy domain does not match the authorized request scope")]
    PrivacyDomainMismatch,
    #[error("query MAC canonicalization failed: {0}")]
    Canonicalization(String),
    #[error(transparent)]
    Contract(#[from] RetrievalContractError),
}

struct RetrievalCursorKeyMaterialV1 {
    secret: Zeroizing<Vec<u8>>,
    revoked: bool,
}

/// Request-local cursor key policy. New cursors use the active key; retained
/// non-revoked keys verify old cursors after rotation.
pub struct RetrievalCursorKeyringV1 {
    privacy_domain: PrivacyDomainId,
    active: (RetrievalCursorKeyId, u64),
    keys: BTreeMap<(RetrievalCursorKeyId, u64), RetrievalCursorKeyMaterialV1>,
    cursor_ttl_micros: u64,
}

impl fmt::Debug for RetrievalCursorKeyringV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetrievalCursorKeyringV1")
            .field("privacy_domain", &self.privacy_domain)
            .field("active", &self.active)
            .field("retained_key_count", &self.keys.len())
            .field("cursor_ttl_micros", &self.cursor_ttl_micros)
            .field("key_material", &"REDACTED")
            .finish()
    }
}

#[derive(Serialize)]
struct QueryMacInput<'a> {
    domain: &'static str,
    privacy_domain: &'a PrivacyDomainId,
    key_epoch: u64,
    query_bytes: &'a [u8],
    sanitizer_revision: &'a SanitizerRevision,
    normalization_revision: &'a QueryNormalizationRevision,
}

impl RetrievalCursorKeyringV1 {
    pub fn new(
        privacy_domain: PrivacyDomainId,
        key_id: RetrievalCursorKeyId,
        key_epoch: u64,
        secret: impl Into<Vec<u8>>,
        cursor_ttl_micros: u64,
    ) -> Result<Self, QueryDigestAuthenticationError> {
        if cursor_ttl_micros == 0 {
            return Err(QueryDigestAuthenticationError::InvalidKeyMaterial);
        }
        let mut keyring = Self {
            privacy_domain,
            active: (key_id.clone(), key_epoch),
            keys: BTreeMap::new(),
            cursor_ttl_micros,
        };
        keyring.retain(key_id, key_epoch, secret)?;
        Ok(keyring)
    }

    pub fn retain(
        &mut self,
        key_id: RetrievalCursorKeyId,
        key_epoch: u64,
        secret: impl Into<Vec<u8>>,
    ) -> Result<(), QueryDigestAuthenticationError> {
        let secret = Zeroizing::new(secret.into());
        if !(MIN_QUERY_MAC_SECRET_BYTES..=MAX_QUERY_MAC_SECRET_BYTES).contains(&secret.len())
            || self.keys.contains_key(&(key_id.clone(), key_epoch))
        {
            return Err(QueryDigestAuthenticationError::InvalidKeyMaterial);
        }
        self.keys.insert(
            (key_id, key_epoch),
            RetrievalCursorKeyMaterialV1 {
                secret,
                revoked: false,
            },
        );
        Ok(())
    }

    pub fn privacy_domain(&self) -> &PrivacyDomainId {
        &self.privacy_domain
    }

    pub fn rotate(
        &mut self,
        key_id: RetrievalCursorKeyId,
        key_epoch: u64,
        secret: impl Into<Vec<u8>>,
    ) -> Result<(), QueryDigestAuthenticationError> {
        self.retain(key_id.clone(), key_epoch, secret)?;
        self.active = (key_id, key_epoch);
        Ok(())
    }

    pub fn revoke(
        &mut self,
        key_id: &RetrievalCursorKeyId,
        key_epoch: u64,
    ) -> Result<(), QueryDigestAuthenticationError> {
        let key = self
            .keys
            .get_mut(&(key_id.clone(), key_epoch))
            .ok_or(QueryDigestAuthenticationError::KeyUnavailable)?;
        key.revoked = true;
        Ok(())
    }

    pub fn digest_active_query(
        &self,
        request: &RetrievalRequest,
        query_view: &EphemeralSanitizedQueryViewV1,
    ) -> Result<QueryDigest, QueryDigestAuthenticationError> {
        self.digest_query_for(&self.active.0, self.active.1, request, query_view)
    }

    /// Authenticate one prepared-query cursor payload with the active key.
    ///
    /// `payload_bytes` must already be the canonical authenticated cursor
    /// payload (domain-separated outside the query sanitizer path).
    pub fn digest_active_prepared_cursor_payload(
        &self,
        request: &RetrievalRequest,
        payload_bytes: &[u8],
    ) -> Result<QueryDigest, QueryDigestAuthenticationError> {
        self.digest_prepared_cursor_payload_for(
            &self.active.0,
            self.active.1,
            request,
            payload_bytes,
        )
    }

    pub(crate) fn active_query_key_id(&self) -> RetrievalCursorKeyId {
        self.active.0.clone()
    }

    pub(crate) fn verify_query_digest_for(
        &self,
        key_id: &RetrievalCursorKeyId,
        request: &RetrievalRequest,
        query_view: &EphemeralSanitizedQueryViewV1,
        digest: &QueryDigest,
    ) -> Result<(), QueryDigestAuthenticationError> {
        if request.scope.privacy_domain != self.privacy_domain
            || digest.privacy_domain != self.privacy_domain
        {
            return Err(QueryDigestAuthenticationError::PrivacyDomainMismatch);
        }
        let material = self.key_material(key_id, digest.key_epoch)?;
        let bytes = self.query_mac_input_bytes(digest.key_epoch, query_view)?;
        let signature = query_mac_bytes(&digest.mac)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&material.secret)
            .map_err(|_| QueryDigestAuthenticationError::InvalidKeyMaterial)?;
        mac.update(&bytes);
        mac.verify_slice(&signature)
            .map_err(|_| QueryDigestAuthenticationError::AuthenticationFailed)
    }

    pub(crate) fn verify_prepared_cursor_payload_for(
        &self,
        key_id: &RetrievalCursorKeyId,
        request: &RetrievalRequest,
        payload_bytes: &[u8],
        digest: &QueryDigest,
    ) -> Result<(), QueryDigestAuthenticationError> {
        if request.scope.privacy_domain != self.privacy_domain
            || digest.privacy_domain != self.privacy_domain
        {
            return Err(QueryDigestAuthenticationError::PrivacyDomainMismatch);
        }
        let material = self.key_material(key_id, digest.key_epoch)?;
        let bytes =
            self.prepared_cursor_mac_input_bytes(key_id, digest.key_epoch, request, payload_bytes)?;
        let signature = query_mac_bytes(&digest.mac)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&material.secret)
            .map_err(|_| QueryDigestAuthenticationError::InvalidKeyMaterial)?;
        mac.update(&bytes);
        mac.verify_slice(&signature)
            .map_err(|_| QueryDigestAuthenticationError::AuthenticationFailed)
    }

    fn digest_query_for(
        &self,
        key_id: &RetrievalCursorKeyId,
        key_epoch: u64,
        request: &RetrievalRequest,
        query_view: &EphemeralSanitizedQueryViewV1,
    ) -> Result<QueryDigest, QueryDigestAuthenticationError> {
        if request.scope.privacy_domain != self.privacy_domain {
            return Err(QueryDigestAuthenticationError::PrivacyDomainMismatch);
        }
        let material = self.key_material(key_id, key_epoch)?;
        let bytes = self.query_mac_input_bytes(key_epoch, query_view)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&material.secret)
            .map_err(|_| QueryDigestAuthenticationError::InvalidKeyMaterial)?;
        mac.update(&bytes);
        let mac = QueryMac::new(format!(
            "hmac-sha256:{}",
            hex::encode(mac.finalize().into_bytes())
        ))?;
        Ok(QueryDigest::new(
            self.privacy_domain.clone(),
            key_epoch,
            mac,
        ))
    }

    fn digest_prepared_cursor_payload_for(
        &self,
        key_id: &RetrievalCursorKeyId,
        key_epoch: u64,
        request: &RetrievalRequest,
        payload_bytes: &[u8],
    ) -> Result<QueryDigest, QueryDigestAuthenticationError> {
        if request.scope.privacy_domain != self.privacy_domain {
            return Err(QueryDigestAuthenticationError::PrivacyDomainMismatch);
        }
        let material = self.key_material(key_id, key_epoch)?;
        let bytes =
            self.prepared_cursor_mac_input_bytes(key_id, key_epoch, request, payload_bytes)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&material.secret)
            .map_err(|_| QueryDigestAuthenticationError::InvalidKeyMaterial)?;
        mac.update(&bytes);
        let mac = QueryMac::new(format!(
            "hmac-sha256:{}",
            hex::encode(mac.finalize().into_bytes())
        ))?;
        Ok(QueryDigest::new(
            self.privacy_domain.clone(),
            key_epoch,
            mac,
        ))
    }

    fn query_mac_input_bytes(
        &self,
        key_epoch: u64,
        query_view: &EphemeralSanitizedQueryViewV1,
    ) -> Result<Vec<u8>, QueryDigestAuthenticationError> {
        let input = QueryMacInput {
            domain: QUERY_DIGEST_MAC_DOMAIN,
            privacy_domain: &self.privacy_domain,
            key_epoch,
            query_bytes: query_view.as_bytes(),
            sanitizer_revision: query_view.sanitizer_revision(),
            normalization_revision: query_view.normalization_revision(),
        };
        serde_json::to_vec(&input)
            .map_err(|error| QueryDigestAuthenticationError::Canonicalization(error.to_string()))
    }

    fn prepared_cursor_mac_input_bytes(
        &self,
        key_id: &RetrievalCursorKeyId,
        key_epoch: u64,
        request: &RetrievalRequest,
        payload_bytes: &[u8],
    ) -> Result<Vec<u8>, QueryDigestAuthenticationError> {
        #[derive(Serialize)]
        struct PreparedCursorMacInput<'a> {
            domain: &'static str,
            privacy_domain: &'a PrivacyDomainId,
            authentication_key_id: &'a RetrievalCursorKeyId,
            key_epoch: u64,
            principal: &'a tracedecay_domain::PrincipalId,
            scope: &'a tracedecay_domain::RetrievalScope,
            temporal_mode: tracedecay_domain::TemporalModeV1,
            profile_id: &'a tracedecay_domain::FusionProfileId,
            snapshot_freshness_digest: &'a tracedecay_domain::FreshnessVectorDigest,
            authorization_revision: &'a tracedecay_domain::AuthorizationRevision,
            cursor_payload: &'a [u8],
        }
        let input = PreparedCursorMacInput {
            domain: PREPARED_QUERY_CURSOR_MAC_DOMAIN_V1,
            privacy_domain: &self.privacy_domain,
            authentication_key_id: key_id,
            key_epoch,
            principal: &request.principal,
            scope: &request.scope,
            temporal_mode: request.temporal_mode,
            profile_id: &request.profile_id,
            snapshot_freshness_digest: &request.snapshot.freshness_digest,
            authorization_revision: &request.snapshot.authorization_revision,
            cursor_payload: payload_bytes,
        };
        serde_json::to_vec(&input)
            .map_err(|error| QueryDigestAuthenticationError::Canonicalization(error.to_string()))
    }

    fn active_key(&self) -> (&RetrievalCursorKeyId, u64) {
        (&self.active.0, self.active.1)
    }

    fn expiry_from(&self, now: UtcMicros) -> Result<UtcMicros, RetrievalError> {
        let ttl = i64::try_from(self.cursor_ttl_micros)
            .map_err(|_| RetrievalError::InvalidRequest("cursor TTL overflowed".to_owned()))?;
        now.0
            .checked_add(ttl)
            .map(UtcMicros)
            .ok_or_else(|| RetrievalError::InvalidRequest("cursor expiry overflowed".to_owned()))
    }

    fn sign_cursor(&self, cursor: &RetrievalCursor) -> Result<QueryMac, RetrievalError> {
        let material = self
            .key_material(&cursor.key_id, cursor.key_epoch)
            .map_err(map_cursor_key_error)?;
        let bytes = cursor_authenticated_bytes(cursor)?;
        keyed_mac(&material.secret, &bytes).map_err(map_cursor_key_error)
    }

    fn verify_cursor(&self, cursor: &RetrievalCursor) -> Result<(), RetrievalError> {
        let material = self
            .key_material(&cursor.key_id, cursor.key_epoch)
            .map_err(map_cursor_key_error)?;
        let bytes = cursor_authenticated_bytes(cursor)?;
        let signature = query_mac_bytes(&cursor.signature).map_err(map_cursor_key_error)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&material.secret)
            .map_err(|_| RetrievalError::CursorAuthenticationFailed)?;
        mac.update(&bytes);
        mac.verify_slice(&signature)
            .map_err(|_| RetrievalError::CursorAuthenticationFailed)
    }

    pub(crate) fn resign_cursor(&self, cursor: &mut RetrievalCursor) -> Result<(), RetrievalError> {
        cursor.signature = self.sign_cursor(cursor)?;
        Ok(())
    }

    fn key_material(
        &self,
        key_id: &RetrievalCursorKeyId,
        key_epoch: u64,
    ) -> Result<&RetrievalCursorKeyMaterialV1, QueryDigestAuthenticationError> {
        let material = self
            .keys
            .get(&(key_id.clone(), key_epoch))
            .ok_or(QueryDigestAuthenticationError::KeyUnavailable)?;
        if material.revoked {
            return Err(QueryDigestAuthenticationError::KeyRevoked);
        }
        Ok(material)
    }
}

/// Failures of the fusion stage. Fusion never substitutes or simulates a
/// missing lane; it composes the typed outcomes it is given.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum FusionStageError {
    #[error("a required exact or lexical lane outcome is unavailable")]
    RequiredLaneUnavailable,
    #[error("candidate evidence is missing for a returned occurrence")]
    MissingOccurrenceEvidence,
    #[error("fixed-point arithmetic overflowed")]
    FixedPointOverflow,
    #[error("profile references a retriever outside the admitted lane set")]
    ProfileLaneMismatch,
    #[error("a retriever lane was supplied more than once")]
    DuplicateLane,
    #[error("a candidate score cannot be represented as a calibrated micros feature")]
    InvalidCalibratedFeature,
    #[error("contract violation: {0}")]
    Contract(String),
}

impl From<RetrievalContractError> for FusionStageError {
    fn from(error: RetrievalContractError) -> Self {
        match error {
            RetrievalContractError::FixedPointOverflow { .. } => Self::FixedPointOverflow,
            RetrievalContractError::MissingOccurrenceEvidence { .. }
            | RetrievalContractError::UnexpectedOccurrenceEvidence { .. } => {
                Self::MissingOccurrenceEvidence
            }
            other => Self::Contract(other.to_string()),
        }
    }
}

/// One independently typed lane admitted to compact composition.
///
/// The lane validates its typed evidence before this boundary. Composition
/// retains the one-to-one occurrence evidence keys, but does not copy or
/// interpret the evidence values owned by the lane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompositionLaneInput {
    pub lane: RetrieverKind,
    pub outcome: RetrieverOutcome<RetrieverBatch<()>>,
}

impl CompositionLaneInput {
    pub fn new<E>(
        lane: RetrieverKind,
        outcome: RetrieverOutcome<RetrieverBatch<E>>,
    ) -> Result<Self, FusionStageError> {
        let outcome = match outcome {
            RetrieverOutcome::Complete(batch) => RetrieverOutcome::Complete(compact_batch(batch)?),
            RetrieverOutcome::Partial { value, reason } => RetrieverOutcome::Partial {
                value: compact_batch(value)?,
                reason,
            },
            RetrieverOutcome::Unavailable(reason) => RetrieverOutcome::Unavailable(reason),
            RetrieverOutcome::Denied => RetrieverOutcome::Denied,
            RetrieverOutcome::Stale(freshness) => RetrieverOutcome::Stale(freshness),
            RetrieverOutcome::BudgetExceeded(usage) => RetrieverOutcome::BudgetExceeded(usage),
            RetrieverOutcome::Cancelled => RetrieverOutcome::Cancelled,
        };
        Ok(Self { lane, outcome })
    }
}

fn compact_batch<E>(batch: RetrieverBatch<E>) -> Result<RetrieverBatch<()>, FusionStageError> {
    batch.validate()?;
    Ok(RetrieverBatch {
        candidates: batch.candidates,
        evidence_by_occurrence: batch
            .evidence_by_occurrence
            .into_keys()
            .map(|occurrence| (occurrence, ()))
            .collect(),
        coverage: batch.coverage,
        continuation: batch.continuation,
    })
}

/// One fusion input: independently typed lane batches admitted for one
/// pinned snapshot (Plan 15 pipeline step 3: each lane contributes its entire
/// committed prefix or none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FusionStageInput {
    pub profile: FusionProfile,
    pub lanes: Vec<CompositionLaneInput>,
}

/// The deterministic fusion stage contract (Plan 15: group contributions by
/// stable anchor plus logical evidence identity; total order is exact class,
/// utility, source validity, stable anchor ID, logical evidence ID, then
/// ordered source occurrence IDs).
pub trait DeterministicFusionStage {
    /// Partition candidates into exact tiers and fuse approximate
    /// contributions with checked fixed-point arithmetic. Exact admission
    /// derives only from validated proofs.
    fn fuse(&self, input: &FusionStageInput) -> Result<Vec<FusedCandidate>, FusionStageError>;

    /// Compute the final deterministic order over fused candidates. One
    /// hundred shuffled producer/completion runs must produce byte-identical
    /// IDs, order, contributions, explanations, coverage, and cursors
    /// (Plan 25 acceptance).
    fn order(&self, candidates: Vec<FusedCandidate>) -> Vec<RankedCandidate>;
}

/// Complete comparator tuple retained for each final candidate. It records
/// every field in the total order instead of reconstructing ordering from the
/// final scalar utility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FusionComparatorRecordV1 {
    pub exact_class: ExactClass,
    pub utility_micros: u64,
    pub source_validity_rank: u8,
    pub anchor_id: RetrievalAnchorId,
    pub logical_evidence_id: LogicalEvidenceId,
    pub source_occurrence_ids: Vec<SourceOccurrenceId>,
    pub comparator_revision: ComponentRevision,
}

/// Result of compact-candidate composition before page hydration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompositionOutputV1 {
    pub profile_id: tracedecay_domain::FusionProfileId,
    pub ranked_candidates: Vec<RankedCandidate>,
    pub comparator_records: Vec<FusionComparatorRecordV1>,
    pub internal_lane_outcomes: BTreeMap<RetrieverKind, RetrieverOutcome<()>>,
    pub public_lane_statuses: BTreeMap<RetrieverKind, PublicRetrieverStatus>,
    pub freshness: Vec<SourceFreshness>,
    pub lane_checkpoints: Vec<RetrieverContinuation>,
    pub dedupe_decisions: Vec<DedupeDecisionV1>,
    pub diversity_decisions: Vec<DiversityDecisionV1>,
}

/// One immutable page from the saved compact candidate list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompositionPageV1 {
    pub ranked_candidates: Vec<RankedCandidate>,
    pub cursor: Option<RetrievalCursor>,
}

/// Generic query composition kernel. Evidence values are validated by
/// `RetrieverBatch<E>` but never interpreted, copied, or hydrated here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompositionKernel {
    ranking_revision: ComponentRevision,
    fusion: DeterministicFixedPointFusion,
    dedupe: DeterministicDedupe,
    diversity: DeterministicDiversity,
}

impl CompositionKernel {
    pub fn new(ranking_revision: ComponentRevision) -> Self {
        Self {
            fusion: DeterministicFixedPointFusion::new(ranking_revision.clone()),
            ranking_revision,
            dedupe: DeterministicDedupe,
            diversity: DeterministicDiversity,
        }
    }

    pub fn compose(
        &self,
        input: &FusionStageInput,
        policy: &tracedecay_domain::DiversityPolicy,
    ) -> Result<CompositionOutputV1, FusionStageError> {
        let admitted = admitted_lanes(input)?;
        let (compact, dedupe_decisions) = self
            .dedupe
            .collapse_compact_candidates(admitted.candidates)
            .map_err(|error| FusionStageError::Contract(error.to_string()))?;
        let mut fused = self.fusion.fuse_compact(&input.profile, compact)?;
        attach_same_source_decisions(&mut fused, &dedupe_decisions)?;
        let ordered = self.fusion.order_fused(fused);
        let (deduped, mut copy_decisions) = self
            .dedupe
            .select_representatives_with_decisions(ordered)
            .map_err(|error| FusionStageError::Contract(error.to_string()))?;
        let ordered = self.fusion.order_fused(deduped);
        let (ranked_candidates, diversity_decisions) = self
            .diversity
            .apply_caps(policy, ordered)
            .map_err(map_diversity_error)?;
        let comparator_records = ranked_candidates
            .iter()
            .map(|ranked| self.fusion.comparator_record(&ranked.candidate))
            .collect();

        let mut all_dedupe_decisions = dedupe_decisions;
        all_dedupe_decisions.append(&mut copy_decisions);
        Ok(CompositionOutputV1 {
            profile_id: input.profile.profile_id.clone(),
            ranked_candidates,
            comparator_records,
            internal_lane_outcomes: admitted.internal_lane_outcomes,
            public_lane_statuses: admitted.public_lane_statuses,
            freshness: admitted.freshness,
            lane_checkpoints: admitted.lane_checkpoints,
            dedupe_decisions: all_dedupe_decisions,
            diversity_decisions,
        })
    }

    /// Freeze and page the already composed candidate set. Resume validates
    /// every public binding and never recomputes against a differently
    /// completed lane set.
    pub fn paginate(
        &self,
        request: &RetrievalRequest,
        query_view: &EphemeralSanitizedQueryViewV1,
        keyring: &RetrievalCursorKeyringV1,
        output: &CompositionOutputV1,
        page_size: usize,
        cursor: Option<&RetrievalCursor>,
    ) -> Result<CompositionPageV1, RetrievalError> {
        self.paginate_at(
            request,
            query_view,
            keyring,
            output,
            page_size,
            cursor,
            current_utc_micros()?,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paginate_at(
        &self,
        request: &RetrievalRequest,
        query_view: &EphemeralSanitizedQueryViewV1,
        keyring: &RetrievalCursorKeyringV1,
        output: &CompositionOutputV1,
        page_size: usize,
        cursor: Option<&RetrievalCursor>,
        now: UtcMicros,
    ) -> Result<CompositionPageV1, RetrievalError> {
        if page_size == 0 || page_size > request.budget.max_fused_candidates as usize {
            return Err(RetrievalError::InvalidRequest(
                "composition page size exceeds its deterministic budget".to_owned(),
            ));
        }

        let snapshot_digest = request.snapshot.compute_digest()?;
        let candidate_set_digest = digest_candidate_set(&output.ranked_candidates)?;
        let start = match cursor {
            Some(cursor) => {
                keyring.verify_cursor(cursor)?;
                if now.0 >= cursor.expiry.0 {
                    return Err(RetrievalError::CursorExpired);
                }
                cursor.validate()?;
                let query_digest = keyring
                    .digest_query_for(&cursor.key_id, cursor.key_epoch, request, query_view)
                    .map_err(|_| RetrievalError::CursorSetMismatch)?;
                validate_cursor(
                    cursor,
                    request,
                    output,
                    &query_digest,
                    &snapshot_digest,
                    &candidate_set_digest,
                    &self.ranking_revision,
                )?;
                cursor.next_ordinal as usize
            }
            None => 0,
        };
        if start > output.ranked_candidates.len() {
            return Err(RetrievalError::CursorSetMismatch);
        }

        let end = start
            .saturating_add(page_size)
            .min(output.ranked_candidates.len());
        let ranked_candidates = output.ranked_candidates[start..end].to_vec();
        let cursor = if end < output.ranked_candidates.len() {
            let query_digest = keyring
                .digest_active_query(request, query_view)
                .map_err(|error| RetrievalError::InvalidRequest(error.to_string()))?;
            Some(build_cursor(
                request,
                output,
                query_digest,
                snapshot_digest,
                candidate_set_digest,
                self.ranking_revision.clone(),
                end as u32,
                now,
                keyring,
            )?)
        } else {
            None
        };
        Ok(CompositionPageV1 {
            ranked_candidates,
            cursor,
        })
    }

    pub(crate) fn cursor_at(
        &self,
        request: &RetrievalRequest,
        query_view: &EphemeralSanitizedQueryViewV1,
        keyring: &RetrievalCursorKeyringV1,
        output: &CompositionOutputV1,
        next_ordinal: usize,
        now: UtcMicros,
    ) -> Result<RetrievalCursor, RetrievalError> {
        if next_ordinal > output.ranked_candidates.len() {
            return Err(RetrievalError::CursorSetMismatch);
        }
        let query_digest = keyring
            .digest_active_query(request, query_view)
            .map_err(|error| RetrievalError::InvalidRequest(error.to_string()))?;
        build_cursor(
            request,
            output,
            query_digest,
            request.snapshot.compute_digest()?,
            digest_candidate_set(&output.ranked_candidates)?,
            self.ranking_revision.clone(),
            u32::try_from(next_ordinal).map_err(|_| RetrievalError::CursorSetMismatch)?,
            now,
            keyring,
        )
    }
}

fn map_diversity_error(error: DiversityStageError) -> FusionStageError {
    FusionStageError::Contract(error.to_string())
}

struct AdmittedLanes {
    candidates: Vec<CompactCandidate>,
    internal_lane_outcomes: BTreeMap<RetrieverKind, RetrieverOutcome<()>>,
    public_lane_statuses: BTreeMap<RetrieverKind, PublicRetrieverStatus>,
    freshness: Vec<SourceFreshness>,
    lane_checkpoints: Vec<RetrieverContinuation>,
}

fn admitted_lanes(input: &FusionStageInput) -> Result<AdmittedLanes, FusionStageError> {
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    let mut internal_lane_outcomes = BTreeMap::new();
    let mut public_lane_statuses = BTreeMap::new();
    let mut freshness = Vec::new();
    let mut lane_checkpoints = Vec::new();

    for lane_input in &input.lanes {
        let lane = lane_input.lane;
        let outcome = &lane_input.outcome;
        if !seen.insert(lane) {
            return Err(FusionStageError::DuplicateLane);
        }
        internal_lane_outcomes.insert(lane, unit_outcome(outcome));
        public_lane_statuses.insert(lane, public_status(outcome));
        match outcome {
            RetrieverOutcome::Complete(batch) | RetrieverOutcome::Partial { value: batch, .. } => {
                batch.validate()?;
                if batch
                    .candidates
                    .iter()
                    .any(|candidate| candidate.retriever != lane)
                {
                    return Err(FusionStageError::Contract(
                        "lane key does not match batch candidates".to_owned(),
                    ));
                }
                candidates.extend(batch.candidates.iter().cloned());
                freshness.extend(
                    batch
                        .candidates
                        .iter()
                        .map(|candidate| candidate.freshness.clone()),
                );
                if let Some(checkpoint) = &batch.continuation {
                    lane_checkpoints.push(checkpoint.clone());
                }
            }
            RetrieverOutcome::Unavailable(_)
            | RetrieverOutcome::Denied
            | RetrieverOutcome::Stale(_)
            | RetrieverOutcome::BudgetExceeded(_)
            | RetrieverOutcome::Cancelled => {
                if matches!(lane, RetrieverKind::ExactLiteral | RetrieverKind::Lexical) {
                    return Err(FusionStageError::RequiredLaneUnavailable);
                }
            }
        }
    }

    if !seen.contains(&RetrieverKind::ExactLiteral) || !seen.contains(&RetrieverKind::Lexical) {
        return Err(FusionStageError::RequiredLaneUnavailable);
    }
    if input
        .profile
        .calibrations
        .keys()
        .chain(input.profile.weights_micros.keys())
        .any(|lane| !seen.contains(lane))
    {
        return Err(FusionStageError::ProfileLaneMismatch);
    }

    freshness.sort_by(freshness_cmp);
    freshness.dedup();
    lane_checkpoints.sort_by(|left, right| {
        left.lane
            .cmp(&right.lane)
            .then_with(|| left.checkpoint_digest.cmp(&right.checkpoint_digest))
    });
    Ok(AdmittedLanes {
        candidates,
        internal_lane_outcomes,
        public_lane_statuses,
        freshness,
        lane_checkpoints,
    })
}

fn unit_outcome(outcome: &RetrieverOutcome<RetrieverBatch<()>>) -> RetrieverOutcome<()> {
    match outcome {
        RetrieverOutcome::Complete(_) => RetrieverOutcome::Complete(()),
        RetrieverOutcome::Partial { reason, .. } => RetrieverOutcome::Partial {
            value: (),
            reason: reason.clone(),
        },
        RetrieverOutcome::Unavailable(reason) => RetrieverOutcome::Unavailable(reason.clone()),
        RetrieverOutcome::Denied => RetrieverOutcome::Denied,
        RetrieverOutcome::Stale(freshness) => RetrieverOutcome::Stale(freshness.clone()),
        RetrieverOutcome::BudgetExceeded(usage) => RetrieverOutcome::BudgetExceeded(*usage),
        RetrieverOutcome::Cancelled => RetrieverOutcome::Cancelled,
    }
}

fn public_status(outcome: &RetrieverOutcome<RetrieverBatch<()>>) -> PublicRetrieverStatus {
    match outcome {
        RetrieverOutcome::Complete(_) => PublicRetrieverStatus::Complete,
        RetrieverOutcome::Partial { .. } | RetrieverOutcome::BudgetExceeded(_) => {
            PublicRetrieverStatus::Partial
        }
        RetrieverOutcome::Stale(_) => PublicRetrieverStatus::Stale,
        RetrieverOutcome::Unavailable(_)
        | RetrieverOutcome::Denied
        | RetrieverOutcome::Cancelled => PublicRetrieverStatus::Unavailable,
    }
}

/// Checked fixed-point implementation of the fusion stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeterministicFixedPointFusion {
    comparator_revision: ComponentRevision,
}

impl DeterministicFixedPointFusion {
    pub fn new(comparator_revision: ComponentRevision) -> Self {
        Self {
            comparator_revision,
        }
    }

    fn fuse_compact(
        &self,
        profile: &FusionProfile,
        mut candidates: Vec<CompactCandidate>,
    ) -> Result<Vec<FusedCandidate>, FusionStageError> {
        candidates.sort_by(compact_candidate_cmp);
        let mut fused = BTreeMap::<(RetrievalAnchorId, LogicalEvidenceId), FusedCandidate>::new();

        for candidate in candidates {
            let calibration_profile_id = profile
                .calibrations
                .get(&candidate.retriever)
                .cloned()
                .ok_or(FusionStageError::ProfileLaneMismatch)?;
            let weight_micros = *profile
                .weights_micros
                .get(&candidate.retriever)
                .ok_or(FusionStageError::ProfileLaneMismatch)?;
            let calibration = profile
                .score_domain_calibrations
                .get(&candidate.score_domain)
                .ok_or(FusionStageError::ProfileLaneMismatch)?;
            if calibration.calibration_profile_id != calibration_profile_id
                || calibration.score_domain != candidate.score_domain
            {
                return Err(FusionStageError::ProfileLaneMismatch);
            }
            let calibrated_feature_micros = calibration.calibrate(candidate.raw_score)?;
            let weighted_contribution_micros =
                FixedPointScore(u64::from(calibrated_feature_micros))
                    .checked_weight(weight_micros)?;
            let exact_class = candidate.exact_class();
            let occurrence = occurrence_from(&candidate);
            let contribution = CandidateContribution {
                retriever: candidate.retriever,
                retriever_revision: candidate.retriever_revision.clone(),
                source_occurrence_id: candidate.source_occurrence_id.clone(),
                ordinal_rank: candidate.ordinal_rank,
                raw_score: candidate.raw_score,
                score_domain: candidate.score_domain.clone(),
                calibration_profile_id,
                calibrated_feature_micros,
                weight_micros,
                weighted_contribution_micros,
            };
            let key = (
                candidate.anchor_id.clone(),
                candidate.logical_evidence_id.clone(),
            );
            let entry = fused.entry(key).or_insert_with(|| FusedCandidate {
                anchor_id: candidate.anchor_id.clone(),
                logical_evidence_id: candidate.logical_evidence_id.clone(),
                occurrences: Vec::new(),
                exact_class,
                utility_micros: 0,
                contributions: Vec::new(),
                freshness: Vec::new(),
                decisions: Vec::new(),
            });
            entry.exact_class = strongest_exact_class(entry.exact_class, exact_class);
            entry.utility_micros = entry
                .utility_micros
                .checked_add(weighted_contribution_micros)
                .ok_or(FusionStageError::FixedPointOverflow)?;
            entry.occurrences.push(occurrence);
            entry.contributions.push(contribution);
            entry.freshness.push(candidate.freshness.clone());
            if exact_class != ExactClass::Approximate {
                entry.decisions.push(RankingDecision {
                    kind: RankingDecisionKind::ExactTierAdmission,
                    retriever: Some(RetrieverKind::ExactLiteral),
                    policy_anchor: Some(profile.evaluation_result_anchor.clone()),
                    evidence_anchor: Some(candidate.retriever_evidence_anchor.clone()),
                    detail: "validated exact admission proof".to_owned(),
                });
            }
        }

        let mut fused = fused.into_values().collect::<Vec<_>>();
        for candidate in &mut fused {
            candidate.occurrences.sort_by(occurrence_cmp);
            candidate.occurrences.dedup();
            candidate.contributions.sort_by(contribution_cmp);
            candidate.freshness.sort_by(freshness_cmp);
            candidate.freshness.dedup();
            candidate.decisions.sort_by(decision_cmp);
            candidate.decisions.dedup();
            candidate.validate()?;
        }
        Ok(fused)
    }

    fn order_fused(&self, mut candidates: Vec<FusedCandidate>) -> Vec<FusedCandidate> {
        candidates.sort_by(compare_fused);
        for candidate in &mut candidates {
            candidate
                .decisions
                .retain(|decision| decision.kind != RankingDecisionKind::ComparatorProvenance);
            let record = self.comparator_record(candidate);
            candidate.decisions.push(RankingDecision {
                kind: RankingDecisionKind::ComparatorProvenance,
                retriever: None,
                policy_anchor: None,
                evidence_anchor: candidate
                    .occurrences
                    .first()
                    .map(|occurrence| occurrence.retriever_evidence_anchor.clone()),
                detail: format!(
                    "exact={:?};utility={};source_validity={};anchor={};logical={};occurrences=[{}];revision={}",
                    record.exact_class,
                    record.utility_micros,
                    record.source_validity_rank,
                    record.anchor_id,
                    record.logical_evidence_id,
                    record
                        .source_occurrence_ids
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(","),
                    record.comparator_revision,
                ),
            });
            candidate.decisions.sort_by(decision_cmp);
        }
        candidates
    }

    pub fn comparator_record(&self, candidate: &FusedCandidate) -> FusionComparatorRecordV1 {
        FusionComparatorRecordV1 {
            exact_class: candidate.exact_class,
            utility_micros: candidate.utility_micros,
            source_validity_rank: source_validity_rank(candidate),
            anchor_id: candidate.anchor_id.clone(),
            logical_evidence_id: candidate.logical_evidence_id.clone(),
            source_occurrence_ids: ordered_occurrence_ids(candidate),
            comparator_revision: self.comparator_revision.clone(),
        }
    }
}

impl DeterministicFusionStage for DeterministicFixedPointFusion {
    fn fuse(&self, input: &FusionStageInput) -> Result<Vec<FusedCandidate>, FusionStageError> {
        let admitted = admitted_lanes(input)?;
        self.fuse_compact(&input.profile, admitted.candidates)
    }

    fn order(&self, candidates: Vec<FusedCandidate>) -> Vec<RankedCandidate> {
        self.order_fused(candidates)
            .into_iter()
            .enumerate()
            .map(|(ordinal, candidate)| RankedCandidate {
                candidate,
                final_ordinal: ordinal as u32,
            })
            .collect()
    }
}

fn compact_candidate_cmp(left: &CompactCandidate, right: &CompactCandidate) -> Ordering {
    left.anchor_id
        .cmp(&right.anchor_id)
        .then_with(|| left.logical_evidence_id.cmp(&right.logical_evidence_id))
        .then_with(|| left.source_occurrence_id.cmp(&right.source_occurrence_id))
        .then_with(|| left.retriever.cmp(&right.retriever))
        .then_with(|| {
            left.retriever_evidence_anchor
                .cmp(&right.retriever_evidence_anchor)
        })
        .then_with(|| {
            left.logical_copy_evidence_anchor
                .cmp(&right.logical_copy_evidence_anchor)
        })
}

fn occurrence_from(candidate: &CompactCandidate) -> OccurrenceProvenance {
    OccurrenceProvenance {
        source_occurrence_id: candidate.source_occurrence_id.clone(),
        file_occurrence_id: candidate.file_occurrence_id.clone(),
        retriever_evidence_anchor: candidate.retriever_evidence_anchor.clone(),
        source_namespace: candidate.source_namespace.clone(),
        repository_id: candidate.repository_id.clone(),
        session_or_thread_id: candidate.session_or_thread_id.clone(),
        logical_copy_cluster_id: candidate.logical_copy_cluster_id.clone(),
        logical_copy_evidence_anchor: candidate.logical_copy_evidence_anchor.clone(),
        evidence_role: candidate.evidence_role,
        freshness: candidate.freshness.clone(),
    }
}

fn strongest_exact_class(left: ExactClass, right: ExactClass) -> ExactClass {
    if exact_class_rank(left) <= exact_class_rank(right) {
        left
    } else {
        right
    }
}

fn occurrence_cmp(left: &OccurrenceProvenance, right: &OccurrenceProvenance) -> Ordering {
    left.source_occurrence_id
        .cmp(&right.source_occurrence_id)
        .then_with(|| {
            left.retriever_evidence_anchor
                .cmp(&right.retriever_evidence_anchor)
        })
        .then_with(|| {
            left.logical_copy_evidence_anchor
                .cmp(&right.logical_copy_evidence_anchor)
        })
}

fn contribution_cmp(left: &CandidateContribution, right: &CandidateContribution) -> Ordering {
    left.retriever
        .cmp(&right.retriever)
        .then_with(|| left.ordinal_rank.cmp(&right.ordinal_rank))
        .then_with(|| left.source_occurrence_id.cmp(&right.source_occurrence_id))
        .then_with(|| left.score_domain.cmp(&right.score_domain))
}

fn freshness_cmp(left: &SourceFreshness, right: &SourceFreshness) -> Ordering {
    left.source_namespace
        .cmp(&right.source_namespace)
        .then_with(|| left.source_instance.cmp(&right.source_instance))
        .then_with(|| left.source_generation.cmp(&right.source_generation))
        .then_with(|| left.projection_watermark.cmp(&right.projection_watermark))
        .then_with(|| left.policy_revision.cmp(&right.policy_revision))
}

fn decision_cmp(left: &RankingDecision, right: &RankingDecision) -> Ordering {
    left.kind
        .cmp(&right.kind)
        .then_with(|| left.retriever.cmp(&right.retriever))
        .then_with(|| left.policy_anchor.cmp(&right.policy_anchor))
        .then_with(|| left.evidence_anchor.cmp(&right.evidence_anchor))
        .then_with(|| left.detail.cmp(&right.detail))
}

fn attach_same_source_decisions(
    candidates: &mut [FusedCandidate],
    decisions: &[DedupeDecisionV1],
) -> Result<(), FusionStageError> {
    // Index occurrences by their `(source_occurrence_id,
    // retriever_evidence_anchor)` pair once, so each recorded decision resolves
    // its fused candidate in O(1) instead of rescanning the full fused slice
    // per decision. `or_insert` keeps the first candidate (in slice order) that
    // owns a matching occurrence, mirroring the prior `iter_mut().find(..)`
    // first-match semantics exactly.
    let mut occurrence_index: HashMap<(&SourceOccurrenceId, &RetrievalAnchorId), usize> =
        HashMap::new();
    for (position, candidate) in candidates.iter().enumerate() {
        for occurrence in &candidate.occurrences {
            occurrence_index
                .entry((
                    &occurrence.source_occurrence_id,
                    &occurrence.retriever_evidence_anchor,
                ))
                .or_insert(position);
        }
    }

    let mut targets = Vec::with_capacity(decisions.len());
    for recorded in decisions {
        let position = recorded
            .decision
            .evidence_anchor
            .as_ref()
            .and_then(|anchor| {
                occurrence_index
                    .get(&(&recorded.kept_occurrence, anchor))
                    .copied()
            })
            .ok_or_else(|| {
                FusionStageError::Contract(
                    "same-source collapse decision lost its fused candidate".to_owned(),
                )
            })?;
        targets.push(position);
    }
    drop(occurrence_index);

    for (recorded, position) in decisions.iter().zip(targets) {
        let candidate = &mut candidates[position];
        candidate.decisions.push(recorded.decision.clone());
        candidate.decisions.sort_by(decision_cmp);
        candidate.decisions.dedup();
    }
    Ok(())
}

pub fn digest_candidate_set(
    candidates: &[RankedCandidate],
) -> Result<CandidateSetDigest, RetrievalError> {
    digest_value("tracedecay.retrieval-candidate-set.v1", candidates)
        .and_then(|value| CandidateSetDigest::new(value).map_err(RetrievalError::from))
}

fn digest_value<T: Serialize + ?Sized>(
    domain: &'static str,
    value: &T,
) -> Result<String, RetrievalError> {
    let bytes = serde_json::to_vec(&(domain, value))
        .map_err(|error| RetrievalError::InvalidRequest(error.to_string()))?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
}

#[allow(clippy::too_many_arguments)]
fn build_cursor(
    request: &RetrievalRequest,
    output: &CompositionOutputV1,
    query_digest: QueryDigest,
    snapshot_digest: CandidateSetDigest,
    candidate_set_digest: CandidateSetDigest,
    ranking_revision: ComponentRevision,
    next_ordinal: u32,
    now: UtcMicros,
    keyring: &RetrievalCursorKeyringV1,
) -> Result<RetrievalCursor, RetrievalError> {
    let (key_id, key_epoch) = keyring.active_key();
    let mut cursor = RetrievalCursor {
        key_id: key_id.clone(),
        key_epoch,
        privacy_domain: request.scope.privacy_domain.clone(),
        query_digest,
        profile_id: output.profile_id.clone(),
        snapshot_digest,
        freshness_digest: request.snapshot.freshness_digest.clone(),
        authorization_revision: request.snapshot.authorization_revision.clone(),
        candidate_set_digest,
        public_lane_statuses: output.public_lane_statuses.clone(),
        lane_checkpoints: output.lane_checkpoints.clone(),
        ranking_revision: tracedecay_domain::RankingRevision::new(
            ranking_revision.as_str().to_owned(),
        )?,
        next_ordinal,
        semantic: None,
        expiry: keyring.expiry_from(now)?,
        signature: QueryMac::new(format!("hmac-sha256:{}", "0".repeat(64)))?,
    };
    cursor.signature = keyring.sign_cursor(&cursor)?;
    Ok(cursor)
}

#[derive(Serialize)]
struct CursorAuthenticatedPayload<'a> {
    domain: &'static str,
    key_id: &'a RetrievalCursorKeyId,
    key_epoch: u64,
    privacy_domain: &'a PrivacyDomainId,
    query_digest: &'a QueryDigest,
    profile_id: &'a tracedecay_domain::FusionProfileId,
    snapshot_digest: &'a CandidateSetDigest,
    freshness_digest: &'a tracedecay_domain::FreshnessVectorDigest,
    authorization_revision: &'a tracedecay_domain::AuthorizationRevision,
    candidate_set_digest: &'a CandidateSetDigest,
    public_lane_statuses: &'a BTreeMap<RetrieverKind, PublicRetrieverStatus>,
    lane_checkpoints: &'a [RetrieverContinuation],
    ranking_revision: &'a tracedecay_domain::RankingRevision,
    next_ordinal: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    semantic: &'a Option<tracedecay_domain::SemanticRetrievalContinuationV1>,
    expiry: UtcMicros,
}

fn cursor_authenticated_bytes(cursor: &RetrievalCursor) -> Result<Vec<u8>, RetrievalError> {
    serde_json::to_vec(&CursorAuthenticatedPayload {
        domain: RETRIEVAL_CURSOR_MAC_DOMAIN,
        key_id: &cursor.key_id,
        key_epoch: cursor.key_epoch,
        privacy_domain: &cursor.privacy_domain,
        query_digest: &cursor.query_digest,
        profile_id: &cursor.profile_id,
        snapshot_digest: &cursor.snapshot_digest,
        freshness_digest: &cursor.freshness_digest,
        authorization_revision: &cursor.authorization_revision,
        candidate_set_digest: &cursor.candidate_set_digest,
        public_lane_statuses: &cursor.public_lane_statuses,
        lane_checkpoints: &cursor.lane_checkpoints,
        ranking_revision: &cursor.ranking_revision,
        next_ordinal: cursor.next_ordinal,
        semantic: &cursor.semantic,
        expiry: cursor.expiry,
    })
    .map_err(|error| RetrievalError::InvalidRequest(error.to_string()))
}

fn validate_cursor(
    cursor: &RetrievalCursor,
    request: &RetrievalRequest,
    output: &CompositionOutputV1,
    query_digest: &QueryDigest,
    snapshot_digest: &CandidateSetDigest,
    candidate_set_digest: &CandidateSetDigest,
    ranking_revision: &ComponentRevision,
) -> Result<(), RetrievalError> {
    cursor.validate()?;
    let expected_ranking_revision =
        tracedecay_domain::RankingRevision::new(ranking_revision.as_str().to_owned())?;
    if cursor.query_digest != *query_digest
        || cursor.profile_id != output.profile_id
        || cursor.snapshot_digest != *snapshot_digest
        || cursor.freshness_digest != request.snapshot.freshness_digest
        || cursor.authorization_revision != request.snapshot.authorization_revision
        || cursor.candidate_set_digest != *candidate_set_digest
        || cursor.public_lane_statuses != output.public_lane_statuses
        || cursor.lane_checkpoints != output.lane_checkpoints
        || cursor.ranking_revision != expected_ranking_revision
        || cursor.privacy_domain != request.scope.privacy_domain
    {
        return Err(RetrievalError::CursorSetMismatch);
    }
    Ok(())
}

fn keyed_mac(
    secret: &[u8],
    authenticated: &[u8],
) -> Result<QueryMac, QueryDigestAuthenticationError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret)
        .map_err(|_| QueryDigestAuthenticationError::InvalidKeyMaterial)?;
    mac.update(authenticated);
    QueryMac::new(format!(
        "hmac-sha256:{}",
        hex::encode(mac.finalize().into_bytes())
    ))
    .map_err(QueryDigestAuthenticationError::from)
}

fn query_mac_bytes(mac: &QueryMac) -> Result<Vec<u8>, QueryDigestAuthenticationError> {
    let encoded = mac
        .as_str()
        .strip_prefix("hmac-sha256:")
        .ok_or(QueryDigestAuthenticationError::InvalidKeyMaterial)?;
    hex::decode(encoded).map_err(|_| QueryDigestAuthenticationError::InvalidKeyMaterial)
}

fn map_cursor_key_error(error: QueryDigestAuthenticationError) -> RetrievalError {
    match error {
        QueryDigestAuthenticationError::KeyUnavailable => RetrievalError::CursorKeyUnavailable,
        QueryDigestAuthenticationError::KeyRevoked => RetrievalError::CursorKeyRevoked,
        QueryDigestAuthenticationError::InvalidKeyMaterial
        | QueryDigestAuthenticationError::AuthenticationFailed
        | QueryDigestAuthenticationError::PrivacyDomainMismatch
        | QueryDigestAuthenticationError::Canonicalization(_)
        | QueryDigestAuthenticationError::Contract(_) => RetrievalError::CursorAuthenticationFailed,
    }
}

fn current_utc_micros() -> Result<UtcMicros, RetrievalError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RetrievalError::InvalidRequest("system clock precedes epoch".to_owned()))?;
    let micros = i64::try_from(duration.as_micros())
        .map_err(|_| RetrievalError::InvalidRequest("system clock overflowed".to_owned()))?;
    Ok(UtcMicros(micros))
}

#[cfg(test)]
mod attach_same_source_decisions_tests {
    use super::*;
    use tracedecay_domain::{EvidenceRole, FreshnessCompatibilityV1};

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn freshness() -> SourceFreshness {
        SourceFreshness {
            source_namespace: id("namespace.code"),
            source_instance: id("source.fixture"),
            source_watermark: Some(7),
            projection_watermark: Some(7),
            observed_at: UtcMicros(7),
            source_generation: Some(1),
            generation_lag: Some(0),
            compatibility: FreshnessCompatibilityV1::Current,
            policy_revision: id("policy.v1"),
        }
    }

    fn occurrence(name: &str) -> OccurrenceProvenance {
        OccurrenceProvenance {
            source_occurrence_id: id::<SourceOccurrenceId>(&format!("occurrence.{name}")),
            file_occurrence_id: None,
            retriever_evidence_anchor: RetrievalAnchorId::new(format!("evidence.{name}"))
                .expect("valid evidence anchor"),
            source_namespace: id("namespace.code"),
            repository_id: None,
            session_or_thread_id: None,
            logical_copy_cluster_id: None,
            logical_copy_evidence_anchor: None,
            evidence_role: EvidenceRole::Primary,
            freshness: freshness(),
        }
    }

    fn fused(occurrences: Vec<OccurrenceProvenance>) -> FusedCandidate {
        FusedCandidate {
            anchor_id: RetrievalAnchorId::new(format!(
                "anchor.{}",
                occurrences[0].source_occurrence_id.as_str()
            ))
            .expect("valid anchor"),
            logical_evidence_id: id::<LogicalEvidenceId>("logical.fixture"),
            occurrences,
            exact_class: ExactClass::Approximate,
            utility_micros: 0,
            contributions: Vec::new(),
            freshness: Vec::new(),
            decisions: Vec::new(),
        }
    }

    fn same_source_decision(name: &str, detail: &str) -> DedupeDecisionV1 {
        DedupeDecisionV1 {
            kept_occurrence: id::<SourceOccurrenceId>(&format!("occurrence.{name}")),
            collapsed_occurrences: Vec::new(),
            collapsed_candidates: Vec::new(),
            copy_cluster: None,
            decision: RankingDecision {
                kind: RankingDecisionKind::SameSourceDuplicateCollapse,
                retriever: None,
                policy_anchor: None,
                evidence_anchor: Some(
                    RetrievalAnchorId::new(format!("evidence.{name}"))
                        .expect("valid evidence anchor"),
                ),
                detail: detail.to_owned(),
            },
        }
    }

    #[test]
    fn attaches_each_decision_to_the_first_matching_candidate() {
        let mut candidates = vec![fused(vec![occurrence("a")]), fused(vec![occurrence("b")])];
        let decisions = vec![
            same_source_decision("b", "collapsed-b"),
            same_source_decision("a", "collapsed-a"),
        ];

        attach_same_source_decisions(&mut candidates, &decisions)
            .expect("decisions attach to their fused candidate");

        assert_eq!(candidates[0].decisions.len(), 1);
        assert_eq!(candidates[0].decisions[0].detail, "collapsed-a");
        assert_eq!(candidates[1].decisions.len(), 1);
        assert_eq!(candidates[1].decisions[0].detail, "collapsed-b");
    }

    #[test]
    fn resolves_the_first_candidate_when_two_share_an_occurrence_key() {
        // Two candidates carry an occurrence with the identical
        // (source_occurrence_id, evidence anchor) key. The decision must attach
        // to the first candidate in slice order, matching the prior
        // `iter_mut().find(..)` first-match semantics the index preserves.
        let mut candidates = vec![
            fused(vec![occurrence("shared")]),
            fused(vec![occurrence("shared")]),
        ];
        let decisions = vec![same_source_decision("shared", "collapsed-shared")];

        attach_same_source_decisions(&mut candidates, &decisions)
            .expect("decision attaches to the first matching candidate");

        assert_eq!(candidates[0].decisions.len(), 1);
        assert!(candidates[1].decisions.is_empty());
    }

    #[test]
    fn sorts_and_dedups_decisions_attached_to_one_candidate() {
        let mut candidates = vec![fused(vec![occurrence("a")])];
        let decisions = vec![
            same_source_decision("a", "z-detail"),
            same_source_decision("a", "z-detail"),
            same_source_decision("a", "a-detail"),
        ];

        attach_same_source_decisions(&mut candidates, &decisions).expect("decisions attach");

        // Identical decisions collapse; the survivors are ordered by
        // decision_cmp (detail ascending as the final tie-break).
        let details: Vec<_> = candidates[0]
            .decisions
            .iter()
            .map(|decision| decision.detail.as_str())
            .collect();
        assert_eq!(details, vec!["a-detail", "z-detail"]);
    }

    #[test]
    fn missing_candidate_is_a_contract_error() {
        let mut candidates = vec![fused(vec![occurrence("a")])];
        let decisions = vec![same_source_decision("missing", "orphan")];

        let error = attach_same_source_decisions(&mut candidates, &decisions)
            .expect_err("a decision without a fused candidate is a contract violation");
        assert!(matches!(error, FusionStageError::Contract(_)));
    }

    #[test]
    fn decision_without_evidence_anchor_is_a_contract_error() {
        let mut candidates = vec![fused(vec![occurrence("a")])];
        let mut decision = same_source_decision("a", "no-anchor");
        decision.decision.evidence_anchor = None;

        let error = attach_same_source_decisions(&mut candidates, &[decision])
            .expect_err("a decision without an evidence anchor cannot bind a candidate");
        assert!(matches!(error, FusionStageError::Contract(_)));
    }
}

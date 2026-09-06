use std::collections::HashMap;
use std::error::Error;
use std::future::Future;
use std::sync::{LazyLock, RwLock};

use sha2::{Digest, Sha256};
use tracedecay_domain::{
    AccessPolicyDigest, AnchorDurabilityClass, AnchorSourceGenerationV2, CanonicalObservationIdV1,
    CapabilityId, CoverageReportV1, DomainError, DurableObservationV1, EvidenceClass,
    NativeAliasKindV2, NativeAliasV2, ObservationCollisionOutcomeV1, ObservationContractError,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadAccessState, PayloadDigestV1, PayloadReferenceV1, PrivacyDomainBoundLocatorDigest,
    PrivacyDomainId, ProjectionGenerationId, ResolutionAuthorizationV1, RetrievalAnchorId,
    RetrievalAnchorRecordV2, RetrievalAnchorRecordV2Parts, RetrievalAnchorTargetV2,
    SanitizationReceiptId, SanitizationReceiptV1, SanitizerDispositionV1, ScopeResolutionId,
    UtcMicros, VectorWatermark,
};

mod anchored_write;

use anchored_write::validate_retrieval_anchor_binding;
pub use anchored_write::{
    AnchoredObservationWrite, ObservationIdentityCollisionDispositionV1,
    RepositoryProvenanceAttachmentV1,
};

const MAX_REPLAY_LIMIT: usize = 1_000;

/// Canonical authority namespace bound into every observation-capture anchor.
pub const OBSERVATION_CAPTURE_AUTHORITY_V1: &str = "observation-capture.v1";

fn cursor_transition_covers(
    expected: Option<&ObservationSourceCursorV1>,
    next: &ObservationSourceCursorV1,
    covered: ObservationSourceRangeV1,
) -> bool {
    if next.position() != covered.end() {
        return false;
    }
    if next.ordering_domain() == ObservationOrderingDomainV1::FileBytes
        && expected.is_none_or(|cursor| cursor.generation() != next.generation())
        && covered.start() != 0
    {
        return false;
    }
    let Some(expected) = expected else {
        return true;
    };
    if expected.source() != next.source() || expected.scope() != next.scope() {
        return false;
    }
    expected.generation() != next.generation()
        || (expected.ordering_domain() == next.ordering_domain()
            && expected.position() == covered.start())
}

/// Validated request to persist one sanitized observation and advance its source cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationWrite {
    observation: DurableObservationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    next_cursor: ObservationSourceCursorV1,
}

impl ObservationWrite {
    pub fn new(
        observation: DurableObservationV1,
        expected_cursor: Option<ObservationSourceCursorV1>,
        next_cursor: ObservationSourceCursorV1,
    ) -> ObservationStoreResult<Self> {
        if observation.source() != next_cursor.source()
            || observation.scope() != next_cursor.scope()
            || observation.identity().generation() != next_cursor.generation()
            || observation.identity().ordering_domain() != next_cursor.ordering_domain()
            || observation.identity().position().end() != next_cursor.position()
            || !cursor_transition_covers(
                expected_cursor.as_ref(),
                &next_cursor,
                observation.identity().position(),
            )
        {
            return Err(ObservationStoreError::CursorObservationMismatch);
        }
        Ok(Self {
            observation,
            expected_cursor,
            next_cursor,
        })
    }

    pub fn observation(&self) -> &DurableObservationV1 {
        &self.observation
    }

    pub fn expected_cursor(&self) -> Option<&ObservationSourceCursorV1> {
        self.expected_cursor.as_ref()
    }

    pub fn next_cursor(&self) -> &ObservationSourceCursorV1 {
        &self.next_cursor
    }

    pub fn into_parts(
        self,
    ) -> (
        DurableObservationV1,
        Option<ObservationSourceCursorV1>,
        ObservationSourceCursorV1,
    ) {
        (self.observation, self.expected_cursor, self.next_cursor)
    }
}

/// Derives an owner-bound authorization snapshot when ingress has no richer policy object.
pub fn build_observation_resolution_authorization_v1(
    observation: &DurableObservationV1,
    authority_namespace: &str,
) -> ObservationStoreResult<ResolutionAuthorizationV1> {
    hotpath::measure_block!("store.observation.build_observation_authorization", {
        let canonical_request_digest = PayloadReferenceV1::for_payload(&serde_json::json!({
            "domain": "tracedecay.observation-anchor.request.v1",
            "authority": authority_namespace,
            "owner": observation.scope(),
            "observation_id": observation.observation_id(),
        }))
        .map_err(ObservationStoreError::Contract)?
        .digest()
        .as_str()
        .to_owned();
        build_resolution_authorization_v1(authority_namespace, canonical_request_digest)
    })
}

/// Derives a caller-bound authorization snapshot for resolutions that have no
/// retained record to carry one, such as absent or ambiguous anchor bindings.
/// The request digest binds only the owner scope and the requested anchor id;
/// it never embeds payload bytes or a source locator.
pub fn build_scope_resolution_authorization_v1(
    scope: &ObservationScopeV1,
    anchor_id: &RetrievalAnchorId,
    authority_namespace: &str,
) -> ObservationStoreResult<ResolutionAuthorizationV1> {
    hotpath::measure_block!("store.observation.build_scope_authorization", {
        let canonical_request_digest = PayloadReferenceV1::for_payload(&serde_json::json!({
            "domain": "tracedecay.observation-anchor.request.v1",
            "authority": authority_namespace,
            "owner": scope,
            "anchor_id": anchor_id,
        }))
        .map_err(ObservationStoreError::Contract)?
        .digest()
        .as_str()
        .to_owned();
        build_resolution_authorization_v1(authority_namespace, canonical_request_digest)
    })
}

/// Upper bound on memoized access-policy digests.
///
/// Authority namespaces are compile-time constants in production, so this only
/// exists so a caller passing unbounded namespaces cannot grow the memo without
/// limit; past the bound the digest is derived without being retained.
const MAX_MEMOIZED_ACCESS_POLICY_DIGESTS: usize = 64;

/// Access-policy digests keyed by authority namespace.
///
/// The digested value binds nothing but the authorization domain constant and
/// the namespace, so it is the same bytes for every resolution in that
/// namespace. Deriving it per resolution put a canonical-JSON encode plus a
/// SHA-256 compression on the anchor-resolution serving path for a value that
/// was born the first time the namespace was used.
///
/// The lock type follows `hotpath::rw_lock!`: instrumented wrapper when the
/// `hotpath` feature is on, `std::sync::RwLock` when it is off.
static ACCESS_POLICY_DIGESTS: LazyLock<hotpath::rw_locks::RwLock<HashMap<String, String>>> =
    LazyLock::new(|| {
        hotpath::rw_lock!(
            RwLock::new(HashMap::new()),
            label = "store.observation.access_policy_digests"
        )
    });

fn access_policy_digest_for(authority_namespace: &str) -> ObservationStoreResult<String> {
    hotpath::measure_block!("store.observation.access_policy_digest", {
        if let Ok(memo) = ACCESS_POLICY_DIGESTS.read()
            && let Some(digest) = memo.get(authority_namespace)
        {
            return Ok(digest.clone());
        }
        let digest = PayloadReferenceV1::for_payload(&serde_json::json!({
            "domain": "tracedecay.observation-anchor.authorization.v1",
            "authority": authority_namespace,
        }))
        .map_err(ObservationStoreError::Contract)?
        .digest()
        .as_str()
        .to_owned();
        if let Ok(mut memo) = ACCESS_POLICY_DIGESTS.write()
            && memo.len() < MAX_MEMOIZED_ACCESS_POLICY_DIGESTS
        {
            memo.insert(authority_namespace.to_owned(), digest.clone());
        }
        Ok(digest)
    })
}

/// Returns the exact access-policy digest retained by production observation
/// anchors so retrieval admission can bind to the same authority without
/// duplicating its canonical digest construction.
pub fn observation_capture_access_policy_digest_v1() -> ObservationStoreResult<AccessPolicyDigest> {
    AccessPolicyDigest::new(access_policy_digest_for(OBSERVATION_CAPTURE_AUTHORITY_V1)?)
        .map_err(ObservationStoreError::RetrievalAnchorContract)
}

fn build_resolution_authorization_v1(
    authority_namespace: &str,
    canonical_request_digest: String,
) -> ObservationStoreResult<ResolutionAuthorizationV1> {
    let access_policy_digest = access_policy_digest_for(authority_namespace)?;
    Ok(ResolutionAuthorizationV1 {
        resolved_scope_id: ScopeResolutionId::new(format!("scope.{authority_namespace}"))
            .map_err(ObservationStoreError::RetrievalAnchorContract)?,
        privacy_domain_id: PrivacyDomainId::new(format!("privacy.{authority_namespace}"))
            .map_err(ObservationStoreError::RetrievalAnchorContract)?,
        access_policy_digest: AccessPolicyDigest::new(access_policy_digest)
            .map_err(ObservationStoreError::RetrievalAnchorContract)?,
        capability_id: CapabilityId::new(format!("capability.{authority_namespace}"))
            .map_err(ObservationStoreError::RetrievalAnchorContract)?,
        canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(canonical_request_digest)
            .map_err(ObservationStoreError::RetrievalAnchorContract)?,
    })
}

/// Builds the canonical stable anchor for one retained sanitized observation.
pub fn build_observation_retrieval_anchor_v2(
    observation: &DurableObservationV1,
    projection_generation: ProjectionGenerationId,
    ingested_at: UtcMicros,
    authorization: ResolutionAuthorizationV1,
) -> ObservationStoreResult<RetrievalAnchorRecordV2> {
    hotpath::measure_block!("store.observation.build_retrieval_anchor", {
        let aliases = observation
            .identity()
            .native_record_id()
            .map(|native_record_id| {
                let locator = serde_json::json!({
                    "owner": observation.scope(),
                    "provider": observation.source().provider(),
                    "session_id": observation.source().session_id(),
                    "native_record_id": native_record_id,
                });
                let digest = PayloadReferenceV1::for_payload(&locator)
                    .map_err(ObservationStoreError::Contract)?
                    .digest()
                    .as_str()
                    .to_owned();
                let locator_digest = PrivacyDomainBoundLocatorDigest::new(digest)
                    .map_err(ObservationStoreError::RetrievalAnchorContract)?;
                NativeAliasV2::new(NativeAliasKindV2::ProviderRecord, locator_digest)
                    .map_err(ObservationStoreError::RetrievalAnchorContract)
            })
            .transpose()?
            .into_iter()
            .collect();
        RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
            target: RetrievalAnchorTargetV2::ExactObservation(observation.observation_id().clone()),
            owner: observation.scope().clone(),
            aliases,
            occurred_at: None,
            ingested_at,
            evidence_class: EvidenceClass::Observed,
            source_generation: AnchorSourceGenerationV2::Observation(
                observation.identity().generation(),
            ),
            projection_generation,
            projection_watermark: VectorWatermark::default(),
            coverage: CoverageReportV1::default(),
            source_observations: vec![observation.observation_id().clone()],
            source_anchors: vec![],
            authorization,
            payload_access: PayloadAccessState::Eligible,
            retention_class: observation.retention_class().clone(),
            durability: AnchorDurabilityClass::DurableEvidence,
        })
        .map_err(ObservationStoreError::RetrievalAnchorContract)
    })
}

/// Store-side observation of an evidence-anchor binding at resolution time.
///
/// This is the raw material for a typed resolution report: it carries the
/// retained record together with the store's current projection watermark, or
/// a safe signal when no single authoritative record can be presented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservedEvidenceAnchorResolution {
    /// Exactly one authoritative record resolved for the anchor. The observed
    /// watermark is the store's current projection-stream position reported
    /// under exactly the shard keys the record's frozen watermark claims.
    Resolved {
        record: Box<RetrievalAnchorRecordV2>,
        observed_watermark: VectorWatermark,
    },
    /// No binding for the anchor exists in this authority.
    Unavailable,
    /// The anchor binds to conflicting evidence in this authority, so no
    /// single record may be presented.
    Ambiguous,
}

/// Fully processed provider evidence that intentionally produces no durable observation.
#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ObservationCoverageReason {
    BlankFrame,
    OutOfScope,
    MalformedFrame,
    OversizedFrame,
    UnknownVersion,
    UnsupportedFact,
    DuplicateObservation,
    /// A retained native record was written by an earlier canonical payload
    /// revision and the current provider-specific semantic comparator proved
    /// that only bounded source-context fields changed.
    CanonicalPayloadRevision,
    SanitizerRejected,
    SanitizerQuarantined,
    /// The daemon admission refused the record with a deterministic,
    /// non-retryable disposition (e.g. a content-derived identity conflict).
    /// Coverage advances so the stream converges; the refusal stays auditable
    /// through the recorded cursor-advance reason.
    AdmissionRefused,
    /// A provider record's canonical identity resolved to already-retained,
    /// incompatible evidence. The bounded code lets Doctor distinguish this
    /// terminal identity defect from another admission refusal without ever
    /// retaining transcript content in the coverage ledger.
    ObservationIdentityCollision,
}

impl ObservationCoverageReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BlankFrame => "blank_frame",
            Self::OutOfScope => "out_of_scope",
            Self::MalformedFrame => "malformed_frame",
            Self::OversizedFrame => "oversized_frame",
            Self::UnknownVersion => "unknown_version",
            Self::UnsupportedFact => "unsupported_fact",
            Self::DuplicateObservation => "duplicate_observation",
            Self::CanonicalPayloadRevision => "canonical_payload_revision",
            Self::SanitizerRejected => "sanitizer_rejected",
            Self::SanitizerQuarantined => "sanitizer_quarantined",
            Self::AdmissionRefused => "admission_refused",
            Self::ObservationIdentityCollision => "observation_identity_collision",
        }
    }

    /// True when the evidence could have produced a durable observation but
    /// was refused — the counts Doctor surfaces as a degraded (yet observed
    /// and final) condition. Blank, out-of-scope, unsupported-fact, and
    /// duplicate dispositions are expected coverage outcomes, not refusals.
    pub fn is_refusal(self) -> bool {
        matches!(
            self,
            Self::MalformedFrame
                | Self::OversizedFrame
                | Self::UnknownVersion
                | Self::SanitizerRejected
                | Self::SanitizerQuarantined
                | Self::AdmissionRefused
                | Self::ObservationIdentityCollision
        )
    }

    /// True when this reason never carries a sanitization receipt, i.e. the
    /// evidence was disposed of before it ever reached the sanitizer.
    pub fn is_receiptless(self) -> bool {
        !matches!(
            self,
            Self::SanitizerRejected
                | Self::SanitizerQuarantined
                | Self::DuplicateObservation
                | Self::CanonicalPayloadRevision
        )
    }

    /// Whether `disposition` is the sanitizer disposition this reason
    /// requires. Receiptless reasons require `None`; receipt-bearing reasons
    /// require the specific disposition their name promises.
    pub fn disposition_matches(self, disposition: Option<SanitizerDispositionV1>) -> bool {
        matches!(
            (self, disposition),
            (
                Self::SanitizerRejected,
                Some(SanitizerDispositionV1::Rejected)
            ) | (
                Self::SanitizerQuarantined,
                Some(SanitizerDispositionV1::Quarantined)
            ) | (
                Self::DuplicateObservation | Self::CanonicalPayloadRevision,
                Some(SanitizerDispositionV1::Accepted | SanitizerDispositionV1::Redacted)
            ) | (
                Self::BlankFrame
                    | Self::OutOfScope
                    | Self::MalformedFrame
                    | Self::OversizedFrame
                    | Self::UnknownVersion
                    | Self::UnsupportedFact
                    | Self::AdmissionRefused
                    | Self::ObservationIdentityCollision,
                None
            )
        )
    }
}

/// The wire form did not name a known [`ObservationCoverageReason`] variant.
///
/// Unknown wire text can originate in corrupt or legacy durable rows. Keep
/// only a fixed-size fingerprint so error and serialization boundaries cannot
/// disclose that text while still correlating repeated unknown values.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq, thiserror::Error)]
#[serde(transparent)]
#[error("unknown observation coverage reason: {fingerprint}")]
pub struct UnknownObservationCoverageReason {
    fingerprint: CursorAdvanceLedgerOpaqueValueHashV1,
}

impl UnknownObservationCoverageReason {
    /// Fixed-size identity for the unrecognized wire value.
    pub fn fingerprint(&self) -> &CursorAdvanceLedgerOpaqueValueHashV1 {
        &self.fingerprint
    }
}

impl TryFrom<&str> for ObservationCoverageReason {
    type Error = UnknownObservationCoverageReason;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "blank_frame" => Ok(Self::BlankFrame),
            "out_of_scope" => Ok(Self::OutOfScope),
            "malformed_frame" => Ok(Self::MalformedFrame),
            "oversized_frame" => Ok(Self::OversizedFrame),
            "unknown_version" => Ok(Self::UnknownVersion),
            "unsupported_fact" => Ok(Self::UnsupportedFact),
            "duplicate_observation" => Ok(Self::DuplicateObservation),
            "canonical_payload_revision" => Ok(Self::CanonicalPayloadRevision),
            "sanitizer_rejected" => Ok(Self::SanitizerRejected),
            "sanitizer_quarantined" => Ok(Self::SanitizerQuarantined),
            "admission_refused" => Ok(Self::AdmissionRefused),
            "observation_identity_collision" => Ok(Self::ObservationIdentityCollision),
            other => Err(UnknownObservationCoverageReason {
                fingerprint: CursorAdvanceLedgerOpaqueValueHashV1::for_raw_value(other),
            }),
        }
    }
}

pub type NonDurableFrameReason = ObservationCoverageReason;

/// Fixed-size opaque fingerprint of corrupt or legacy ledger text.
///
/// The error boundary may retain this fingerprint to correlate repeated bad
/// rows, but never retains the raw database value that produced it.
#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct CursorAdvanceLedgerOpaqueValueHashV1(String);

impl CursorAdvanceLedgerOpaqueValueHashV1 {
    fn for_raw_value(value: &str) -> Self {
        Self(
            tracedecay_domain::canonical_text::encode_tagged_lowercase_hex(
                "sha256:",
                &Sha256::digest(value.as_bytes()),
            ),
        )
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CursorAdvanceLedgerOpaqueValueHashV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for CursorAdvanceLedgerOpaqueValueHashV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        if tracedecay_domain::canonical_text::is_tagged_lowercase_hex(&value, "sha256:", 64) {
            Ok(Self(value))
        } else {
            Err(serde::de::Error::custom(
                "cursor-advance ledger opaque value hash must be sha256 lowercase hex",
            ))
        }
    }
}

/// Bounded reason identity exposed from an immutable cursor-advance ledger
/// row. Unknown database text is represented only by a fingerprint.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CursorAdvanceLedgerReasonV1 {
    Known(ObservationCoverageReason),
    Opaque {
        fingerprint: CursorAdvanceLedgerOpaqueValueHashV1,
    },
}

/// Bounded sanitization-receipt identity exposed from an immutable
/// cursor-advance ledger row. Any database text not proven by the canonical
/// sanitization-receipt authority is represented only by a fingerprint.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CursorAdvanceLedgerReceiptIdV1 {
    Absent,
    Known(SanitizationReceiptId),
    Opaque {
        fingerprint: CursorAdvanceLedgerOpaqueValueHashV1,
    },
}

/// Content-free identity of one immutable cursor-advance ledger row.
///
/// Canonical rows retain typed reasons and authority-proven receipt
/// identifiers. Corrupt, legacy, or unproven rows retain only fixed-size
/// opaque fingerprints; no raw ledger text can cross this error boundary.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CursorAdvanceLedgerIdentityV1 {
    reason: CursorAdvanceLedgerReasonV1,
    receipt_id: CursorAdvanceLedgerReceiptIdV1,
}

impl CursorAdvanceLedgerIdentityV1 {
    pub fn new(
        reason: ObservationCoverageReason,
        receipt_id: Option<SanitizationReceiptId>,
    ) -> Self {
        Self {
            reason: CursorAdvanceLedgerReasonV1::Known(reason),
            receipt_id: match receipt_id {
                Some(receipt_id) => CursorAdvanceLedgerReceiptIdV1::Known(receipt_id),
                None => CursorAdvanceLedgerReceiptIdV1::Absent,
            },
        }
    }

    /// Classify values read from the private SQL ledger without retaining
    /// their raw text in the outward-facing error DTO.
    ///
    /// A syntactically valid receipt identifier is not evidence that the
    /// ledger value is safe to expose. Callers may provide an authority
    /// receipt only after resolving it from the canonical receipt authority
    /// and verifying its identifier against the stored ledger value.
    pub fn from_stored_row_with_authority_receipt(
        reason: &str,
        receipt_id: Option<&str>,
        authority_receipt: Option<&SanitizationReceiptV1>,
    ) -> Self {
        Self {
            reason: match ObservationCoverageReason::try_from(reason) {
                Ok(reason) => CursorAdvanceLedgerReasonV1::Known(reason),
                Err(_) => CursorAdvanceLedgerReasonV1::Opaque {
                    fingerprint: CursorAdvanceLedgerOpaqueValueHashV1::for_raw_value(reason),
                },
            },
            receipt_id: match receipt_id {
                None => CursorAdvanceLedgerReceiptIdV1::Absent,
                Some(receipt_id) => match authority_receipt {
                    Some(authority_receipt)
                        if authority_receipt.receipt().receipt_id().as_str() == receipt_id =>
                    {
                        CursorAdvanceLedgerReceiptIdV1::Known(
                            authority_receipt.receipt().receipt_id().clone(),
                        )
                    }
                    _ => CursorAdvanceLedgerReceiptIdV1::Opaque {
                        fingerprint: CursorAdvanceLedgerOpaqueValueHashV1::for_raw_value(
                            receipt_id,
                        ),
                    },
                },
            },
        }
    }

    pub fn reason(&self) -> &CursorAdvanceLedgerReasonV1 {
        &self.reason
    }

    pub fn receipt_id(&self) -> &CursorAdvanceLedgerReceiptIdV1 {
        &self.receipt_id
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ObservationCoverageV1 {
    generation: ObservationSourceGenerationV1,
    ordering_domain: ObservationOrderingDomainV1,
    range: ObservationSourceRangeV1,
}

impl ObservationCoverageV1 {
    pub fn new(
        generation: ObservationSourceGenerationV1,
        ordering_domain: ObservationOrderingDomainV1,
        range: ObservationSourceRangeV1,
    ) -> Self {
        Self {
            generation,
            ordering_domain,
            range,
        }
    }

    pub fn generation(self) -> ObservationSourceGenerationV1 {
        self.generation
    }

    pub fn ordering_domain(self) -> ObservationOrderingDomainV1 {
        self.ordering_domain
    }

    pub fn range(self) -> ObservationSourceRangeV1 {
        self.range
    }
}

/// Permanent disagreement between an immutable cursor-advance ledger row and
/// a candidate advance over the same source coverage.
///
/// This exposes only the source/scope/coverage coordinate and row identities;
/// it intentionally omits every transcript locator and payload field.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CursorAdvanceLedgerDisagreementV1 {
    source: ObservationSourceIdentityV1,
    scope: ObservationScopeV1,
    coverage: ObservationCoverageV1,
    stored: CursorAdvanceLedgerIdentityV1,
    candidate: CursorAdvanceLedgerIdentityV1,
}

impl CursorAdvanceLedgerDisagreementV1 {
    pub fn new(
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        coverage: ObservationCoverageV1,
        stored: CursorAdvanceLedgerIdentityV1,
        candidate: CursorAdvanceLedgerIdentityV1,
    ) -> Self {
        Self {
            source,
            scope,
            coverage,
            stored,
            candidate,
        }
    }

    pub fn source(&self) -> &ObservationSourceIdentityV1 {
        &self.source
    }

    pub fn scope(&self) -> &ObservationScopeV1 {
        &self.scope
    }

    pub fn coverage(&self) -> ObservationCoverageV1 {
        self.coverage
    }

    pub fn stored(&self) -> &CursorAdvanceLedgerIdentityV1 {
        &self.stored
    }

    pub fn candidate(&self) -> &CursorAdvanceLedgerIdentityV1 {
        &self.candidate
    }
}

/// Validated exact-CAS cursor advance over fully processed non-durable evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationCursorAdvance {
    expected_cursor: Option<ObservationSourceCursorV1>,
    next_cursor: ObservationSourceCursorV1,
    covered: ObservationSourceRangeV1,
    reason: ObservationCoverageReason,
    sanitization_receipt: Option<SanitizationReceiptV1>,
}

impl ObservationCursorAdvance {
    pub fn new(
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        expected_cursor: Option<ObservationSourceCursorV1>,
        covered: ObservationSourceRangeV1,
        reason: ObservationCoverageReason,
    ) -> ObservationStoreResult<Self> {
        Self::build(
            source,
            scope,
            generation,
            ObservationOrderingDomainV1::FileBytes,
            expected_cursor,
            covered,
            reason,
            None,
        )
    }

    pub fn for_ordering(
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        ordering_domain: ObservationOrderingDomainV1,
        expected_cursor: Option<ObservationSourceCursorV1>,
        covered: ObservationSourceRangeV1,
        reason: ObservationCoverageReason,
    ) -> ObservationStoreResult<Self> {
        Self::build(
            source,
            scope,
            generation,
            ordering_domain,
            expected_cursor,
            covered,
            reason,
            None,
        )
    }

    pub fn new_with_sanitization_receipt(
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        expected_cursor: Option<ObservationSourceCursorV1>,
        covered: ObservationSourceRangeV1,
        reason: ObservationCoverageReason,
        sanitization_receipt: SanitizationReceiptV1,
    ) -> ObservationStoreResult<Self> {
        Self::build(
            source,
            scope,
            generation,
            ObservationOrderingDomainV1::FileBytes,
            expected_cursor,
            covered,
            reason,
            Some(sanitization_receipt),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn for_ordering_with_sanitization_receipt(
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        ordering_domain: ObservationOrderingDomainV1,
        expected_cursor: Option<ObservationSourceCursorV1>,
        covered: ObservationSourceRangeV1,
        reason: ObservationCoverageReason,
        sanitization_receipt: SanitizationReceiptV1,
    ) -> ObservationStoreResult<Self> {
        Self::build(
            source,
            scope,
            generation,
            ordering_domain,
            expected_cursor,
            covered,
            reason,
            Some(sanitization_receipt),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        source: ObservationSourceIdentityV1,
        scope: ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
        ordering_domain: ObservationOrderingDomainV1,
        expected_cursor: Option<ObservationSourceCursorV1>,
        covered: ObservationSourceRangeV1,
        reason: ObservationCoverageReason,
        sanitization_receipt: Option<SanitizationReceiptV1>,
    ) -> ObservationStoreResult<Self> {
        let receipt_matches_reason = reason.disposition_matches(
            sanitization_receipt
                .as_ref()
                .map(SanitizationReceiptV1::disposition),
        );
        if !receipt_matches_reason {
            return Err(ObservationStoreError::CursorSanitizationReceiptMismatch);
        }
        let next_cursor = ObservationSourceCursorV1::for_ordering(
            source,
            scope,
            generation,
            ordering_domain,
            covered.end(),
        )
        .map_err(ObservationStoreError::Contract)?;
        if !cursor_transition_covers(expected_cursor.as_ref(), &next_cursor, covered) {
            return Err(ObservationStoreError::CursorCoverageMismatch);
        }
        Ok(Self {
            expected_cursor,
            next_cursor,
            covered,
            reason,
            sanitization_receipt,
        })
    }

    pub fn expected_cursor(&self) -> Option<&ObservationSourceCursorV1> {
        self.expected_cursor.as_ref()
    }

    pub fn next_cursor(&self) -> &ObservationSourceCursorV1 {
        &self.next_cursor
    }

    pub fn covered(&self) -> ObservationSourceRangeV1 {
        self.covered
    }

    pub fn coverage(&self) -> ObservationCoverageV1 {
        ObservationCoverageV1::new(
            self.next_cursor.generation(),
            self.next_cursor.ordering_domain(),
            self.covered,
        )
    }

    pub fn reason(&self) -> ObservationCoverageReason {
        self.reason
    }

    pub fn sanitization_receipt(&self) -> Option<&SanitizationReceiptV1> {
        self.sanitization_receipt.as_ref()
    }

    pub fn ledger_identity(&self) -> CursorAdvanceLedgerIdentityV1 {
        CursorAdvanceLedgerIdentityV1::new(
            self.reason,
            self.sanitization_receipt
                .as_ref()
                .map(|receipt| receipt.receipt().receipt_id().clone()),
        )
    }

    #[must_use]
    pub fn with_resume_checkpoint(mut self, file_identity: u64, resume_fingerprint: u64) -> Self {
        self.next_cursor = self
            .next_cursor
            .with_resume_checkpoint(file_identity, resume_fingerprint);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorAdvanceOutcome {
    Committed,
    ExactDuplicate,
}

/// Stable receipt for committed observation evidence and its authoritative cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationCommitReceipt {
    sequence: u64,
    observation: Box<DurableObservationV1>,
    committed_cursor: ObservationSourceCursorV1,
    retrieval_anchor: Box<RetrievalAnchorRecordV2>,
    projection_generation: ProjectionGenerationId,
    repository_provenance: RepositoryProvenanceAttachmentV1,
}

impl ObservationCommitReceipt {
    pub fn new(
        sequence: u64,
        observation: DurableObservationV1,
        committed_cursor: ObservationSourceCursorV1,
        retrieval_anchor: RetrievalAnchorRecordV2,
        projection_generation: ProjectionGenerationId,
    ) -> ObservationStoreResult<Self> {
        validate_retrieval_anchor_binding(&observation, &retrieval_anchor, &projection_generation)?;
        Ok(Self {
            sequence,
            observation: Box::new(observation),
            committed_cursor,
            retrieval_anchor: Box::new(retrieval_anchor),
            projection_generation,
            repository_provenance: RepositoryProvenanceAttachmentV1::unavailable(),
        })
    }

    pub fn with_repository_provenance_attachment(
        mut self,
        repository_provenance: RepositoryProvenanceAttachmentV1,
    ) -> ObservationStoreResult<Self> {
        repository_provenance
            .validate_for_observation(&self.observation, &self.projection_generation)?;
        self.repository_provenance = repository_provenance;
        Ok(self)
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn observation(&self) -> &DurableObservationV1 {
        self.observation.as_ref()
    }

    pub fn sanitization_receipt(&self) -> &SanitizationReceiptV1 {
        self.observation.receipt()
    }

    pub fn committed_cursor(&self) -> &ObservationSourceCursorV1 {
        &self.committed_cursor
    }

    pub fn retrieval_anchor(&self) -> &RetrievalAnchorRecordV2 {
        self.retrieval_anchor.as_ref()
    }

    pub fn retrieval_anchor_id(&self) -> &RetrievalAnchorId {
        self.retrieval_anchor.anchor_id()
    }

    pub fn projection_generation(&self) -> &ProjectionGenerationId {
        &self.projection_generation
    }

    pub fn repository_provenance_attachment(&self) -> &RepositoryProvenanceAttachmentV1 {
        &self.repository_provenance
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservationPersistOutcome {
    Committed(ObservationCommitReceipt),
    ExactDuplicate(ObservationCommitReceipt),
    CoveredDuplicate(ObservationCommitReceipt),
}

impl ObservationPersistOutcome {
    pub fn receipt(&self) -> &ObservationCommitReceipt {
        match self {
            Self::Committed(receipt)
            | Self::ExactDuplicate(receipt)
            | Self::CoveredDuplicate(receipt) => receipt,
        }
    }
}

/// One immutable observation in authoritative ingestion order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredObservation {
    commit_receipt: ObservationCommitReceipt,
    projection_status: ObservationProjectionStatus,
}

impl StoredObservation {
    pub fn new(
        sequence: u64,
        observation: DurableObservationV1,
        committed_cursor: ObservationSourceCursorV1,
        retrieval_anchor: RetrievalAnchorRecordV2,
        projection_generation: ProjectionGenerationId,
        projection_status: ObservationProjectionStatus,
    ) -> ObservationStoreResult<Self> {
        Ok(Self::from_commit_receipt(
            ObservationCommitReceipt::new(
                sequence,
                observation,
                committed_cursor,
                retrieval_anchor,
                projection_generation,
            )?,
            projection_status,
        ))
    }

    pub fn from_commit_receipt(
        commit_receipt: ObservationCommitReceipt,
        projection_status: ObservationProjectionStatus,
    ) -> Self {
        Self {
            commit_receipt,
            projection_status,
        }
    }

    pub fn commit_receipt(&self) -> &ObservationCommitReceipt {
        &self.commit_receipt
    }

    pub fn sequence(&self) -> u64 {
        self.commit_receipt.sequence()
    }

    pub fn observation(&self) -> &DurableObservationV1 {
        self.commit_receipt.observation()
    }

    pub fn sanitization_receipt(&self) -> &SanitizationReceiptV1 {
        self.commit_receipt.sanitization_receipt()
    }

    pub fn committed_cursor(&self) -> &ObservationSourceCursorV1 {
        self.commit_receipt.committed_cursor()
    }

    pub fn repository_provenance_attachment(&self) -> &RepositoryProvenanceAttachmentV1 {
        self.commit_receipt.repository_provenance_attachment()
    }

    pub fn retrieval_anchor(&self) -> &RetrievalAnchorRecordV2 {
        self.commit_receipt.retrieval_anchor()
    }

    pub fn retrieval_anchor_id(&self) -> &RetrievalAnchorId {
        self.commit_receipt.retrieval_anchor_id()
    }

    pub fn projection_generation(&self) -> &ProjectionGenerationId {
        self.commit_receipt.projection_generation()
    }

    pub fn projection_status(&self) -> ObservationProjectionStatus {
        self.projection_status
    }
}

/// Ordered result from one bounded observation-admission transaction.
///
/// The optional stored snapshot is produced by the same store-owned bulk
/// operation as the persistence outcome. Callers must not replace it with
/// per-record post-commit reads, which would serialize a batch on the
/// readback path and can observe a later projection state instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationBatchPersistOutcome {
    outcome: ObservationPersistOutcome,
    stored: Option<StoredObservation>,
}

impl ObservationBatchPersistOutcome {
    #[hotpath::skip]
    pub const fn new(
        outcome: ObservationPersistOutcome,
        stored: Option<StoredObservation>,
    ) -> Self {
        Self { outcome, stored }
    }

    #[hotpath::skip]
    pub const fn outcome(&self) -> &ObservationPersistOutcome {
        &self.outcome
    }

    #[hotpath::skip]
    pub const fn stored(&self) -> Option<&StoredObservation> {
        self.stored.as_ref()
    }

    pub fn into_parts(self) -> (ObservationPersistOutcome, Option<StoredObservation>) {
        (self.outcome, self.stored)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationProjectionStatus {
    Queued,
    NotQueued,
}

/// Why one bounded observation batch must be retried as scalar operations.
///
/// These causes describe only collisions between not-yet-durable members of
/// the current batch. Collisions against durable evidence remain terminal
/// store errors and must never be retried as scalar writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationBatchFallbackCause {
    IntraBatchIdentityCollision,
    IntraBatchSanitizationReceiptCollision,
    IntraBatchRetrievalAnchorAliasCollision,
    /// A collision path that must compare-and-set against the durable source
    /// frontier, while an earlier member of this batch has not made that
    /// frontier durable yet.
    IntraBatchDurableFrontier,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObservationReplayRequest {
    after_sequence: u64,
    limit: usize,
}

impl ObservationReplayRequest {
    pub fn new(after_sequence: u64, limit: usize) -> ObservationStoreResult<Self> {
        if limit == 0 || limit > MAX_REPLAY_LIMIT {
            return Err(ObservationStoreError::InvalidReplayLimit {
                limit,
                max: MAX_REPLAY_LIMIT,
            });
        }
        Ok(Self {
            after_sequence,
            limit,
        })
    }

    pub fn after_sequence(self) -> u64 {
        self.after_sequence
    }

    pub fn limit(self) -> usize {
        self.limit
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ObservationStoreError {
    #[error("observation batch requires scalar fallback: {cause:?}")]
    BatchRequiresScalarFallback {
        cause: ObservationBatchFallbackCause,
    },
    #[error("observation cursor does not match its source evidence")]
    CursorObservationMismatch,
    #[error("covered source evidence is not contiguous with the expected cursor")]
    CursorCoverageMismatch,
    #[error("source cursor conflict: expected {expected:?}, found {actual:?}")]
    CursorConflict {
        expected: Box<Option<ObservationSourceCursorV1>>,
        actual: Box<Option<ObservationSourceCursorV1>>,
    },
    #[error("source cursor advance receipt collided with different contents")]
    CursorAdvanceCollision,
    #[error("source cursor advance ledger disagrees with an immutable coverage record")]
    CursorAdvanceLedgerDisagreement {
        disagreement: Box<CursorAdvanceLedgerDisagreementV1>,
    },
    #[error("source cursor advance reason disagrees with its sanitization receipt")]
    CursorSanitizationReceiptMismatch,
    #[error(
        "observation {observation_id:?} collided: existing digest {existing_digest:?}, candidate digest {candidate_digest:?}"
    )]
    ObservationCollision {
        observation_id: Box<CanonicalObservationIdV1>,
        existing_digest: Box<PayloadDigestV1>,
        candidate_digest: Box<PayloadDigestV1>,
        outcome: ObservationCollisionOutcomeV1,
    },
    #[error("sanitization receipt identifier collided with different contents")]
    SanitizationReceiptCollision,
    #[error("retrieval anchor does not target the persisted observation")]
    RetrievalAnchorObservationMismatch,
    #[error("retrieval anchor owner does not match the persisted observation scope")]
    RetrievalAnchorOwnerMismatch,
    #[error("retrieval anchor source generation does not match the persisted observation")]
    RetrievalAnchorSourceGenerationMismatch,
    #[error("retrieval anchor source lineage does not match the persisted observation")]
    RetrievalAnchorSourceLineageMismatch,
    #[error("retrieval anchor projection generation does not match the store write")]
    RetrievalAnchorProjectionGenerationMismatch,
    #[error("retrieval anchor identity collided with different authoritative contents")]
    RetrievalAnchorCollision,
    #[error("retrieval anchor contract validation failed")]
    RetrievalAnchorContract(#[source] DomainError),
    #[error("repository provenance availability and retrieval anchor disagree")]
    RepositoryProvenanceAvailabilityMismatch,
    #[error("repository provenance does not bind to the observation authority")]
    RepositoryProvenanceBindingMismatch,
    #[error("repository provenance contract validation failed")]
    RepositoryProvenanceContract(#[source] DomainError),
    #[error(
        "retrieval anchor alias {alias:?} collided between existing anchor {existing_anchor_id:?} and candidate anchor {candidate_anchor_id:?}"
    )]
    RetrievalAnchorAliasCollision {
        alias: Box<NativeAliasV2>,
        existing_anchor_id: Box<RetrievalAnchorId>,
        candidate_anchor_id: Box<RetrievalAnchorId>,
    },
    #[error("replay limit {limit} must be between 1 and {max}")]
    InvalidReplayLimit { limit: usize, max: usize },
    #[error("observation contract validation failed")]
    Contract(#[source] ObservationContractError),
    #[error("observation storage operation {operation} failed")]
    Storage {
        operation: &'static str,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
}

pub type ObservationStoreResult<T> = Result<T, ObservationStoreError>;

/// Write-only authority for sanitized, anchor-bound observation capture.
///
/// The accepted request is deliberately [`AnchoredObservationWrite`], so
/// provider scanners cannot bypass sanitization or mint a second write path.
pub trait ObservationCaptureSink: Send + Sync {
    fn persist_admitted_observation(
        &self,
        write: AnchoredObservationWrite,
    ) -> impl Future<Output = ObservationStoreResult<ObservationPersistOutcome>> + Send;
}

/// Exact-CAS cursor authority used by provider capture coordinators.
pub trait ObservationCursorPort: Send + Sync {
    fn read_source_cursor(
        &self,
        source: &ObservationSourceIdentityV1,
        scope: &ObservationScopeV1,
    ) -> impl Future<Output = ObservationStoreResult<Option<ObservationSourceCursorV1>>> + Send;

    fn advance_admitted_source_cursor(
        &self,
        advance: ObservationCursorAdvance,
    ) -> impl Future<Output = ObservationStoreResult<CursorAdvanceOutcome>> + Send;
}

/// Read authority required by capture admission and bounded replay.
pub trait ObservationAdmissionPort: Send + Sync {
    fn read_admitted_observation(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> impl Future<Output = ObservationStoreResult<Option<StoredObservation>>> + Send;

    fn replay_admitted_observations(
        &self,
        request: ObservationReplayRequest,
    ) -> impl Future<Output = ObservationStoreResult<Vec<StoredObservation>>> + Send;
}

/// Authoritative persistence boundary for sanitized observations and their stable anchors.
pub trait ObservationStore: Send + Sync {
    fn persist_observation(
        &self,
        write: AnchoredObservationWrite,
    ) -> impl Future<Output = ObservationStoreResult<ObservationPersistOutcome>> + Send;

    /// Persist a bounded admission batch through one store-owned writer
    /// transaction plus an ordered, bounded post-submit snapshot. An empty
    /// `writes` returns an empty outcome list and must not mint a success that
    /// skipped cursor, collision, or file-identity authority. Implementations
    /// cannot default this to N one-record transactions or point reads; test
    /// fakes have to name the batch contract.
    fn persist_observations(
        &self,
        writes: Vec<AnchoredObservationWrite>,
    ) -> impl Future<Output = ObservationStoreResult<Vec<ObservationBatchPersistOutcome>>> + Send;

    fn get_source_cursor(
        &self,
        source: &ObservationSourceIdentityV1,
        scope: &ObservationScopeV1,
    ) -> impl Future<Output = ObservationStoreResult<Option<ObservationSourceCursorV1>>> + Send;

    fn advance_source_cursor(
        &self,
        advance: ObservationCursorAdvance,
    ) -> impl Future<Output = ObservationStoreResult<CursorAdvanceOutcome>> + Send;

    fn get_observation(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> impl Future<Output = ObservationStoreResult<Option<StoredObservation>>> + Send;

    fn replay_observations(
        &self,
        request: ObservationReplayRequest,
    ) -> impl Future<Output = ObservationStoreResult<Vec<StoredObservation>>> + Send;
}

impl<T> ObservationCaptureSink for T
where
    T: ObservationStore + ?Sized,
{
    #[hotpath::skip]
    async fn persist_admitted_observation(
        &self,
        write: AnchoredObservationWrite,
    ) -> ObservationStoreResult<ObservationPersistOutcome> {
        self.persist_observation(write).await
    }
}

impl<T> ObservationCursorPort for T
where
    T: ObservationStore + ?Sized,
{
    #[hotpath::skip]
    async fn read_source_cursor(
        &self,
        source: &ObservationSourceIdentityV1,
        scope: &ObservationScopeV1,
    ) -> ObservationStoreResult<Option<ObservationSourceCursorV1>> {
        self.get_source_cursor(source, scope).await
    }

    #[hotpath::skip]
    async fn advance_admitted_source_cursor(
        &self,
        advance: ObservationCursorAdvance,
    ) -> ObservationStoreResult<CursorAdvanceOutcome> {
        self.advance_source_cursor(advance).await
    }
}

impl<T> ObservationAdmissionPort for T
where
    T: ObservationStore + ?Sized,
{
    #[hotpath::skip]
    async fn read_admitted_observation(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> ObservationStoreResult<Option<StoredObservation>> {
        self.get_observation(observation_id).await
    }

    #[hotpath::skip]
    async fn replay_admitted_observations(
        &self,
        request: ObservationReplayRequest,
    ) -> ObservationStoreResult<Vec<StoredObservation>> {
        self.replay_observations(request).await
    }
}

#[cfg(test)]
mod tests;

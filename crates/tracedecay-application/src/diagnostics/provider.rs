use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use tracedecay_domain::feedback::ProviderEvaluationStateV1;
use tracedecay_domain::{
    CodeGenerationId, ComponentVersion, ContentDigest, FileOccurrenceId, GenerationDiagnosticV1,
    HostInstanceId, LanguageDescriptorRevision, LanguageId, ManifestDigest, ProviderId,
    RetrievalAnchorId, SessionId, UtcMicros, canonical_sha256,
};
use tracedecay_policy::analyzer::{
    AnalyzerAdmissionDispositionV1, AnalyzerAdmissionInputV1, AnalyzerAdmissionReasonV1,
    AnalyzerAdmissionSnapshotV1,
};
use tracedecay_tool_catalog::CapabilityId;

use crate::ResolvedScope;
use crate::error::ApplicationContractError;
use crate::policy::{PolicyEvaluationContextV1, PolicyEvaluatorCompositionV1};
use crate::result::{CoverageCompleteness, FreshnessState, PolicyDecisionRef};

const PROVIDER_IDENTITY_DIGEST_DOMAIN: &str = "tracedecay.application.provider-identity.v1";

/// Exact clean generation or isolated client/session overlay identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ProviderSourceIdentity {
    CleanGeneration {
        generation: CodeGenerationId,
    },
    SessionOverlay {
        session_id: SessionId,
        client_id: HostInstanceId,
        document_version: u64,
        overlay_digest: ManifestDigest,
    },
}

impl ProviderSourceIdentity {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        match self {
            Self::CleanGeneration { generation } => generation.validate()?,
            Self::SessionOverlay {
                session_id,
                client_id,
                document_version,
                overlay_digest,
            } => {
                session_id.validate()?;
                client_id.validate()?;
                if *document_version == 0 {
                    return Err(ApplicationContractError::ZeroValue {
                        field: "provider overlay document version",
                    });
                }
                overlay_digest.validate()?;
            }
        }
        Ok(())
    }

    pub const fn is_overlay(&self) -> bool {
        matches!(self, Self::SessionOverlay { .. })
    }

    pub fn clean_generation(&self) -> Option<&CodeGenerationId> {
        match self {
            Self::CleanGeneration { generation } => Some(generation),
            Self::SessionOverlay { .. } => None,
        }
    }
}

/// Exact document attachment for a provider result.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderDocumentIdentity {
    pub file: FileOccurrenceId,
    pub content_digest: ContentDigest,
    pub document_version: Option<u64>,
}

impl ProviderDocumentIdentity {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        self.file.validate()?;
        self.content_digest.validate()?;
        if self.document_version == Some(0) {
            return Err(ApplicationContractError::ZeroValue {
                field: "provider document version",
            });
        }
        Ok(())
    }
}

/// Cataloged producer and language-descriptor identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticProviderDescriptor {
    pub provider: ProviderId,
    pub analyzer_revision: ComponentVersion,
    pub language: LanguageId,
    pub language_descriptor_revision: LanguageDescriptorRevision,
}

impl DiagnosticProviderDescriptor {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        self.provider.validate()?;
        self.analyzer_revision.validate()?;
        self.language.validate()?;
        self.language_descriptor_revision.validate()?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderFreshness {
    pub state: FreshnessState,
    pub observed_at: UtcMicros,
}

impl ProviderFreshness {
    pub fn current(observed_at: UtcMicros) -> Self {
        Self {
            state: FreshnessState::Current,
            observed_at,
        }
    }
}

/// Provider coverage remains distinct from a zero-diagnostic result.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderCoverage {
    pub requested: u64,
    pub returned: u64,
    pub completeness: CoverageCompleteness,
}

impl ProviderCoverage {
    pub fn complete(requested: u64, returned: u64) -> Self {
        Self {
            requested,
            returned,
            completeness: CoverageCompleteness::Complete,
        }
    }

    fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.returned > self.requested
            || (self.completeness == CoverageCompleteness::Complete && self.requested == 0)
        {
            return Err(ApplicationContractError::InvalidRange {
                field: "provider coverage",
            });
        }
        Ok(())
    }
}

/// Claim origin remains separate from caller authority.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProviderOrigin {
    ConfiguredAnalyzer,
    CodeIntelligence,
    AuthorizedNativeHost,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderProvenance {
    pub origin: ProviderOrigin,
    pub anchor: Option<RetrievalAnchorId>,
}

impl ProviderProvenance {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        if let Some(anchor) = &self.anchor {
            anchor.validate()?;
        }
        Ok(())
    }
}

/// Revision/digest pair owned by configuration or another authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RevisionDigest {
    pub revision: ComponentVersion,
    pub digest: ManifestDigest,
}

impl RevisionDigest {
    fn validate(&self) -> Result<(), ApplicationContractError> {
        self.revision.validate()?;
        self.digest.validate()?;
        Ok(())
    }
}

/// Complete canonical provider-result identity tuple.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticProviderIdentityParts {
    pub scope: ResolvedScope,
    pub source: ProviderSourceIdentity,
    pub document: ProviderDocumentIdentity,
    pub producer: DiagnosticProviderDescriptor,
    pub requested_capability: CapabilityId,
    pub freshness: ProviderFreshness,
    pub coverage: ProviderCoverage,
    pub provenance: ProviderProvenance,
    pub configuration: RevisionDigest,
    pub policy: PolicyDecisionRef,
}

/// Canonical identity for every provider result. Plan 35 may cache or execute
/// providers behind this shape, but cannot redefine its identity semantics.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticProviderIdentity {
    pub scope: ResolvedScope,
    pub source: ProviderSourceIdentity,
    pub document: ProviderDocumentIdentity,
    pub producer: DiagnosticProviderDescriptor,
    pub requested_capability: CapabilityId,
    pub freshness: ProviderFreshness,
    pub coverage: ProviderCoverage,
    pub provenance: ProviderProvenance,
    pub configuration: RevisionDigest,
    pub policy: PolicyDecisionRef,
}

impl DiagnosticProviderIdentity {
    pub fn new(parts: DiagnosticProviderIdentityParts) -> Result<Self, ApplicationContractError> {
        let identity = Self {
            scope: parts.scope,
            source: parts.source,
            document: parts.document,
            producer: parts.producer,
            requested_capability: parts.requested_capability,
            freshness: parts.freshness,
            coverage: parts.coverage,
            provenance: parts.provenance,
            configuration: parts.configuration,
            policy: parts.policy,
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.scope.validate()?;
        self.source.validate()?;
        self.document.validate()?;
        self.producer.validate()?;
        self.coverage.validate()?;
        self.provenance.validate()?;
        self.configuration.validate()?;
        self.policy.validate()?;
        match (&self.source, self.document.document_version) {
            (
                ProviderSourceIdentity::SessionOverlay {
                    document_version, ..
                },
                Some(version),
            ) if *document_version == version => {}
            (ProviderSourceIdentity::SessionOverlay { .. }, _) => {
                return Err(ApplicationContractError::Inconsistent {
                    field: "provider overlay document version",
                });
            }
            _ => {}
        }
        Ok(())
    }

    pub fn compute_digest(&self) -> Result<ManifestDigest, ApplicationContractError> {
        self.validate()?;
        Ok(canonical_sha256(&(PROVIDER_IDENTITY_DIGEST_DOMAIN, self))?)
    }

    pub const fn is_overlay(&self) -> bool {
        self.source.is_overlay()
    }
}

/// One provider identity pinned to the exact analyzer-policy snapshot that
/// admitted (or declined) it. This is construction evidence only: it neither
/// starts an analyzer nor changes its lifecycle.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnalyzerAdmittedDiagnosticProviderV1 {
    identity: DiagnosticProviderIdentity,
    admission_input: AnalyzerAdmissionInputV1,
    admission_snapshot: AnalyzerAdmissionSnapshotV1,
}

impl AnalyzerAdmittedDiagnosticProviderV1 {
    /// Evaluates the approved analyzer policy directly from the current,
    /// exact Plan-20 application snapshot.
    pub fn evaluate_current_configuration_snapshot(
        composition: &PolicyEvaluatorCompositionV1,
        context: &PolicyEvaluationContextV1,
        identity: DiagnosticProviderIdentity,
        admission_input: AnalyzerAdmissionInputV1,
    ) -> Result<Self, ApplicationContractError> {
        if &identity.scope != context.scope() {
            return Err(ApplicationContractError::Inconsistent {
                field: "analyzer policy application scope",
            });
        }
        let evaluation = composition.admit_analyzer(context, &admission_input)?;
        Self::from_configuration_admission_snapshot(identity, admission_input, evaluation.decision)
    }

    /// Constructs only from the immutable Plan-20 configuration and Plan-35
    /// admission snapshot already selected by their owning daemon authorities.
    /// This application contract never resolves configuration, selects an
    /// executable, evaluates a fallback, starts an analyzer, or synthesizes a
    /// replacement decision.
    pub fn from_configuration_admission_snapshot(
        identity: DiagnosticProviderIdentity,
        admission_input: AnalyzerAdmissionInputV1,
        admission_snapshot: AnalyzerAdmissionSnapshotV1,
    ) -> Result<Self, ApplicationContractError> {
        let provider = Self {
            identity,
            admission_input,
            admission_snapshot,
        };
        provider.validate()?;
        Ok(provider)
    }

    pub fn identity(&self) -> &DiagnosticProviderIdentity {
        &self.identity
    }

    pub fn admission_input(&self) -> &AnalyzerAdmissionInputV1 {
        &self.admission_input
    }

    pub fn admission_snapshot(&self) -> &AnalyzerAdmissionSnapshotV1 {
        &self.admission_snapshot
    }

    /// True when the same immutable analyzer-policy admission covers a
    /// generation/document-exact provider identity for a later request.
    ///
    /// Source, document, observation time, and coverage are request evidence.
    /// Analyzer selection, scope, producer, configuration, policy, capability,
    /// and provenance remain pinned to the admission snapshot.
    pub fn admits_identity(&self, identity: &DiagnosticProviderIdentity) -> bool {
        identity.validate().is_ok()
            && self.validate().is_ok()
            && identity.scope == self.identity.scope
            && identity.source.is_overlay() == self.identity.source.is_overlay()
            && identity.producer == self.identity.producer
            && identity.requested_capability == self.identity.requested_capability
            && identity.provenance == self.identity.provenance
            && identity.configuration == self.identity.configuration
            && identity.policy == self.identity.policy
            && identity.freshness.state == self.identity.freshness.state
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.identity.validate()?;
        if !self.admission_snapshot.is_bound_to(&self.admission_input)
            || self.identity.requested_capability.as_str()
                != self.admission_input.requested_capability.as_str()
            || self.identity.producer.language.as_str() != self.admission_input.language_id.as_str()
            || self.identity.configuration.digest != self.admission_input.configuration_digest
            || self.identity.policy.revision != self.admission_input.policy_revision
            || self.identity.policy.digest != self.admission_input.policy_digest
            || self.identity.policy.revision != self.admission_snapshot.decision.policy_revision
            || self.identity.policy.digest != self.admission_snapshot.decision.policy_digest
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "analyzer-admitted diagnostic provider",
            });
        }
        let admission_reports_stale = self.admission_snapshot.decision.disposition
            == AnalyzerAdmissionDispositionV1::Indeterminate
            && self
                .admission_snapshot
                .decision
                .ordered_reason_codes
                .contains(&AnalyzerAdmissionReasonV1::CandidateStale);
        if admission_reports_stale && self.identity.freshness.state != FreshnessState::Stale {
            return Err(ApplicationContractError::Inconsistent {
                field: "analyzer-admitted diagnostic provider freshness",
            });
        }
        Ok(())
    }

    /// Maps an immutable admission snapshot into the canonical provider-state
    /// taxonomy without treating denial, stale availability, or uncertainty
    /// as an empty clean result.
    pub fn state(&self) -> DiagnosticProviderState {
        debug_assert!(self.validate().is_ok());
        match self.admission_snapshot.decision.disposition {
            AnalyzerAdmissionDispositionV1::Allow
                if self.identity.freshness.state == FreshnessState::Stale =>
            {
                DiagnosticProviderState::Stale
            }
            AnalyzerAdmissionDispositionV1::Allow
                if self.identity.coverage.completeness != CoverageCompleteness::Complete =>
            {
                DiagnosticProviderState::Partial
            }
            AnalyzerAdmissionDispositionV1::Allow => DiagnosticProviderState::SupportedComplete,
            AnalyzerAdmissionDispositionV1::Deny
            | AnalyzerAdmissionDispositionV1::NotApplicable => DiagnosticProviderState::Unsupported,
            AnalyzerAdmissionDispositionV1::Indeterminate
                if self
                    .admission_snapshot
                    .decision
                    .ordered_reason_codes
                    .contains(&AnalyzerAdmissionReasonV1::CandidateStale) =>
            {
                DiagnosticProviderState::Stale
            }
            AnalyzerAdmissionDispositionV1::Indeterminate => DiagnosticProviderState::Unavailable,
        }
    }
}

/// Explicit provider completion state. Unsupported, absent, stale, and
/// partial values cannot collapse into a clean empty diagnostic result.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticProviderState {
    SupportedComplete,
    Unsupported,
    Absent,
    Indexing,
    Stale,
    Cancelled,
    TimedOut,
    Failed,
    Partial,
    Unavailable,
}

impl DiagnosticProviderState {
    /// Feedback cycles consume the one canonical provider-state taxonomy
    /// rather than inventing a diagnostic-specific empty-result convention.
    pub const fn feedback_state(self) -> ProviderEvaluationStateV1 {
        match self {
            Self::SupportedComplete => ProviderEvaluationStateV1::SupportedCompletedComplete,
            Self::Unsupported => ProviderEvaluationStateV1::Unsupported,
            Self::Absent => ProviderEvaluationStateV1::Absent,
            Self::Indexing => ProviderEvaluationStateV1::Indexing,
            Self::Stale => ProviderEvaluationStateV1::Stale,
            Self::Cancelled => ProviderEvaluationStateV1::Cancelled,
            Self::TimedOut => ProviderEvaluationStateV1::TimedOut,
            Self::Failed => ProviderEvaluationStateV1::Failed,
            Self::Partial => ProviderEvaluationStateV1::Partial,
            Self::Unavailable => ProviderEvaluationStateV1::Unavailable,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticProviderResult<T> {
    pub identity: DiagnosticProviderIdentity,
    pub state: DiagnosticProviderState,
    pub payload: Option<T>,
}

impl<T> DiagnosticProviderResult<T> {
    pub fn new(
        identity: DiagnosticProviderIdentity,
        state: DiagnosticProviderState,
        payload: Option<T>,
    ) -> Result<Self, ApplicationContractError> {
        let result = Self {
            identity,
            state,
            payload,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.identity.validate()?;
        let payload_is_consistent = match self.state {
            DiagnosticProviderState::SupportedComplete => self.payload.is_some(),
            DiagnosticProviderState::Unsupported
            | DiagnosticProviderState::Absent
            | DiagnosticProviderState::Indexing
            | DiagnosticProviderState::Stale
            | DiagnosticProviderState::Unavailable => self.payload.is_none(),
            DiagnosticProviderState::Cancelled
            | DiagnosticProviderState::TimedOut
            | DiagnosticProviderState::Failed
            | DiagnosticProviderState::Partial => true,
        };
        if !payload_is_consistent {
            return Err(ApplicationContractError::Inconsistent {
                field: "diagnostic provider payload state",
            });
        }
        if self.state == DiagnosticProviderState::SupportedComplete
            && (self.identity.freshness.state != FreshnessState::Current
                || self.identity.coverage.completeness != CoverageCompleteness::Complete)
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "complete diagnostic provider coverage",
            });
        }
        if self.state == DiagnosticProviderState::Partial
            && self.identity.coverage.completeness == CoverageCompleteness::Complete
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "partial diagnostic provider coverage",
            });
        }
        if self.state == DiagnosticProviderState::Stale
            && self.identity.freshness.state == FreshnessState::Current
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "stale diagnostic provider freshness",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CurrentDiagnosticsRequest {
    pub identity: DiagnosticProviderIdentity,
}

impl CurrentDiagnosticsRequest {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.identity.validate()
    }
}

/// Exact historical clean-generation read for baseline classification. The
/// requested provider remains bound to the current generation while
/// `generation` names the immutable comparison generation; overlays are
/// structurally rejected before any durable history read.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationDiagnosticHistoryRequest {
    pub identity: DiagnosticProviderIdentity,
    pub generation: CodeGenerationId,
    pub file: FileOccurrenceId,
}

impl GenerationDiagnosticHistoryRequest {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.identity.validate()?;
        self.generation.validate()?;
        self.file.validate()?;
        if self.identity.is_overlay() {
            return Err(ApplicationContractError::Inconsistent {
                field: "overlay diagnostic history request",
            });
        }
        Ok(())
    }
}

/// Transport-neutral provider port. Implementations are owned by future
/// analyzer/runtime packets and are not supplied by this crate.
pub type DiagnosticProviderFuture<'a, T> =
    Pin<Box<dyn Future<Output = DiagnosticProviderResult<T>> + Send + 'a>>;

pub trait DiagnosticProviderPort {
    fn current_diagnostics<'a>(
        &'a self,
        context: &'a crate::RequestContext,
        request: &'a CurrentDiagnosticsRequest,
    ) -> DiagnosticProviderFuture<'a, Vec<GenerationDiagnosticV1>>;
}

/// Read-only, generation-bound diagnostic history port. It is deliberately
/// separate from current provider execution so a feedback adapter can reuse
/// the diagnostic store without inventing a feedback-local history store.
pub trait GenerationDiagnosticHistoryPort {
    fn diagnostics_for_generation<'a>(
        &'a self,
        context: &'a crate::RequestContext,
        request: &'a GenerationDiagnosticHistoryRequest,
    ) -> DiagnosticProviderFuture<'a, Vec<GenerationDiagnosticV1>>;
}

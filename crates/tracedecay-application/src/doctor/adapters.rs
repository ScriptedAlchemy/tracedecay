//! Doctor source-port adapters and the composed-report factory.
//!
//! Each adapter holds one already-resolved kernel *read* and implements the
//! matching source port. Surfaces that own live signals (the daemon doctor
//! reader, tests) map those signals into reads, then hand the bundle to
//! [`compose_doctor_report`]. This module owns no store, scheduler, or
//! transport; daemon I/O that gathers the signals stays in the composition
//! root.
//!
//! [`DaemonRuntimeHealthSignalV1`] is the boundary type daemon writers fill.
//! Mapping it through [`runtime_health_read`] never imports daemon types.

use tracedecay_domain::CodeGenerationId;

use crate::RequestContext;
use crate::error::ApplicationContractError;
use crate::feedback::FeedbackCompletedPublicationV1;

use super::report::{DoctorReportComposerV1, DoctorReportV1};
#[cfg(test)]
use super::sources::SemanticOwnerStateV1;
use super::sources::{
    AdvisoryFeedbackDoctorPort, AdvisoryFeedbackFindingReadV1, AdvisoryFeedbackReadV1,
    AdvisoryFeedbackSummaryReadV1, CodeIndexMountDoctorPort, CodeIndexMountReadV1,
    ConfigurationAuthorityDoctorPort, ConfigurationAuthorityReadV1, DoctorSourceFuture,
    DoctorStorageFamilyReadV1, DoctorStorageIncompleteReasonV1, HostIntegrationDoctorPort,
    HostIntegrationReadV1, IngestRefusalCensusReadV1, LanguageServerDoctorPort,
    LanguageServerReadV1, ObservabilityDoctorPort, ObservabilityReadV1, OperationalAuditDoctorPort,
    OperationalAuditReadV1, RuntimeHealthDoctorPort, RuntimeHealthReadV1, RuntimeLivenessV1,
    SemanticOwnerDoctorPort, SemanticOwnerReadV1, StorageDoctorPort,
};
use super::types::{DoctorCoverageCompletenessV1, DoctorStorageFindingV1};

/// Adapter over the configuration authority (Configuration family).
pub struct ConfigurationAuthorityDoctorAdapterV1 {
    read: ConfigurationAuthorityReadV1,
}

impl ConfigurationAuthorityDoctorAdapterV1 {
    /// Build the adapter from an already-resolved kernel read.
    #[must_use]
    pub fn from_read(read: ConfigurationAuthorityReadV1) -> Self {
        Self { read }
    }
}

impl ConfigurationAuthorityDoctorPort for ConfigurationAuthorityDoctorAdapterV1 {
    fn configuration_health<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, ConfigurationAuthorityReadV1> {
        let read = self.read.clone();
        Box::pin(async move { read })
    }
}

/// The real daemon/runtime health signals a serving runtime writes.
///
/// This is the *writer-side* snapshot: the daemon fills each field from its
/// own startup and storage-authority probes. Each optional signal is `None`
/// when that probe has not run, so an undetermined signal weakens coverage
/// rather than being assumed healthy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DaemonRuntimeHealthSignalV1 {
    /// The daemon runtime is serving requests (its actors are alive).
    pub serving: bool,
    /// Schema migration and compatibility projections have converged.
    pub startup_converged: bool,
    /// The storage quick-check passed, when the daemon has run it.
    pub quick_check_ok: Option<bool>,
    /// The storage authority audit passed, when the daemon has run it.
    pub authority_audit_ok: Option<bool>,
    /// The session temporal projections are healthy, when determined.
    pub temporal_ok: Option<bool>,
}

/// Map a daemon runtime-health signal into its kernel read.
///
/// A runtime that is not serving is genuinely undetermined health, not a proven
/// degraded condition: it reports `Unreachable`. A serving runtime whose storage
/// authority signals prove a failure is `Stuck`; one that is serving but has not
/// converged is `Degraded`; one that is serving, converged, and clean is
/// `Healthy` — but only with complete coverage when every optional signal was
/// actually observed. A missing signal drops coverage to partial (an honest
/// "healthy so far as observed", never a healthy-complete claim).
#[must_use]
pub fn runtime_health_read(signal: &DaemonRuntimeHealthSignalV1) -> RuntimeHealthReadV1 {
    if !signal.serving {
        return RuntimeHealthReadV1::Observed {
            liveness: RuntimeLivenessV1::Unreachable,
            coverage: DoctorCoverageCompletenessV1::Unknown,
        };
    }
    let proven_failure = signal.quick_check_ok == Some(false)
        || signal.authority_audit_ok == Some(false)
        || signal.temporal_ok == Some(false);
    if proven_failure {
        return RuntimeHealthReadV1::Observed {
            liveness: RuntimeLivenessV1::Stuck,
            coverage: DoctorCoverageCompletenessV1::Complete,
        };
    }
    if !signal.startup_converged {
        return RuntimeHealthReadV1::Observed {
            liveness: RuntimeLivenessV1::Degraded,
            coverage: DoctorCoverageCompletenessV1::Complete,
        };
    }
    let fully_observed = signal.quick_check_ok == Some(true)
        && signal.authority_audit_ok == Some(true)
        && signal.temporal_ok == Some(true);
    let coverage = if fully_observed {
        DoctorCoverageCompletenessV1::Complete
    } else {
        DoctorCoverageCompletenessV1::Partial
    };
    RuntimeHealthReadV1::Observed {
        liveness: RuntimeLivenessV1::Healthy,
        coverage,
    }
}

/// Adapter over the daemon/runtime health snapshot (`StorageRuntime` family).
pub struct RuntimeHealthDoctorAdapterV1 {
    read: RuntimeHealthReadV1,
}

impl RuntimeHealthDoctorAdapterV1 {
    /// Build the adapter from an already-resolved kernel read.
    #[must_use]
    pub fn from_read(read: RuntimeHealthReadV1) -> Self {
        Self { read }
    }
}

impl RuntimeHealthDoctorPort for RuntimeHealthDoctorAdapterV1 {
    fn runtime_health<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, RuntimeHealthReadV1> {
        let read = self.read.clone();
        Box::pin(async move { read })
    }
}

/// Adapter over Remote HTTPS and registered-profile operational authority.
pub struct OperationalAuditDoctorAdapterV1 {
    read: OperationalAuditReadV1,
}

impl OperationalAuditDoctorAdapterV1 {
    #[must_use]
    pub fn from_read(read: OperationalAuditReadV1) -> Self {
        Self { read }
    }
}

impl OperationalAuditDoctorPort for OperationalAuditDoctorAdapterV1 {
    fn operational_audit<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, OperationalAuditReadV1> {
        let read = self.read.clone();
        Box::pin(async move { read })
    }
}

/// Adapter over host/agent integration conformance (Advisory family).
pub struct HostIntegrationDoctorAdapterV1 {
    read: HostIntegrationReadV1,
}

impl HostIntegrationDoctorAdapterV1 {
    /// Build the adapter from an already-resolved kernel read.
    #[must_use]
    pub fn from_read(read: HostIntegrationReadV1) -> Self {
        Self { read }
    }
}

impl HostIntegrationDoctorPort for HostIntegrationDoctorAdapterV1 {
    fn host_conformance<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, HostIntegrationReadV1> {
        let read = self.read.clone();
        Box::pin(async move { read })
    }
}

/// Project the latest exact-scope durable feedback publication into Doctor's
/// distinct advisory port. Host conformance remains a separate source.
#[must_use]
pub fn advisory_feedback_read_from_publication(
    publication: Option<&FeedbackCompletedPublicationV1>,
    current_generation: Option<&CodeGenerationId>,
) -> AdvisoryFeedbackReadV1 {
    let Some(publication) = publication else {
        return AdvisoryFeedbackReadV1::Absent;
    };
    if publication.validate().is_err() {
        return AdvisoryFeedbackReadV1::Unknown;
    }
    let Some(generation_id) = publication.input.target.generation_id.clone() else {
        return AdvisoryFeedbackReadV1::Unknown;
    };
    let generation_current = current_generation == Some(&generation_id);
    let summary = AdvisoryFeedbackSummaryReadV1 {
        result_id: publication.result.result_id.clone(),
        cycle_id: publication.result.cycle_id.clone(),
        scope: publication.result.scope.clone(),
        generation_id: generation_id.clone(),
        generation_current,
        termination: publication.result.termination,
        provider_states: publication.result.provider_states.clone(),
        total_findings: publication.result.total_findings,
        returned_findings: publication.result.returned_findings,
        omitted_findings: publication.result.omitted_findings,
    };
    let impact_anchors = publication
        .result
        .impact
        .as_ref()
        .map(|impact| impact.evidence_anchors.as_slice())
        .unwrap_or_default();
    let findings = publication
        .result
        .findings
        .iter()
        .map(|finding| {
            let mut evidence_anchors = finding
                .retrieval_anchor_id
                .iter()
                .cloned()
                .chain(impact_anchors.iter().cloned())
                .collect::<Vec<_>>();
            evidence_anchors.sort();
            evidence_anchors.dedup();
            AdvisoryFeedbackFindingReadV1 {
                result_id: publication.result.result_id.clone(),
                cycle_id: publication.result.cycle_id.clone(),
                finding_id: finding.finding_id.clone(),
                scope: publication.result.scope.clone(),
                generation_id: generation_id.clone(),
                generation_current,
                lifecycle: finding.lifecycle,
                provider_state: finding.provider_state,
                evidence_anchors,
                total_findings: publication.result.total_findings,
                returned_findings: publication.result.returned_findings,
                omitted_findings: publication.result.omitted_findings,
            }
        })
        .collect();
    AdvisoryFeedbackReadV1::Observed {
        summary: Box::new(summary),
        findings,
    }
}

/// Adapter over the mounted feedback owner's canonical read store.
pub struct AdvisoryFeedbackDoctorAdapterV1 {
    read: AdvisoryFeedbackReadV1,
}

impl AdvisoryFeedbackDoctorAdapterV1 {
    #[must_use]
    pub fn from_read(read: AdvisoryFeedbackReadV1) -> Self {
        Self { read }
    }
}

impl AdvisoryFeedbackDoctorPort for AdvisoryFeedbackDoctorAdapterV1 {
    fn advisory_feedback<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, AdvisoryFeedbackReadV1> {
        let read = self.read.clone();
        Box::pin(async move { read })
    }
}

/// Adapter over the code-index mount state (`SemanticIndex` family).
pub struct CodeIndexMountDoctorAdapterV1 {
    read: CodeIndexMountReadV1,
}

impl CodeIndexMountDoctorAdapterV1 {
    /// Build the adapter from an already-resolved kernel read.
    #[must_use]
    pub fn from_read(read: CodeIndexMountReadV1) -> Self {
        Self { read }
    }
}

impl CodeIndexMountDoctorPort for CodeIndexMountDoctorAdapterV1 {
    fn code_index_mount<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, CodeIndexMountReadV1> {
        let read = self.read.clone();
        Box::pin(async move { read })
    }
}

/// Adapter over the independently scheduled semantic owner.
pub struct SemanticOwnerDoctorAdapterV1 {
    read: SemanticOwnerReadV1,
}

impl SemanticOwnerDoctorAdapterV1 {
    #[must_use]
    pub fn from_read(read: SemanticOwnerReadV1) -> Self {
        Self { read }
    }
}

impl SemanticOwnerDoctorPort for SemanticOwnerDoctorAdapterV1 {
    fn semantic_owner<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, SemanticOwnerReadV1> {
        let read = self.read.clone();
        Box::pin(async move { read })
    }
}

/// Adapter over live language-server/analyzer state.
pub struct LanguageServerDoctorAdapterV1 {
    read: LanguageServerReadV1,
}

impl LanguageServerDoctorAdapterV1 {
    #[must_use]
    pub fn from_read(read: LanguageServerReadV1) -> Self {
        Self { read }
    }
}

impl LanguageServerDoctorPort for LanguageServerDoctorAdapterV1 {
    fn language_server_health<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, LanguageServerReadV1> {
        let read = self.read.clone();
        Box::pin(async move { read })
    }
}

/// Adapter over the canonical durable Plan-26 observation read model.
pub struct ObservabilityDoctorAdapterV1 {
    read: ObservabilityReadV1,
    refusals: IngestRefusalCensusReadV1,
}

impl ObservabilityDoctorAdapterV1 {
    #[must_use]
    pub fn from_read(read: ObservabilityReadV1) -> Self {
        Self {
            read,
            refusals: IngestRefusalCensusReadV1::Unknown,
        }
    }

    #[must_use]
    pub fn with_refusals(mut self, refusals: IngestRefusalCensusReadV1) -> Self {
        self.refusals = refusals;
        self
    }
}

impl ObservabilityDoctorPort for ObservabilityDoctorAdapterV1 {
    fn observability_health<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, ObservabilityReadV1> {
        let read = self.read.clone();
        Box::pin(async move { read })
    }

    fn ingest_refusal_census<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, IngestRefusalCensusReadV1> {
        let read = self.refusals.clone();
        Box::pin(async move { read })
    }
}

/// Wrap a set of typed storage findings the retention producers emitted into a
/// kernel read.
///
/// An empty finding set is a typed [`DoctorStorageFamilyReadV1::Absent`] — the
/// runtime was consulted but produced nothing — never a fabricated healthy
/// claim; the composer classifies an empty observed read as absent regardless,
/// and this keeps the intent explicit at the source.
#[must_use]
pub fn storage_family_read(findings: Vec<DoctorStorageFindingV1>) -> DoctorStorageFamilyReadV1 {
    if findings.is_empty() {
        DoctorStorageFamilyReadV1::Absent
    } else {
        DoctorStorageFamilyReadV1::Observed { findings }
    }
}

/// Combine two independently consulted storage-family reads.
///
/// Findings accumulate. When either side is unresolved, coverage weakens to
/// the more severe incomplete reason rather than dropping observed findings.
#[must_use]
pub fn merge_storage_reads(
    first: DoctorStorageFamilyReadV1,
    second: DoctorStorageFamilyReadV1,
) -> DoctorStorageFamilyReadV1 {
    let (mut findings, first_incomplete) = storage_read_parts(first);
    let (second_findings, second_incomplete) = storage_read_parts(second);
    findings.extend(second_findings);
    let incomplete = first_incomplete.max(second_incomplete);

    match (findings.is_empty(), incomplete) {
        (false, Some(reason)) => DoctorStorageFamilyReadV1::ObservedIncomplete { findings, reason },
        (false, None) => storage_family_read(findings),
        (true, Some(DoctorStorageIncompleteReasonV1::Unsupported)) => {
            DoctorStorageFamilyReadV1::Unsupported
        }
        (true, Some(DoctorStorageIncompleteReasonV1::Denied)) => DoctorStorageFamilyReadV1::Denied,
        (true, Some(DoctorStorageIncompleteReasonV1::Unknown)) => {
            DoctorStorageFamilyReadV1::Unknown
        }
        (true, Some(DoctorStorageIncompleteReasonV1::Unavailable { detail })) => {
            DoctorStorageFamilyReadV1::Unavailable { detail }
        }
        (true, Some(DoctorStorageIncompleteReasonV1::ResetRequired { detail })) => {
            DoctorStorageFamilyReadV1::ResetRequired { detail }
        }
        (true, Some(DoctorStorageIncompleteReasonV1::Corrupt { detail })) => {
            DoctorStorageFamilyReadV1::Corrupt { detail }
        }
        (true, None) => DoctorStorageFamilyReadV1::Absent,
    }
}

fn storage_read_parts(
    read: DoctorStorageFamilyReadV1,
) -> (
    Vec<DoctorStorageFindingV1>,
    Option<DoctorStorageIncompleteReasonV1>,
) {
    match read {
        DoctorStorageFamilyReadV1::Observed { findings } => (findings, None),
        DoctorStorageFamilyReadV1::ObservedIncomplete { findings, reason } => {
            (findings, Some(reason))
        }
        DoctorStorageFamilyReadV1::Unsupported => (
            Vec::new(),
            Some(DoctorStorageIncompleteReasonV1::Unsupported),
        ),
        DoctorStorageFamilyReadV1::Absent => (Vec::new(), None),
        DoctorStorageFamilyReadV1::Denied => {
            (Vec::new(), Some(DoctorStorageIncompleteReasonV1::Denied))
        }
        DoctorStorageFamilyReadV1::Unknown => {
            (Vec::new(), Some(DoctorStorageIncompleteReasonV1::Unknown))
        }
        DoctorStorageFamilyReadV1::Unavailable { detail } => (
            Vec::new(),
            Some(DoctorStorageIncompleteReasonV1::Unavailable { detail }),
        ),
        DoctorStorageFamilyReadV1::ResetRequired { detail } => (
            Vec::new(),
            Some(DoctorStorageIncompleteReasonV1::ResetRequired { detail }),
        ),
        DoctorStorageFamilyReadV1::Corrupt { detail } => (
            Vec::new(),
            Some(DoctorStorageIncompleteReasonV1::Corrupt { detail }),
        ),
    }
}

/// Adapter over storage retention/size findings (Storage family).
pub struct StorageDoctorAdapterV1 {
    read: DoctorStorageFamilyReadV1,
}

impl StorageDoctorAdapterV1 {
    /// Build the adapter from an already-resolved kernel read.
    #[must_use]
    pub fn from_read(read: DoctorStorageFamilyReadV1) -> Self {
        Self { read }
    }
}

impl StorageDoctorPort for StorageDoctorAdapterV1 {
    fn storage_findings<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, DoctorStorageFamilyReadV1> {
        let read = self.read.clone();
        Box::pin(async move { read })
    }
}

/// The seven resolved kernel reads a Doctor report composes from.
///
/// A surface builds this bundle from the real signals it can reach and hands
/// it to [`compose_doctor_report`]. A signal the surface cannot obtain carries
/// its honest typed absence rather than a fabricated healthy read.
#[derive(Clone, Debug)]
pub struct DoctorKernelInputsV1 {
    /// Configuration-authority read (Configuration family).
    pub configuration: ConfigurationAuthorityReadV1,
    /// Daemon/runtime health read (`StorageRuntime` family).
    pub runtime: RuntimeHealthReadV1,
    /// Remote HTTPS and exact registered-profile operational authority.
    pub operational_audit: OperationalAuditReadV1,
    /// Host/agent integration conformance read (Advisory family).
    pub host: HostIntegrationReadV1,
    /// Mounted canonical feedback-owner read (Advisory family).
    pub advisory_feedback: AdvisoryFeedbackReadV1,
    /// Live language-server/analyzer read (`LanguageServer` family).
    pub language_server: LanguageServerReadV1,
    /// Code-index mount read (`SemanticIndex` family).
    pub code_index: CodeIndexMountReadV1,
    /// Independent semantic activation-owner read (`SemanticIndex` family).
    pub semantic_owner: SemanticOwnerReadV1,
    /// Canonical durable Plan-26 feedback read (`Observability` family).
    pub observability: ObservabilityReadV1,
    /// Durable ingest-coverage refusal census (`Observability` family).
    pub ingest_refusals: IngestRefusalCensusReadV1,
    /// Storage retention/size read (Storage family).
    pub storage: DoctorStorageFamilyReadV1,
}

/// Compose a Doctor report from already-resolved source adapters.
///
/// Wires all seven adapters into the kernel [`DoctorReportComposerV1`] and
/// composes. The composer enumerates every finding family truthfully: a family
/// whose read is unavailable is carried with its real evidence state and an
/// explicit coverage record, and the report asserts health only when every
/// family was consulted with complete coverage and every finding is healthy.
#[hotpath::measure(label = "daemon.doctor.compose", future = true)]
pub async fn compose_doctor_report(
    context: &RequestContext,
    inputs: &DoctorKernelInputsV1,
) -> Result<DoctorReportV1, ApplicationContractError> {
    let configuration =
        ConfigurationAuthorityDoctorAdapterV1::from_read(inputs.configuration.clone());
    let runtime = RuntimeHealthDoctorAdapterV1::from_read(inputs.runtime.clone());
    let operational_audit =
        OperationalAuditDoctorAdapterV1::from_read(inputs.operational_audit.clone());
    let host = HostIntegrationDoctorAdapterV1::from_read(inputs.host.clone());
    let advisory_feedback =
        AdvisoryFeedbackDoctorAdapterV1::from_read(inputs.advisory_feedback.clone());
    let language_server = LanguageServerDoctorAdapterV1::from_read(inputs.language_server.clone());
    let code_index = CodeIndexMountDoctorAdapterV1::from_read(inputs.code_index.clone());
    let semantic_owner = SemanticOwnerDoctorAdapterV1::from_read(inputs.semantic_owner.clone());
    let observability = ObservabilityDoctorAdapterV1::from_read(inputs.observability.clone())
        .with_refusals(inputs.ingest_refusals.clone());
    let storage = StorageDoctorAdapterV1::from_read(inputs.storage.clone());

    let composer = DoctorReportComposerV1::new()
        .with_configuration(&configuration)
        .with_runtime(&runtime)
        .with_operational_audit(&operational_audit)
        .with_host(&host)
        .with_advisory_feedback(&advisory_feedback)
        .with_language_server(&language_server)
        .with_code_index(&code_index)
        .with_semantic_owner(&semantic_owner)
        .with_observability(&observability)
        .with_storage(&storage);

    composer.compose(context).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeSet;

    use tracedecay_domain::{
        ActorId, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use crate::doctor::{
        CodeIndexMountReadV1, CodeIndexMountStateV1, ConfigurationAuthorityReadV1,
        ConfigurationDriftV1, DoctorCoverageCompletenessV1, DoctorCoverageStatementV1,
        DoctorEvidenceRefV1, DoctorEvidenceReferenceV1, DoctorEvidenceStateV1,
        DoctorFamilyConsultationV1, DoctorFamilyCoverageV1, DoctorFamilyUnavailableReasonV1,
        DoctorFindingFamilyV1, DoctorFindingV1, DoctorStorageFamilyReadV1,
        DoctorStorageFindingKindV1, DoctorStorageFindingV1, DoctorStorageIncompleteReasonV1,
        HostConformanceV1, HostIntegrationReadV1, IngestRefusalCensusReadV1, IngestRefusalCountV1,
        LanguageServerReadV1, LanguageServerStateV1, ObservabilityReadV1, ObservabilityStateV1,
        OperationalAuditReadV1, ProfileAuthorityReadV1, RemoteOperationalReadV1,
        RuntimeHealthReadV1, RuntimeLivenessV1,
    };
    use crate::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestContext, RequestId, ResolvedScope,
    };

    use super::*;

    fn context() -> RequestContext {
        let actor = ActorId::new("actor.doctor-adapter-test").unwrap();
        let scope = ResolvedScope::new(
            ProjectId::new("project.doctor-adapter-test").unwrap(),
            RepositoryId::new("repository.doctor-adapter-test").unwrap(),
            WorktreeId::new("worktree.doctor-adapter-test").unwrap(),
            None,
        )
        .unwrap();
        let capability = CapabilityId::new("capability.doctor-adapter-test").unwrap();
        let use_case = UseCaseId::new("use-case.doctor-adapter-test").unwrap();
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.doctor-adapter-test").unwrap(),
            1,
            ManifestDigest::new(format!("sha256:{}", "11".repeat(32))).unwrap(),
            actor.clone(),
            UtcMicros(1),
            UtcMicros(10_000),
            scope.clone(),
            BTreeSet::from([capability]),
            BTreeSet::from([use_case]),
            DisclosureClass::Evidence,
        )
        .unwrap();
        RequestContext::new(
            actor,
            scope,
            grant,
            RequestId::new("request.doctor-adapter-test").unwrap(),
            Deadline::new(UtcMicros(9_000)).unwrap(),
            CancellationContext::active("cancel.doctor-adapter-test").unwrap(),
        )
        .unwrap()
    }

    fn orphan_storage_finding() -> DoctorStorageFindingV1 {
        let evidence = DoctorEvidenceRefV1::new(
            DoctorFindingFamilyV1::Storage,
            DoctorEvidenceReferenceV1::new("storage.orphan_store.fixture.age-42d").unwrap(),
        );
        let coverage = DoctorCoverageStatementV1::new(
            DoctorCoverageCompletenessV1::Complete,
            "orphan store identity no longer resolves",
        )
        .unwrap();
        let finding = DoctorFindingV1::new(
            DoctorFindingFamilyV1::Storage,
            DoctorEvidenceStateV1::Degraded,
            vec![evidence],
            coverage,
        )
        .unwrap();
        DoctorStorageFindingV1::new(DoctorStorageFindingKindV1::OrphanStore, finding).unwrap()
    }

    #[tokio::test]
    async fn configuration_adapter_returns_seeded_read() {
        let ctx = context();
        for read in [
            ConfigurationAuthorityReadV1::Resolved {
                drift: ConfigurationDriftV1::InSync,
                coverage: DoctorCoverageCompletenessV1::Complete,
            },
            ConfigurationAuthorityReadV1::Absent,
            ConfigurationAuthorityReadV1::Denied,
            ConfigurationAuthorityReadV1::Unknown,
        ] {
            let adapter = ConfigurationAuthorityDoctorAdapterV1::from_read(read.clone());
            assert_eq!(adapter.configuration_health(&ctx).await, read);
        }
    }

    #[test]
    fn runtime_healthy_requires_all_signals_observed_for_complete_coverage() {
        let healthy = DaemonRuntimeHealthSignalV1 {
            serving: true,
            startup_converged: true,
            quick_check_ok: Some(true),
            authority_audit_ok: Some(true),
            temporal_ok: Some(true),
        };
        assert_eq!(
            runtime_health_read(&healthy),
            RuntimeHealthReadV1::Observed {
                liveness: RuntimeLivenessV1::Healthy,
                coverage: DoctorCoverageCompletenessV1::Complete,
            }
        );
        for partial in [
            DaemonRuntimeHealthSignalV1 {
                temporal_ok: None,
                ..healthy
            },
            DaemonRuntimeHealthSignalV1 {
                authority_audit_ok: None,
                ..healthy
            },
        ] {
            assert_eq!(
                runtime_health_read(&partial),
                RuntimeHealthReadV1::Observed {
                    liveness: RuntimeLivenessV1::Healthy,
                    coverage: DoctorCoverageCompletenessV1::Partial,
                }
            );
        }
    }

    #[test]
    fn runtime_degraded_stuck_and_unreachable_are_honest() {
        let degraded = DaemonRuntimeHealthSignalV1 {
            serving: true,
            startup_converged: false,
            ..DaemonRuntimeHealthSignalV1::default()
        };
        assert_eq!(
            runtime_health_read(&degraded),
            RuntimeHealthReadV1::Observed {
                liveness: RuntimeLivenessV1::Degraded,
                coverage: DoctorCoverageCompletenessV1::Complete,
            }
        );
        let stuck = DaemonRuntimeHealthSignalV1 {
            serving: true,
            startup_converged: true,
            quick_check_ok: Some(false),
            ..DaemonRuntimeHealthSignalV1::default()
        };
        assert_eq!(
            runtime_health_read(&stuck),
            RuntimeHealthReadV1::Observed {
                liveness: RuntimeLivenessV1::Stuck,
                coverage: DoctorCoverageCompletenessV1::Complete,
            }
        );
        let unreachable = DaemonRuntimeHealthSignalV1::default();
        assert_eq!(
            runtime_health_read(&unreachable),
            RuntimeHealthReadV1::Observed {
                liveness: RuntimeLivenessV1::Unreachable,
                coverage: DoctorCoverageCompletenessV1::Unknown,
            }
        );
    }

    #[tokio::test]
    async fn runtime_adapter_returns_seeded_read() {
        let ctx = context();
        let read = RuntimeHealthReadV1::Denied;
        let adapter = RuntimeHealthDoctorAdapterV1::from_read(read.clone());
        assert_eq!(adapter.runtime_health(&ctx).await, read);
    }

    #[tokio::test]
    async fn host_adapter_returns_seeded_read() {
        let ctx = context();
        let read = HostIntegrationReadV1::Observed {
            conformance: HostConformanceV1::ProtocolDrift,
            coverage: DoctorCoverageCompletenessV1::Complete,
        };
        let adapter = HostIntegrationDoctorAdapterV1::from_read(read.clone());
        assert_eq!(adapter.host_conformance(&ctx).await, read);
    }

    #[tokio::test]
    async fn code_index_adapter_returns_seeded_read() {
        let ctx = context();
        let read = CodeIndexMountReadV1::Absent;
        let adapter = CodeIndexMountDoctorAdapterV1::from_read(read.clone());
        assert_eq!(adapter.code_index_mount(&ctx).await, read);
    }

    #[test]
    fn storage_family_read_absent_when_empty() {
        assert_eq!(
            storage_family_read(Vec::new()),
            DoctorStorageFamilyReadV1::Absent
        );
    }

    #[test]
    fn storage_family_read_observed_when_findings_present() {
        let read = storage_family_read(vec![orphan_storage_finding()]);
        match read {
            DoctorStorageFamilyReadV1::Observed { findings } => {
                assert_eq!(findings.len(), 1);
                assert_eq!(findings[0].kind(), DoctorStorageFindingKindV1::OrphanStore);
            }
            other => panic!("expected observed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn storage_adapter_returns_seeded_read() {
        let ctx = context();
        let adapter =
            StorageDoctorAdapterV1::from_read(storage_family_read(vec![orphan_storage_finding()]));
        match adapter.storage_findings(&ctx).await {
            DoctorStorageFamilyReadV1::Observed { findings } => assert_eq!(findings.len(), 1),
            other => panic!("expected observed, got {other:?}"),
        }
    }

    #[test]
    fn unresolved_storage_producers_preserve_findings_and_weaken_coverage() {
        for (unresolved, expected_reason) in [
            (
                DoctorStorageFamilyReadV1::Unsupported,
                DoctorStorageIncompleteReasonV1::Unsupported,
            ),
            (
                DoctorStorageFamilyReadV1::Denied,
                DoctorStorageIncompleteReasonV1::Denied,
            ),
            (
                DoctorStorageFamilyReadV1::Unknown,
                DoctorStorageIncompleteReasonV1::Unknown,
            ),
        ] {
            let observed = storage_family_read(vec![orphan_storage_finding()]);
            for merged in [
                merge_storage_reads(observed.clone(), unresolved.clone()),
                merge_storage_reads(unresolved, observed),
            ] {
                match merged {
                    DoctorStorageFamilyReadV1::ObservedIncomplete { findings, reason } => {
                        assert_eq!(reason, expected_reason);
                        assert_eq!(findings.len(), 1);
                        assert_eq!(findings[0].kind(), DoctorStorageFindingKindV1::OrphanStore);
                    }
                    other => panic!("expected incomplete observations, got {other:?}"),
                }
            }
        }
    }

    #[tokio::test]
    async fn composed_report_carries_real_states_and_enumerates_coverage() {
        let ctx = context();
        let inputs = DoctorKernelInputsV1 {
            configuration: ConfigurationAuthorityReadV1::Resolved {
                drift: ConfigurationDriftV1::InSync,
                coverage: DoctorCoverageCompletenessV1::Complete,
            },
            runtime: runtime_health_read(&DaemonRuntimeHealthSignalV1 {
                serving: true,
                startup_converged: false,
                ..DaemonRuntimeHealthSignalV1::default()
            }),
            operational_audit: OperationalAuditReadV1 {
                remote: RemoteOperationalReadV1::Unconfigured,
                profile_authority: ProfileAuthorityReadV1::Unavailable,
            },
            host: HostIntegrationReadV1::Denied,
            advisory_feedback: AdvisoryFeedbackReadV1::Absent,
            language_server: LanguageServerReadV1::Observed {
                state: LanguageServerStateV1::Ready,
                coverage: DoctorCoverageCompletenessV1::Complete,
            },
            code_index: CodeIndexMountReadV1::Observed {
                state: CodeIndexMountStateV1::Mounted,
                coverage: DoctorCoverageCompletenessV1::Complete,
            },
            semantic_owner: SemanticOwnerReadV1::Observed {
                state: SemanticOwnerStateV1::Ready,
                coverage: DoctorCoverageCompletenessV1::Complete,
            },
            observability: ObservabilityReadV1::Observed {
                state: ObservabilityStateV1::Current,
                total_count: 7,
                last_observed_at_micros: Some(42),
                coverage: DoctorCoverageCompletenessV1::Partial,
            },
            ingest_refusals: IngestRefusalCensusReadV1::Observed {
                refusals: vec![IngestRefusalCountV1 {
                    provider: "cursor".to_owned(),
                    reason: "admission_refused".to_owned(),
                    count: 160,
                }],
            },
            storage: merge_storage_reads(
                storage_family_read(vec![orphan_storage_finding()]),
                DoctorStorageFamilyReadV1::Unknown,
            ),
        };

        let report = compose_doctor_report(&ctx, &inputs).await.expect("report");

        assert_eq!(report.coverage().families().len(), 7);

        let family_state = |family: DoctorFindingFamilyV1| {
            report
                .findings()
                .find(|finding| finding.family() == family)
                .map(DoctorFindingV1::state)
        };
        assert_eq!(
            family_state(DoctorFindingFamilyV1::Configuration),
            Some(DoctorEvidenceStateV1::HealthyCompleteCoverage)
        );
        assert_eq!(
            family_state(DoctorFindingFamilyV1::StorageRuntime),
            Some(DoctorEvidenceStateV1::Degraded)
        );
        assert_eq!(
            family_state(DoctorFindingFamilyV1::Advisory),
            Some(DoctorEvidenceStateV1::Denied)
        );
        assert_eq!(
            family_state(DoctorFindingFamilyV1::SemanticIndex),
            Some(DoctorEvidenceStateV1::HealthyCompleteCoverage)
        );
        assert_eq!(
            family_state(DoctorFindingFamilyV1::Storage),
            Some(DoctorEvidenceStateV1::Degraded)
        );
        assert_eq!(
            family_state(DoctorFindingFamilyV1::LanguageServer),
            Some(DoctorEvidenceStateV1::HealthyCompleteCoverage)
        );
        assert_eq!(
            family_state(DoctorFindingFamilyV1::Observability),
            Some(DoctorEvidenceStateV1::Partial)
        );

        assert!(!report.is_healthy_complete());
        assert_ne!(
            report.coverage().completeness(),
            DoctorCoverageCompletenessV1::Complete
        );

        let consultation = |family: DoctorFindingFamilyV1| {
            report
                .coverage()
                .families()
                .iter()
                .find(|record| record.family() == family)
                .map(DoctorFamilyCoverageV1::consultation)
        };
        assert_eq!(
            consultation(DoctorFindingFamilyV1::LanguageServer),
            Some(DoctorFamilyConsultationV1::Consulted)
        );
        assert_eq!(
            consultation(DoctorFindingFamilyV1::Observability),
            Some(DoctorFamilyConsultationV1::Consulted)
        );
        assert_eq!(
            consultation(DoctorFindingFamilyV1::Advisory),
            Some(DoctorFamilyConsultationV1::Unavailable {
                reason: DoctorFamilyUnavailableReasonV1::Denied,
            })
        );
        assert_eq!(
            consultation(DoctorFindingFamilyV1::Configuration),
            Some(DoctorFamilyConsultationV1::Consulted)
        );
        assert_eq!(
            consultation(DoctorFindingFamilyV1::Storage),
            Some(DoctorFamilyConsultationV1::Unavailable {
                reason: DoctorFamilyUnavailableReasonV1::Unknown,
            })
        );
    }
}

//! Daemon-side adapters for the Doctor kernel source ports (Plan 09 §PR14).
//!
//! The transport-neutral Doctor kernel
//! ([`tracedecay_application::doctor`]) defines seven narrow source ports and one
//! [`DoctorReportComposerV1`] that composes their findings into a
//! [`DoctorReportV1`]. The kernel owns no store, runtime, or health formula; the
//! adapters that read real daemon state live here, in the daemon that owns that
//! state.
//!
//! Each adapter implements one port by returning a kernel *read* value it was
//! constructed with. The read is resolved from a real daemon signal by the pure
//! mapper functions in this module (`*_read` / `*_read_from_*`), so the honest
//! mapping (unit-tested exhaustively) is kept separate from the thin IO that
//! gathers the signal. Truthfulness is preserved end to end: a signal that
//! cannot be consulted maps to the kernel's typed
//! `Unsupported`/`Absent`/`Denied`/`Unknown` read — never a fabricated healthy
//! result — and partial coverage carries its real reason.
//!
//! The [`compose_doctor_report`] factory wires all seven adapters into the kernel
//! composer from a [`DoctorKernelInputsV1`] bundle. Any surface (the dashboard
//! `/api/doctor/findings` handler, the MCP doctor tools) builds that bundle from
//! the real signals it can reach and requests a composed report; the surface
//! never re-implements the composition or the honest mapping.
//!
//! The composer factory and its mappers are the daemon-owned handoff surface for
//! those surface bindings (owned by the API-bindings lane), so the module is not
//! yet consumed from a non-test path in this crate; `dead_code` is allowed until
//! the surface handlers are wired to it.
#![allow(dead_code)]

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_application::doctor::{
    AdvisoryFeedbackDoctorPort, AdvisoryFeedbackFindingReadV1, AdvisoryFeedbackReadV1,
    AdvisoryFeedbackSummaryReadV1, CodeIndexMountDoctorPort, CodeIndexMountReadV1,
    CodeIndexMountStateV1, ConfigurationAuthorityDoctorPort, ConfigurationAuthorityReadV1,
    ConfigurationDriftV1, DoctorCoverageCompletenessV1, DoctorReportComposerV1, DoctorReportV1,
    DoctorSourceFuture, DoctorStorageFamilyReadV1, DoctorStorageFindingV1, HostConformanceV1,
    HostIntegrationDoctorPort, HostIntegrationReadV1, LanguageServerDoctorPort,
    LanguageServerReadV1, LanguageServerStateV1, ObservabilityDoctorPort, ObservabilityReadV1,
    ObservabilityStateV1, OperationalAuditDoctorPort, OperationalAuditReadV1,
    ProfileAuthorityReadV1, RemoteOperationalReadV1, RuntimeHealthDoctorPort, RuntimeHealthReadV1,
    RuntimeLivenessV1, StorageDoctorPort,
};
use tracedecay_application::{
    ApplicationContractError, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, EffectReceipt, EffectTermination, IdempotencyKey,
    OperationBudgetUsage, OperationReceipt, PreviewId, RequestContext, RequestId, now_micros,
};
use tracedecay_domain::ManifestDigest;

use crate::config::PinnedRuntimeConfiguration;
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};

const DOCTOR_REPORT_CAPABILITY: &str = "capability.application.doctor.report";
const DOCTOR_REPORT_USE_CASE: &str = "use-case.application.doctor.report";
const DOCTOR_CONTEXT_HORIZON_MICROS: i64 = 30_000_000;

#[derive(Clone)]
pub(super) struct ProductionDoctorRemediationOwnersV1 {
    pub project_root: PathBuf,
    pub project_id: tracedecay_domain::ProjectId,
    pub layout: crate::storage::StoreLayout,
    pub registry: Arc<crate::global_db::RegisteredGlobalDb>,
    pub profile_sessions: Arc<crate::global_db::RegisteredGlobalDb>,
    pub project_sessions: Arc<crate::global_db::RegisteredGlobalDb>,
    pub profile_root: PathBuf,
    pub config: crate::config::TraceDecayConfig,
    pub global_retention: crate::retention::RetentionConfig,
    pub store_administration: super::StoreAdministration,
    pub invocation: super::DaemonInvocationState,
    pub code_index_store_root: PathBuf,
    pub semantic_runtime: crate::semantic_code::DaemonSemanticRuntimeHandleV1,
    pub semantic_database: Arc<crate::db::Database>,
    pub semantic_lifecycle: Option<Arc<crate::semantic_code::SemanticModelLifecycleOwnerV1>>,
    pub semantic_resources: crate::config::SemanticResourceCeilings,
    pub route_registered: Arc<std::sync::atomic::AtomicBool>,
}

// === Configuration authority (Configuration family) ==========================

/// The real configuration-authority signal the daemon observes for a project.
///
/// The configuration control plane pins one resolved revision and the daemon
/// reads it back from the process-local snapshot cache. Drift and an
/// unhonorable pin are distinct observed conditions; a cold cache is a typed
/// absence, never a fabricated in-sync claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigurationAuthoritySignalV1 {
    /// A resolved snapshot is pinned and effective configuration matches it.
    Pinned,
    /// Effective configuration diverges from the desired/resolved authority.
    Drifted,
    /// A requested pin could not be honored by the authority.
    PinUnavailable,
    /// The authority is reachable but has pinned no resolution for the project.
    Missing,
    /// Authorization to read the configuration authority was denied.
    Denied,
    /// The configuration state could not be determined.
    Unknown,
}

/// Map a configuration-authority signal into its kernel read.
#[must_use]
pub fn configuration_read(signal: ConfigurationAuthoritySignalV1) -> ConfigurationAuthorityReadV1 {
    match signal {
        ConfigurationAuthoritySignalV1::Pinned => ConfigurationAuthorityReadV1::Resolved {
            drift: ConfigurationDriftV1::InSync,
            coverage: DoctorCoverageCompletenessV1::Complete,
        },
        ConfigurationAuthoritySignalV1::Drifted => ConfigurationAuthorityReadV1::Resolved {
            drift: ConfigurationDriftV1::Drifted,
            coverage: DoctorCoverageCompletenessV1::Complete,
        },
        ConfigurationAuthoritySignalV1::PinUnavailable => ConfigurationAuthorityReadV1::Resolved {
            drift: ConfigurationDriftV1::PinUnavailable,
            coverage: DoctorCoverageCompletenessV1::Complete,
        },
        ConfigurationAuthoritySignalV1::Missing => ConfigurationAuthorityReadV1::Absent,
        ConfigurationAuthoritySignalV1::Denied => ConfigurationAuthorityReadV1::Denied,
        ConfigurationAuthoritySignalV1::Unknown => ConfigurationAuthorityReadV1::Unknown,
    }
}

/// Map a real pinned-configuration lookup outcome into a kernel read.
///
/// A pinned snapshot resolves in-sync (the cache invariant guarantees the pinned
/// configuration equals the value derived from its resolved snapshot, so within
/// the cache there is no unobserved drift). A cold cache — the fail-closed
/// accessor's `Err` — is a typed [`ConfigurationAuthorityReadV1::Absent`], never
/// a fabricated healthy result.
#[must_use]
pub fn configuration_read_from_pin<E>(
    resolved: &Result<PinnedRuntimeConfiguration, E>,
) -> ConfigurationAuthorityReadV1 {
    match resolved {
        Ok(_) => configuration_read(ConfigurationAuthoritySignalV1::Pinned),
        Err(_) => configuration_read(ConfigurationAuthoritySignalV1::Missing),
    }
}

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

    /// Build the adapter from a real configuration-authority signal.
    #[must_use]
    pub fn from_signal(signal: ConfigurationAuthoritySignalV1) -> Self {
        Self::from_read(configuration_read(signal))
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

// === Daemon/runtime health (StorageRuntime family) ===========================

/// The real daemon/runtime health signals the daemon reads from its own state.
///
/// This is the *daemon-side* read: the adapter runs inside the serving daemon
/// and reports the convergence of its own startup health (schema migration,
/// storage authority audit, temporal projections), not the external CLI socket
/// probe. Each optional signal is `None` when the daemon has not determined it,
/// so an undetermined signal weakens coverage rather than being assumed healthy.
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
/// A daemon that is not serving is genuinely undetermined health, not a proven
/// degraded condition: it reports `Unreachable`. A serving daemon whose storage
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

    /// Build the adapter from a real daemon runtime-health signal.
    #[must_use]
    pub fn from_signal(signal: &DaemonRuntimeHealthSignalV1) -> Self {
        Self::from_read(runtime_health_read(signal))
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

// === Host/agent integration conformance (Advisory family) ====================

/// A conformance summary over a host's installed components versus the expected
/// first-party catalog.
///
/// `probed` is how many installed component manifests were checked, `accepted`
/// how many the catalog verifier accepted (matching digest/version), and
/// `executable_present` whether the integration's executable was found at all.
/// The counts come from the real
/// [`crate::agents::host_bundle_registry`] verifier via
/// [`host_conformance_summary`]; this struct is the transport-neutral input the
/// pure mapper reasons over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostConformanceSummaryV1 {
    /// Installed component manifests checked against the catalog.
    pub probed: usize,
    /// Manifests the catalog verifier accepted.
    pub accepted: usize,
    /// Whether the integration executable is present.
    pub executable_present: bool,
}

/// Build a conformance summary by verifying each installed manifest against the
/// expected first-party catalog.
///
/// `accepts` is the real catalog check (for example
/// `|manifest| set.verify_manifest(manifest).is_ok()`); this helper only counts
/// its outcomes so the manifest type stays out of the pure mapping layer.
pub fn host_conformance_summary<M>(
    installed: &[M],
    executable_present: bool,
    mut accepts: impl FnMut(&M) -> bool,
) -> HostConformanceSummaryV1 {
    let accepted = installed
        .iter()
        .filter(|manifest| accepts(manifest))
        .count();
    HostConformanceSummaryV1 {
        probed: installed.len(),
        accepted,
        executable_present,
    }
}

/// Map a host conformance summary into its kernel read.
///
/// No installed components and no executable is a typed absence (nothing to
/// probe). A missing executable is an observed `ExecutableAbsent`. When every
/// probed manifest is accepted the integration is `Conformant`; a rejected
/// manifest is a digest/version mismatch reported as `ProtocolDrift`; a mixed
/// result is `Drifted`. A clean result is complete-coverage only when at least
/// one component was actually probed.
#[must_use]
pub fn host_conformance_read(summary: &HostConformanceSummaryV1) -> HostIntegrationReadV1 {
    if summary.probed == 0 {
        return if summary.executable_present {
            // The executable is present but exposes no probeable component
            // manifest: undetermined conformance, not a healthy claim.
            HostIntegrationReadV1::Unknown
        } else {
            HostIntegrationReadV1::Absent
        };
    }
    if !summary.executable_present {
        return HostIntegrationReadV1::Observed {
            conformance: HostConformanceV1::ExecutableAbsent,
            coverage: DoctorCoverageCompletenessV1::Complete,
        };
    }
    let conformance = if summary.accepted == summary.probed {
        HostConformanceV1::Conformant
    } else if summary.accepted == 0 {
        HostConformanceV1::ProtocolDrift
    } else {
        HostConformanceV1::Drifted
    };
    HostIntegrationReadV1::Observed {
        conformance,
        coverage: DoctorCoverageCompletenessV1::Complete,
    }
}

fn host_integration_read_from_report(
    report: &crate::agents::host_bundle_v2::HostBundleDoctorReportV1,
) -> HostIntegrationReadV1 {
    use crate::agents::host_bundle_v2::HostBundleComponentDoctorStateV1;

    if report.native_edit_stop_conformance.is_empty() {
        return HostIntegrationReadV1::Unsupported;
    }
    if report.components.is_empty() {
        return HostIntegrationReadV1::Absent;
    }
    let conformance = if report.components.iter().any(|component| {
        matches!(
            component.state,
            HostBundleComponentDoctorStateV1::Corrupt
                | HostBundleComponentDoctorStateV1::OwnershipConflict
        )
    }) {
        HostConformanceV1::ProtocolDrift
    } else if report.components.iter().any(|component| {
        // `Drifted`, `OrphanedRegistration`, and `ActivationDeferred` are
        // repairable conformance, not protocol drift: the component's ownership
        // is intact and either the ordinary reinstall or the host's own
        // activation converges it, so none may escalate to `ProtocolDrift`.
        matches!(
            component.state,
            HostBundleComponentDoctorStateV1::Repairable
                | HostBundleComponentDoctorStateV1::Missing
                | HostBundleComponentDoctorStateV1::Drifted
                | HostBundleComponentDoctorStateV1::OrphanedRegistration
                | HostBundleComponentDoctorStateV1::ActivationDeferred
        )
    }) {
        HostConformanceV1::Drifted
    } else {
        HostConformanceV1::Conformant
    };
    HostIntegrationReadV1::Observed {
        conformance,
        coverage: DoctorCoverageCompletenessV1::Complete,
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

// === Canonical advisory feedback (Advisory family) ==========================

/// Project the latest exact-scope durable feedback publication into Doctor's
/// distinct advisory port. Host conformance remains a separate source.
#[must_use]
pub fn advisory_feedback_read_from_publication(
    publication: Option<&tracedecay_application::feedback::FeedbackCompletedPublicationV1>,
    current_generation: Option<&tracedecay_domain::CodeGenerationId>,
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

// === Code/semantic index mount (SemanticIndex family) ========================

/// The real code-index mount signal the daemon reads from its scheduler
/// registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeIndexMountSignalV1 {
    /// The worktree is mounted and a fresh complete generation is available.
    MountedFresh,
    /// The worktree is mounted but no complete generation is published yet.
    Indexing,
    /// The worktree is mounted but behind the current generation.
    Stale,
    /// No worktree is mounted for the project.
    Unmounted,
    /// The mounted index is incompatible with the current schema/generation.
    Incompatible,
}

/// Map a code-index mount signal into its kernel read.
#[must_use]
pub fn code_index_read(signal: CodeIndexMountSignalV1) -> CodeIndexMountReadV1 {
    let state = match signal {
        CodeIndexMountSignalV1::MountedFresh => CodeIndexMountStateV1::Mounted,
        CodeIndexMountSignalV1::Indexing => CodeIndexMountStateV1::Indexing,
        CodeIndexMountSignalV1::Stale => CodeIndexMountStateV1::Stale,
        CodeIndexMountSignalV1::Unmounted => CodeIndexMountStateV1::Unmounted,
        CodeIndexMountSignalV1::Incompatible => CodeIndexMountStateV1::Incompatible,
    };
    CodeIndexMountReadV1::Observed {
        state,
        coverage: DoctorCoverageCompletenessV1::Complete,
    }
}

/// Read the real code-index mount state from the daemon scheduler registry.
///
/// An unmounted worktree reports `Unmounted`; a mounted worktree whose freshness
/// ladder has already proven a complete generation current reports `Mounted`;
/// stale, restored-unverified, or busy schedulers report `Indexing` and schedule
/// background reconciliation. Doctor never performs code-index catch-up on its
/// request path.
pub(in crate::daemon) async fn code_index_read_from_registry(
    registry: &crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: &Path,
) -> CodeIndexMountReadV1 {
    if !registry.is_worktree_mounted(project_root).await {
        return code_index_read(CodeIndexMountSignalV1::Unmounted);
    }
    let signal = if registry.latest_complete_ready(project_root).await.is_some() {
        CodeIndexMountSignalV1::MountedFresh
    } else {
        CodeIndexMountSignalV1::Indexing
    };
    code_index_read(signal)
}

/// Adapter over the code/semantic index mount state (`SemanticIndex` family).
pub struct CodeIndexMountDoctorAdapterV1 {
    read: CodeIndexMountReadV1,
}

impl CodeIndexMountDoctorAdapterV1 {
    /// Build the adapter from an already-resolved kernel read.
    #[must_use]
    pub fn from_read(read: CodeIndexMountReadV1) -> Self {
        Self { read }
    }

    /// Build the adapter from a real code-index mount signal.
    #[must_use]
    pub fn from_signal(signal: CodeIndexMountSignalV1) -> Self {
        Self::from_read(code_index_read(signal))
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

// === Language server/analyzer (LanguageServer family) ========================

/// Map the daemon diagnostic broker's project-active engine statuses.
#[must_use]
pub fn language_server_read_from_engine_states(
    states: impl IntoIterator<Item = tracedecay_lsp::analyzer::broker::EngineState>,
) -> LanguageServerReadV1 {
    use tracedecay_lsp::analyzer::broker::EngineState;

    let states = states.into_iter().collect::<Vec<_>>();
    if states.is_empty() {
        return LanguageServerReadV1::Absent;
    }
    let state = if states.contains(&EngineState::Crashed) {
        LanguageServerStateV1::Crashed
    } else if states.contains(&EngineState::Unavailable) {
        LanguageServerStateV1::Unavailable
    } else if states.contains(&EngineState::Disabled) {
        LanguageServerStateV1::Disabled
    } else if states.contains(&EngineState::Refreshing) {
        LanguageServerStateV1::Refreshing
    } else if states.iter().all(|state| *state == EngineState::Ready) {
        LanguageServerStateV1::Ready
    } else {
        LanguageServerStateV1::Available
    };
    LanguageServerReadV1::Observed {
        state,
        coverage: DoctorCoverageCompletenessV1::Complete,
    }
}

/// Read live project-active analyzer state from the daemon diagnostic owner.
pub async fn language_server_read_from_broker(
    broker: &tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>,
) -> LanguageServerReadV1 {
    let statuses = broker.lock().await.project_engine_statuses();
    language_server_read_from_engine_states(statuses.into_iter().map(|status| status.state))
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

// === Canonical Plan-26 observations (Observability family) ===================

/// Map the canonical durable Plan-26 read model into a truthful Doctor read.
#[must_use]
pub fn observability_read_from_model(
    model: Result<
        crate::application::feedback::observations::FeedbackObservationReadModelV1,
        crate::application::feedback::concrete::Pr12FeedbackRuntimeError,
    >,
) -> ObservabilityReadV1 {
    match model {
        Ok(model)
            if model.total_count == 0
                && model.denominators.eligible == 0
                && model.denominators.incomplete_boots == 0
                && model.watermark.producer_boot_id.is_none() =>
        {
            ObservabilityReadV1::Absent
        }
        Ok(model) => {
            use crate::application::feedback::observations::Plan26CoverageV1;
            let (state, coverage) = match model.coverage {
                Plan26CoverageV1::Known => (
                    ObservabilityStateV1::Current,
                    DoctorCoverageCompletenessV1::Complete,
                ),
                Plan26CoverageV1::Stale => (
                    ObservabilityStateV1::Stale,
                    DoctorCoverageCompletenessV1::Partial,
                ),
                Plan26CoverageV1::Partial
                | Plan26CoverageV1::Sampled
                | Plan26CoverageV1::Capped => (
                    ObservabilityStateV1::Current,
                    DoctorCoverageCompletenessV1::Partial,
                ),
                Plan26CoverageV1::Unknown => (
                    ObservabilityStateV1::Current,
                    DoctorCoverageCompletenessV1::Unknown,
                ),
            };
            ObservabilityReadV1::Observed {
                state,
                total_count: model.total_count,
                last_observed_at_micros: model.watermark.observed_through.map(|value| value.0),
                coverage,
            }
        }
        Err(_) => ObservabilityReadV1::Unknown,
    }
}

/// Adapter over the canonical durable Plan-26 observation read model.
pub struct ObservabilityDoctorAdapterV1 {
    read: ObservabilityReadV1,
}

impl ObservabilityDoctorAdapterV1 {
    #[must_use]
    pub fn from_read(read: ObservabilityReadV1) -> Self {
        Self { read }
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
}

// === Storage retention/size (Storage family) =================================

/// Wrap a set of typed storage findings the daemon's Plan 38 producers emitted
/// into a kernel read.
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

/// Collect the daemon's read-only orphan-store Doctor findings for a profile.
///
/// Runs the Plan 38 §2 orphan-store sweep in classification-only mode (no
/// collection) and maps each classified orphan/re-linkable store onto the typed
/// kernel [`DoctorStorageFindingV1`] via the daemon's own
/// [`crate::doctor::registry_drift`] mapper — the same finding the applied
/// backstop sweep surfaces, read-only. Live stores are not a retention concern
/// and produce no finding.
pub async fn collect_orphan_store_findings(
    global_db: &crate::global_db::RegisteredGlobalDb,
    profile_root: &Path,
    retention_secs: i64,
    now: i64,
) -> DoctorStorageFamilyReadV1 {
    let report = crate::retention::orphan_stores::sweep_orphan_stores(
        global_db,
        profile_root,
        retention_secs,
        now,
        false,
    )
    .await;
    let Ok(report) = report else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    storage_family_read(
        report
            .plan
            .collect
            .iter()
            .chain(report.plan.retained_immature.iter())
            .chain(report.plan.relink.iter())
            .filter_map(crate::doctor::registry_drift::orphan_store_doctor_finding)
            .collect(),
    )
}

/// Collect the daemon's read-only unregistered-store-directory Doctor
/// findings for a profile (plan 38 §2's disjoint on-disk-only audit class —
/// a store directory with no `code_projects` row at all, invisible to the
/// registry-driven walk [`collect_orphan_store_findings`] performs). Runs the
/// bottom-up sweep in classification-only mode (no collection).
pub async fn collect_unregistered_store_findings(
    global_db: &crate::global_db::RegisteredGlobalDb,
    profile_root: &Path,
    retention_secs: i64,
    now: i64,
) -> DoctorStorageFamilyReadV1 {
    let report = crate::retention::orphan_stores::sweep_unregistered_stores(
        global_db,
        profile_root,
        retention_secs,
        now,
        false,
    )
    .await;
    let Ok(report) = report else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    storage_family_read(
        report
            .plan
            .collect
            .iter()
            .chain(report.plan.retained_immature.iter())
            .filter_map(crate::doctor::registry_drift::unregistered_store_doctor_finding)
            .collect(),
    )
}

/// Evaluate every owner-configured soft budget against the daemon's retained
/// project, registry, and session stores. A configured key that is not mounted
/// is emitted as typed unknown telemetry rather than silently omitted.
struct CollectedStoreTelemetryV1 {
    findings: DoctorStorageFamilyReadV1,
    table_growth_evidence: Vec<tracedecay_application::storage::TableGrowthDoctorEvidenceV1>,
}

const MAX_SYNCHRONOUS_TABLE_GROWTH_STORE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SYNCHRONOUS_EXHAUSTIVE_SCAN_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SYNCHRONOUS_EXHAUSTIVE_SCAN_ENTRIES: usize = 4_096;
/// Entry ceiling for the code-index generation census.
///
/// The census is metadata-only — a `stat` and a bounded manifest prefix per
/// generation — so its cost scales with the number of directory entries, not
/// with their size. Gating it on bytes instead (the previous
/// `MAX_SYNCHRONOUS_EXHAUSTIVE_SCAN_BYTES` budget) compared a 64 MiB ceiling
/// against generation files that are routinely ~1 GiB each, so the gate failed
/// on every real profile and the finding this kernel exists to produce was
/// structurally unreachable.
const MAX_SYNCHRONOUS_GENERATION_CENSUS_ENTRIES: usize = 4_096;

fn permits_synchronous_exhaustive_scan(root: &Path) -> bool {
    let mut pending = vec![root.to_path_buf()];
    let mut observed_bytes = 0_u64;
    let mut observed_entries = 0_usize;
    while let Some(path) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(path) else {
            return false;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return false;
            };
            observed_entries = observed_entries.saturating_add(1);
            if observed_entries > MAX_SYNCHRONOUS_EXHAUSTIVE_SCAN_ENTRIES {
                return false;
            }
            let Ok(file_type) = entry.file_type() else {
                return false;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                pending.push(entry.path());
                continue;
            }
            if !file_type.is_file() {
                return false;
            }
            let Ok(metadata) = entry.metadata() else {
                return false;
            };
            observed_bytes = observed_bytes.saturating_add(metadata.len());
            if observed_bytes > MAX_SYNCHRONOUS_EXHAUSTIVE_SCAN_BYTES {
                return false;
            }
        }
    }
    true
}

/// Whether the sealed-generation directory is small enough (in *entries*) for a
/// synchronous metadata census. Byte size is deliberately not consulted.
fn permits_synchronous_generation_census(generations_root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(generations_root) else {
        return false;
    };
    let mut observed_entries = 0_usize;
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let Ok(file_type) = entry.file_type() else {
            return false;
        };
        if file_type.is_symlink() {
            continue;
        }
        observed_entries = observed_entries.saturating_add(1);
        if observed_entries > MAX_SYNCHRONOUS_GENERATION_CENSUS_ENTRIES {
            return false;
        }
    }
    true
}

fn permits_synchronous_session_retention_backlog(database_path: &Path) -> bool {
    ["", "-wal", "-shm"]
        .into_iter()
        .try_fold(0_u64, |total, suffix| {
            let mut path = database_path.as_os_str().to_os_string();
            path.push(suffix);
            match std::fs::metadata(PathBuf::from(path)) {
                Ok(metadata) => total
                    .checked_add(metadata.len())
                    .filter(|size| *size <= MAX_SYNCHRONOUS_EXHAUSTIVE_SCAN_BYTES),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(total),
                Err(_) => None,
            }
        })
        .is_some()
}

fn permits_synchronous_table_growth(
    read: &tracedecay_application::storage::StorageTelemetryReadV1,
) -> bool {
    matches!(
        read,
        tracedecay_application::storage::StorageTelemetryReadV1::Observed { sample }
            if sample.total_bytes().get() <= MAX_SYNCHRONOUS_TABLE_GROWTH_STORE_BYTES
    )
}

async fn collect_over_budget_store_findings(
    context: &RequestContext,
    telemetry_ports: &[(
        tracedecay_application::storage::StoreKeyV1,
        tracedecay_rusqlite_runtime::SqliteStoreSizeTelemetryPort,
    )],
    retention: &crate::config::RetentionConfig,
) -> CollectedStoreTelemetryV1 {
    use std::collections::BTreeMap;
    use tracedecay_application::storage::{
        StorageTelemetryReadV1, StoreSizeTelemetryPort, TableGrowthTelemetryReadV1,
        over_budget_finding, table_growth_doctor_evidence, table_growth_finding,
    };

    let mut reads = BTreeMap::new();
    let mut table_growth_evidence = Vec::new();
    for (store, port) in telemetry_ports {
        let read = port.store_size(context, store).await;
        let table_growth = if permits_synchronous_table_growth(&read) {
            port.table_growth(context, store).await
        } else {
            TableGrowthTelemetryReadV1::Unknown {
                store: store.clone(),
            }
        };
        if let TableGrowthTelemetryReadV1::Observed { samples, .. } = &table_growth {
            for sample in samples {
                tracing::info!(
                    target: "tracedecay::storage_telemetry",
                    store = sample.store.as_str(),
                    table = sample.table.as_str(),
                    previous_bytes = sample.previous_bytes.0,
                    current_bytes = sample.current_bytes.0,
                    growth_bytes = sample.growth_bytes().0,
                    previous_observed_at = sample.previous_observed_at.0,
                    current_observed_at = sample.current_observed_at.0,
                    "observed SQLite table payload growth"
                );
            }
        }
        table_growth_evidence.extend(table_growth_doctor_evidence(&table_growth));
        reads.entry(store.as_str().to_owned()).or_insert(read);
    }

    let mut findings = Vec::new();
    for evidence in &table_growth_evidence {
        let Ok(finding) = table_growth_finding(evidence) else {
            return CollectedStoreTelemetryV1 {
                findings: DoctorStorageFamilyReadV1::Unknown,
                table_growth_evidence,
            };
        };
        findings.push(finding);
    }
    for configured_store in retention.store_soft_budgets_bytes.keys() {
        let Ok(Some(budget)) = retention.store_soft_budget(configured_store) else {
            return CollectedStoreTelemetryV1 {
                findings: DoctorStorageFamilyReadV1::Unknown,
                table_growth_evidence,
            };
        };
        let read =
            reads
                .remove(configured_store)
                .unwrap_or_else(|| StorageTelemetryReadV1::Unknown {
                    store: budget.store.clone(),
                });
        let Ok(finding) =
            over_budget_finding(&budget, &read, DoctorCoverageCompletenessV1::Complete)
        else {
            return CollectedStoreTelemetryV1 {
                findings: DoctorStorageFamilyReadV1::Unknown,
                table_growth_evidence,
            };
        };
        findings.push(finding);
    }
    CollectedStoreTelemetryV1 {
        findings: storage_family_read(findings),
        table_growth_evidence,
    }
}

/// Observe current-project branch stores against live local refs.
pub fn collect_stale_branch_store_findings(
    project_root: &Path,
    layout: &crate::storage::StoreLayout,
) -> DoctorStorageFamilyReadV1 {
    use tracedecay_application::storage::{
        BranchRefV1, StaleBranchDbRecordV1, StorageByteSizeV1, StoreKeyV1, stale_branch_dbs_finding,
    };

    if !layout.branch_meta_path.exists() {
        return DoctorStorageFamilyReadV1::Absent;
    }
    let Some(meta) = crate::branch_meta::load_branch_meta(&layout.data_root) else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let mut findings = Vec::new();
    for (branch, entry) in meta.branches {
        if branch == meta.default_branch || entry.gc_protected {
            continue;
        }
        let Ok(store) = StoreKeyV1::new(entry.db_file.clone()) else {
            return DoctorStorageFamilyReadV1::Unknown;
        };
        let Ok(branch_ref) = BranchRefV1::new(branch.clone()) else {
            return DoctorStorageFamilyReadV1::Unknown;
        };
        let db_path = layout.data_root.join(&entry.db_file);
        let size_bytes = ["", "-wal", "-shm"]
            .into_iter()
            .map(|suffix| {
                let mut path = db_path.as_os_str().to_os_string();
                path.push(suffix);
                std::fs::metadata(PathBuf::from(path))
            })
            .filter_map(Result::ok)
            .map(|metadata| metadata.len())
            .fold(0_u64, u64::saturating_add);
        let record = StaleBranchDbRecordV1 {
            store,
            branch: branch_ref,
            ref_present: crate::branch::is_branch_ref_present(project_root, &branch),
            size_bytes: StorageByteSizeV1(size_bytes),
        };
        let Ok(finding) = stale_branch_dbs_finding(&record, DoctorCoverageCompletenessV1::Complete)
        else {
            return DoctorStorageFamilyReadV1::Unknown;
        };
        findings.push(finding);
    }
    storage_family_read(findings)
}

/// Scan every registered profile-sharded store for loose or quarantined
/// recovery debris and map the exhaustive census through the Plan 38 producer.
pub async fn collect_incident_debris_findings(
    registry: &crate::global_db::RegisteredGlobalDb,
    profile_root: &Path,
    observed_at_secs: i64,
) -> DoctorStorageFamilyReadV1 {
    let Ok(census) =
        crate::retention::orphan_stores::build_store_census(registry, profile_root).await
    else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let mut findings = Vec::new();
    for entry in &census {
        let Ok(scan) = crate::retention::incident_debris::scan_incident_debris(
            entry,
            profile_root,
            observed_at_secs,
        ) else {
            return DoctorStorageFamilyReadV1::Unknown;
        };
        let Ok(finding) = tracedecay_application::storage::incident_debris_finding(&scan) else {
            return DoctorStorageFamilyReadV1::Unknown;
        };
        findings.push(finding);
    }
    storage_family_read(findings)
}

/// Read the configured session-retention backlog from the retained session
/// store. This mirrors the retention SQL in read-only form and emits clean
/// zero-byte records when a configured window has no eligible rows.
pub async fn collect_retention_backlog_findings(
    profile_sessions: &crate::global_db::RegisteredGlobalDb,
    retention: &crate::config::RetentionConfig,
    observed_at_secs: i64,
) -> DoctorStorageFamilyReadV1 {
    if !permits_synchronous_session_retention_backlog(profile_sessions.db_path()) {
        return DoctorStorageFamilyReadV1::Unknown;
    }
    let Some(file_name) = profile_sessions
        .db_path()
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let Ok(store) = tracedecay_application::storage::StoreKeyV1::new(file_name.to_owned()) else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let Ok(snapshot) = profile_sessions.read_snapshot().await else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let Ok(records) = crate::sessions::lcm::retention::read_session_retention_backlog(
        &snapshot,
        store,
        &retention.session_lcm,
        observed_at_secs,
    )
    .await
    else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let mut findings = Vec::new();
    for record in records {
        let Ok(finding) = tracedecay_application::storage::retention_backlog_finding(
            &record,
            DoctorCoverageCompletenessV1::Complete,
        ) else {
            return DoctorStorageFamilyReadV1::Unknown;
        };
        findings.push(finding);
    }
    storage_family_read(findings)
}

/// Read the exact code-generation liveness plan and surface superseded,
/// collectable, and stranded-scope bytes through Doctor. These are ordinary
/// files, not `SQLite` tables, so dbstat/table attribution cannot observe them.
///
/// The census is metadata-only by construction: gating this family on a byte
/// budget made the finding unreachable on every profile that actually had
/// something to report, because one sealed generation alone exceeds any budget
/// small enough to be called cheap.
pub async fn collect_code_generation_retention_findings(
    graph: &crate::db::Database,
    code_index_store_root: &Path,
    project_root: &Path,
) -> DoctorStorageFamilyReadV1 {
    use crate::retention::code_index_generations::{
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS, DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        GenerationDigestVerificationV1, ScopeRootRetentionPlanV1,
        plan_code_generation_retention_with_verification, plan_scope_root_retention,
    };
    use crate::semantic_code::legacy_migration::LegacyVectorInventoryPortV1;
    use crate::store::vector_generations::DatabaseVectorGenerationStoreV1;
    use tracedecay_application::storage::{
        CodeGenerationRetentionRecordV1, StorageByteSizeV1, StoreKeyV1,
        code_generation_retention_finding,
    };

    if !code_index_store_root
        .join("active-code-generation-v1.json")
        .is_file()
    {
        return DoctorStorageFamilyReadV1::Absent;
    }
    if !permits_synchronous_generation_census(&code_index_store_root.join("code-generations-v1")) {
        return DoctorStorageFamilyReadV1::Unknown;
    }
    let Ok(store) = DatabaseVectorGenerationStoreV1::open(graph).await else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let Ok(inventory) = store.read_legacy_inventory().await else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let Ok(inventory) = inventory.read_only_inventory() else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let vector_readable_sources = inventory.retained_readable_sources();
    let root = code_index_store_root.to_path_buf();
    // The shared parent that holds every scope root for this repository. A
    // stranded sibling scope is invisible to the scope-local census above, so
    // it is measured here or it is not measured anywhere.
    let scope_store_root = code_index_store_root.parent().map(Path::to_path_buf);
    let project_root = project_root.to_path_buf();
    let now = now_secs();
    let Ok(census) = tokio::task::spawn_blocking(move || {
        let plan = plan_code_generation_retention_with_verification(
            &root,
            &vector_readable_sources,
            DEFAULT_SUPERSEDED_GENERATION_FLOOR,
            GenerationDigestVerificationV1::MetadataOnly,
        );
        // Zeros are only ever published together with `Partial`: a live-root set
        // that could not be proven must never read as "nothing is stranded".
        let scopes = scope_store_root.and_then(|scope_store_root| {
            let live_roots =
                super::store_maintenance::resolve_live_code_index_roots(&project_root).ok()?;
            plan_scope_root_retention(
                &scope_store_root,
                &live_roots,
                DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS,
                now,
            )
            .ok()
        });
        (plan, scopes)
    })
    .await
    else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let (plan, scopes) = census;
    let Ok(plan) = plan else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let Ok(store) = StoreKeyV1::new("code-index-v1") else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let completeness = if scopes.is_some() {
        DoctorCoverageCompletenessV1::Complete
    } else {
        DoctorCoverageCompletenessV1::Partial
    };
    let record = CodeGenerationRetentionRecordV1 {
        store,
        superseded_generation_count: plan.superseded_generations.len() as u64,
        superseded_generation_bytes: StorageByteSizeV1(plan.superseded_generation_bytes()),
        collectable_generation_count: plan.collectable_generations.len() as u64,
        collectable_generation_bytes: StorageByteSizeV1(plan.collectable_generation_bytes()),
        stranded_scope_count: scopes
            .as_ref()
            .map_or(0, ScopeRootRetentionPlanV1::stranded_scope_count),
        stranded_scope_bytes: StorageByteSizeV1(
            scopes
                .as_ref()
                .map_or(0, ScopeRootRetentionPlanV1::stranded_scope_bytes),
        ),
    };
    let Ok(finding) = code_generation_retention_finding(&record, completeness) else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    storage_family_read(vec![finding])
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

    /// Build the adapter from a set of typed storage findings.
    #[must_use]
    pub fn from_findings(findings: Vec<DoctorStorageFindingV1>) -> Self {
        Self::from_read(storage_family_read(findings))
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

// === Composer factory ========================================================

/// The seven resolved kernel reads a daemon-owned Doctor report composes from.
///
/// A surface (the dashboard doctor-findings handler, the MCP doctor tools)
/// builds this bundle from the real signals it can reach — via the `*_read` /
/// `*_read_from_*` mappers in this module — and hands it to
/// [`compose_doctor_report`]. A signal the surface cannot obtain carries its
/// honest typed absence rather than a fabricated healthy read.
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
    /// Live daemon language-server/analyzer read (`LanguageServer` family).
    pub language_server: LanguageServerReadV1,
    /// Code/semantic index mount read (`SemanticIndex` family).
    pub code_index: CodeIndexMountReadV1,
    /// Canonical durable Plan-26 feedback read (`Observability` family).
    pub observability: ObservabilityReadV1,
    /// Storage retention/size read (Storage family).
    pub storage: DoctorStorageFamilyReadV1,
}

impl DoctorKernelInputsV1 {
    /// A bundle in which every source is honestly undetermined.
    ///
    /// A surface with no daemon handles at all starts here rather than
    /// fabricating a healthy report; each family is then filled in as its real
    /// signal becomes reachable.
    #[must_use]
    pub fn all_unknown() -> Self {
        Self {
            configuration: ConfigurationAuthorityReadV1::Unknown,
            runtime: RuntimeHealthReadV1::Unknown,
            operational_audit: OperationalAuditReadV1 {
                remote: RemoteOperationalReadV1::Unavailable,
                profile_authority: ProfileAuthorityReadV1::Unavailable,
            },
            host: HostIntegrationReadV1::Unknown,
            advisory_feedback: AdvisoryFeedbackReadV1::Unknown,
            language_server: LanguageServerReadV1::Unknown,
            code_index: CodeIndexMountReadV1::Unknown,
            observability: ObservabilityReadV1::Unknown,
            storage: DoctorStorageFamilyReadV1::Unknown,
        }
    }
}

/// Compose a Doctor report from the daemon-owned source adapters.
///
/// Wires all seven adapters into the kernel [`DoctorReportComposerV1`] and
/// composes. The composer enumerates every finding family truthfully: a family
/// whose read is unavailable is carried with its real evidence state and an
/// explicit coverage record, and the report asserts health only when every
/// family was consulted with complete coverage and every finding is healthy.
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
    let observability = ObservabilityDoctorAdapterV1::from_read(inputs.observability.clone());
    let storage = StorageDoctorAdapterV1::from_read(inputs.storage.clone());

    let composer = DoctorReportComposerV1::new()
        .with_configuration(&configuration)
        .with_runtime(&runtime)
        .with_operational_audit(&operational_audit)
        .with_host(&host)
        .with_advisory_feedback(&advisory_feedback)
        .with_language_server(&language_server)
        .with_code_index(&code_index)
        .with_observability(&observability)
        .with_storage(&storage);

    composer.compose(context).await
}

type CachedStoreTelemetryPort = (
    ManifestDigest,
    tracedecay_rusqlite_runtime::SqliteStoreSizeTelemetryPort,
);

#[derive(Default)]
struct StoreTelemetryPortCache {
    ports: HashMap<PathBuf, CachedStoreTelemetryPort>,
}

fn cached_store_telemetry_port<E>(
    cache: &Mutex<StoreTelemetryPortCache>,
    path: &Path,
    scope: &tracedecay_application::ResolvedScope,
    open: impl FnOnce() -> Result<tracedecay_rusqlite_runtime::exact_sql::ExactSqlHandle, E>,
) -> Option<(
    tracedecay_application::storage::StoreKeyV1,
    tracedecay_rusqlite_runtime::SqliteStoreSizeTelemetryPort,
)> {
    let store =
        tracedecay_application::storage::StoreKeyV1::new(path.file_name()?.to_str()?.to_owned())
            .ok()?;
    let mut cache = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((digest, port)) = cache.ports.get(path)
        && digest == &scope.scope_digest
    {
        return Some((store, port.clone()));
    }
    let port = tracedecay_rusqlite_runtime::SqliteStoreSizeTelemetryPort::new(
        open().ok()?,
        store.clone(),
        scope.clone(),
        Duration::from_secs(5),
    );
    cache.ports.insert(
        path.to_path_buf(),
        (scope.scope_digest.clone(), port.clone()),
    );
    Some((store, port))
}

/// Build the daemon-owned live Doctor reader installed into a project MCP
/// server. Every read re-resolves exact project/worktree identity, observes the
/// current registered runtimes, and composes through the sole application
/// kernel. The dashboard receives no database handles or authority-bearing
/// inputs.
#[allow(clippy::too_many_arguments)]
pub(in crate::daemon) fn production_doctor_report_reader(
    project_root: PathBuf,
    project_id: tracedecay_domain::ProjectId,
    layout: crate::storage::StoreLayout,
    graph: crate::db::Database,
    registry: Arc<crate::global_db::RegisteredGlobalDb>,
    profile_sessions: Arc<crate::global_db::RegisteredGlobalDb>,
    project_sessions: Arc<crate::global_db::RegisteredGlobalDb>,
    profile_root: PathBuf,
    host_home: Option<PathBuf>,
    remote_operational: RemoteOperationalReadV1,
    retention: crate::config::RetentionConfig,
    schedulers: crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    diagnostic_broker: Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>,
    feedback_runtimes: crate::daemon::service::invocation::DaemonFeedbackRuntimeRegistrar,
) -> crate::dashboard::DoctorReportReader {
    let store_telemetry_ports = Arc::new(Mutex::new(StoreTelemetryPortCache::default()));
    Arc::new(move || {
        let project_root = project_root.clone();
        let project_id = project_id.clone();
        let layout = layout.clone();
        let graph = graph.clone();
        let registry = Arc::clone(&registry);
        let profile_sessions = Arc::clone(&profile_sessions);
        let project_sessions = Arc::clone(&project_sessions);
        let profile_root = profile_root.clone();
        let host_home = host_home.clone();
        let remote_operational = remote_operational.clone();
        let retention = retention.clone();
        let schedulers = schedulers.clone();
        let diagnostic_broker = Arc::clone(&diagnostic_broker);
        let feedback_runtimes = feedback_runtimes.clone();
        let store_telemetry_ports = Arc::clone(&store_telemetry_ports);
        Box::pin(async move {
            let scope =
                super::project_open_owners::resolved_scope_for_project(&project_root, &project_id)
                    .map_err(|_| ApplicationContractError::Inconsistent {
                        field: "daemon Doctor project scope",
                    })?;
            let context = doctor_report_request_context(scope)?;
            let mut telemetry_ports = Vec::new();
            let mut telemetry_paths = BTreeSet::new();
            if telemetry_paths.insert(graph.database_path().to_path_buf())
                && let Some(port) = cached_store_telemetry_port(
                    &store_telemetry_ports,
                    graph.database_path(),
                    context.scope(),
                    || graph.storage_telemetry_handle(),
                )
            {
                telemetry_ports.push(port);
            }
            for database in [
                registry.as_ref(),
                profile_sessions.as_ref(),
                project_sessions.as_ref(),
            ] {
                if telemetry_paths.insert(database.db_path().to_path_buf())
                    && let Some(port) = cached_store_telemetry_port(
                        &store_telemetry_ports,
                        database.db_path(),
                        context.scope(),
                        || database.storage_telemetry_handle(),
                    )
                {
                    telemetry_ports.push(port);
                }
            }
            let pinned = crate::config::runtime_configuration_for_layout(&project_root, &layout);
            let quick_check_ok = graph
                .quick_check_report()
                .await
                .ok()
                .map(|problem| problem.is_none());
            let graph_authority_current = graph.write_authority().is_ok_and(|authority| {
                authority
                    .require_active_write_scope("read dashboard Doctor graph authority")
                    .is_ok()
            });
            let registered_authority_current = registry.writer_connection().is_ok()
                && profile_sessions.writer_connection().is_ok();
            let temporal = profile_sessions.session_temporal_doctor_health().await;
            let temporal_ok = match temporal.status() {
                crate::global_db::session_temporal::SessionTemporalHealthStatus::Complete => {
                    Some(temporal.findings().is_empty())
                }
                crate::global_db::session_temporal::SessionTemporalHealthStatus::Partial
                | crate::global_db::session_temporal::SessionTemporalHealthStatus::Unavailable
                | crate::global_db::session_temporal::SessionTemporalHealthStatus::Locked => None,
            };
            let retention_secs = retention
                .orphan_store_gc_days
                .and_then(|days| i64::try_from(days).ok())
                .and_then(|days| days.checked_mul(24 * 60 * 60))
                .unwrap_or(i64::MAX);
            let now = now_secs();
            let permits_profile_census =
                permits_synchronous_exhaustive_scan(&profile_root.join("projects"));
            let orphan = if permits_profile_census {
                collect_orphan_store_findings(registry.as_ref(), &profile_root, retention_secs, now)
                    .await
            } else {
                DoctorStorageFamilyReadV1::Unknown
            };
            let unregistered = if permits_profile_census {
                collect_unregistered_store_findings(
                    registry.as_ref(),
                    &profile_root,
                    retention_secs,
                    now,
                )
                .await
            } else {
                DoctorStorageFamilyReadV1::Unknown
            };
            let store_telemetry =
                collect_over_budget_store_findings(&context, &telemetry_ports, &retention).await;
            let stale_branches = collect_stale_branch_store_findings(&project_root, &layout);
            let incident_debris = if permits_profile_census {
                collect_incident_debris_findings(registry.as_ref(), &profile_root, now).await
            } else {
                DoctorStorageFamilyReadV1::Unknown
            };
            let profile_retention_backlog =
                collect_retention_backlog_findings(profile_sessions.as_ref(), &retention, now)
                    .await;
            let project_retention_backlog =
                collect_retention_backlog_findings(project_sessions.as_ref(), &retention, now)
                    .await;
            let code_index_store_root = super::code_index_scheduler::scoped_code_index_store_root(
                &layout.data_root.join("code-index-v1"),
                &project_root,
            );
            let code_generation_retention = collect_code_generation_retention_findings(
                &graph,
                &code_index_store_root,
                &project_root,
            )
            .await;
            let storage = [
                orphan,
                unregistered,
                store_telemetry.findings,
                stale_branches,
                incident_debris,
                profile_retention_backlog,
                project_retention_backlog,
                code_generation_retention,
            ]
            .into_iter()
            .reduce(merge_storage_reads)
            .unwrap_or(DoctorStorageFamilyReadV1::Absent);
            let language_server = language_server_read_from_broker(&diagnostic_broker).await;
            let observability = observability_read_from_model(
                crate::application::feedback::concrete::plan26_feedback_observation_read_model(
                    &graph,
                )
                .await,
            );
            let current_generation = schedulers
                .latest_complete_ready(&project_root)
                .await
                .map(|latest| latest.generation().manifest().generation_id.clone());
            let advisory_feedback = match feedback_runtimes.doctor_read_store(&project_root).await {
                Some(store) => match store.doctor_latest_publication(&context).await {
                    Ok(publication) => advisory_feedback_read_from_publication(
                        publication.as_ref(),
                        current_generation.as_ref(),
                    ),
                    Err(_) => AdvisoryFeedbackReadV1::Unknown,
                },
                None => AdvisoryFeedbackReadV1::Absent,
            };
            let host = host_home
                .as_ref()
                .map_or(HostIntegrationReadV1::Unsupported, |home| {
                    let context = crate::agents::HealthcheckContext {
                        home: home.clone(),
                        project_path: project_root.clone(),
                    };
                    crate::agents::inspect_receipt_backed_host_components(
                        &context,
                        &profile_root.join("host-components"),
                    )
                    .as_ref()
                    .map_or(
                        HostIntegrationReadV1::Unknown,
                        host_integration_read_from_report,
                    )
                });
            let inputs = DoctorKernelInputsV1 {
                configuration: configuration_read_from_pin::<crate::errors::TraceDecayError>(
                    &pinned,
                ),
                runtime: runtime_health_read(&DaemonRuntimeHealthSignalV1 {
                    serving: true,
                    startup_converged: graph_authority_current && registered_authority_current,
                    quick_check_ok,
                    // The observation-authority audit is the exhaustive invariant
                    // pass (`validate_observation_authority_connection`) that the
                    // CLI and core Doctor routes run only when a caller asks for
                    // it. This reader does not run it, so the signal is not-run —
                    // `None` — rather than a boolean re-derived from schema and
                    // write-scope currency, which is a different question and is
                    // already reported through `startup_converged`. A not-run
                    // signal drops runtime coverage to partial, exactly as the
                    // coverage split intends.
                    authority_audit_ok: None,
                    temporal_ok,
                }),
                operational_audit: OperationalAuditReadV1 {
                    remote: remote_operational,
                    profile_authority: ProfileAuthorityReadV1::Observed {
                        registry_attached: registry.writer_connection().is_ok(),
                        profile_sessions_attached: profile_sessions.writer_connection().is_ok(),
                        coverage: DoctorCoverageCompletenessV1::Complete,
                    },
                },
                host,
                advisory_feedback,
                language_server,
                code_index: code_index_read_from_registry(&schedulers, &project_root).await,
                observability,
                storage,
            };
            let report = compose_doctor_report(&context, &inputs).await?;
            Ok(crate::dashboard::AdmittedDoctorReportV1::new(report)
                .with_table_growth_evidence(store_telemetry.table_growth_evidence))
        })
    })
}

pub(in crate::daemon) fn production_doctor_remediation_dispatcher(
    owners: ProductionDoctorRemediationOwnersV1,
    report_reader: crate::dashboard::DoctorReportReader,
) -> crate::dashboard::DoctorRemediationDispatcherV1 {
    use crate::dashboard::{
        DoctorRemediationDispatchErrorV1, DoctorRemediationDispatcherV1,
        DoctorRemediationLegalActionV1,
    };
    use tracedecay_application::doctor::{DoctorRemediationKindV1, DoctorRemediationRegistryV1};

    let legal_owners = owners.clone();
    let legal_actions: crate::dashboard::doctor_remediation_api::LegalActions = Arc::new(
        move |reference| {
            let owners = legal_owners.clone();
            Box::pin(async move {
                if !owners.route_registered.load(Ordering::Acquire)
                    || super::project_open_owners::resolved_scope_for_project(
                        &owners.project_root,
                        &owners.project_id,
                    )
                    .is_err()
                {
                    return Vec::new();
                }
                let registry = DoctorRemediationRegistryV1::default_registry();
                let Ok(descriptor) = registry.resolve(&reference) else {
                    return Vec::new();
                };
                let mounted = match descriptor.surface() {
                    tracedecay_application::doctor::DoctorOwningSurfaceV1::ConfigurationControlPlane => {
                        owners
                            .invocation
                            .configuration_runtime_registrar()
                            .doctor_owner_mounted(&owners.project_root)
                            .await
                    }
                    tracedecay_application::doctor::DoctorOwningSurfaceV1::StorageRuntime => {
                        owners.registry.writer_connection().is_ok()
                            && owners.profile_sessions.writer_connection().is_ok()
                            && owners.project_sessions.writer_connection().is_ok()
                    }
                    tracedecay_application::doctor::DoctorOwningSurfaceV1::DaemonRuntime => true,
                    tracedecay_application::doctor::DoctorOwningSurfaceV1::HostIntegration => {
                        crate::agents::home_dir().is_some()
                            && crate::agents::host_bundle_v2::resolved_host_bundle_lifecycle_root()
                                .is_ok()
                    }
                    tracedecay_application::doctor::DoctorOwningSurfaceV1::SemanticIndexRuntime => {
                        true
                    }
                };
                if !mounted {
                    return Vec::new();
                }
                match reference.kind() {
                    DoctorRemediationKindV1::Preview if descriptor.preview_available() => {
                        vec![DoctorRemediationLegalActionV1::RequestPreview]
                    }
                    DoctorRemediationKindV1::Action => {
                        let mut actions = vec![DoctorRemediationLegalActionV1::RequestApply];
                        if descriptor.preview_available() {
                            actions.push(DoctorRemediationLegalActionV1::RequestPreview);
                        }
                        actions
                    }
                    DoctorRemediationKindV1::Preview => Vec::new(),
                }
            })
        },
    );
    let dispatch_owners = owners.clone();
    let dispatch: crate::dashboard::doctor_remediation_api::Dispatch = Arc::new(move |command| {
        let owners = dispatch_owners.clone();
        Box::pin(async move {
            if !owners.route_registered.load(Ordering::Acquire) {
                return Err(DoctorRemediationDispatchErrorV1::Denied);
            }
            let scope = super::project_open_owners::resolved_scope_for_project(
                &owners.project_root,
                &owners.project_id,
            )
            .map_err(|_| DoctorRemediationDispatchErrorV1::Denied)?;
            dispatch_doctor_owner_operation(&owners, scope, command).await
        })
    });
    let observation: crate::dashboard::doctor_remediation_api::Observation =
        Arc::new(move |operation| {
            let report_reader = Arc::clone(&report_reader);
            Box::pin(async move {
                let report = report_reader()
                    .await
                    .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
                verify_doctor_remediation_observation(&report.report, &operation)
            })
        });
    DoctorRemediationDispatcherV1::new_durable(
        owners
            .layout
            .dashboard_root
            .join("doctor-remediation-operations"),
        legal_actions,
        dispatch,
        observation,
    )
}

fn verify_doctor_remediation_observation(
    report: &DoctorReportV1,
    operation: &crate::dashboard::DoctorRemediationOperationV1,
) -> Result<
    crate::dashboard::DoctorRemediationVerificationV1,
    crate::dashboard::DoctorRemediationDispatchErrorV1,
> {
    use crate::dashboard::{DoctorRemediationDispatchErrorV1, DoctorRemediationVerificationV1};
    use tracedecay_application::doctor::{
        DoctorCoverageCompletenessV1, DoctorEvidenceStateV1, DoctorFamilyConsultationV1,
        DoctorFindingFamilyV1, operations,
    };

    let family = match operation.owning_operation.as_str() {
        operations::CONFIGURATION_PROTECTED_APPLY | operations::CONFIGURATION_PIN_AUTHORITY => {
            DoctorFindingFamilyV1::Configuration
        }
        operations::RUNTIME_RECOVER_DAEMON => DoctorFindingFamilyV1::StorageRuntime,
        operations::HOST_REPAIR_INTEGRATION => DoctorFindingFamilyV1::Advisory,
        operations::CODE_INDEX_REMOUNT => DoctorFindingFamilyV1::SemanticIndex,
        operations::STORAGE_RETENTION_COLLECT
        | operations::STORAGE_COLLECT_ORPHAN_STORE
        | operations::STORAGE_BRANCH_GC
        | operations::STORAGE_QUARANTINE_AND_COLLECT_DEBRIS => DoctorFindingFamilyV1::Storage,
        _ => return Err(DoctorRemediationDispatchErrorV1::InvalidReference),
    };
    let observation_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.doctor-remediation-reobservation.v1",
        &operation.operation_id,
        &operation.owning_operation,
        report,
    ))
    .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    let family_coverage = report
        .coverage()
        .families()
        .iter()
        .find(|coverage| coverage.family() == family)
        .ok_or(DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    if let DoctorFamilyConsultationV1::Unavailable { reason } = family_coverage.consultation() {
        return Ok(match reason {
            tracedecay_application::doctor::DoctorFamilyUnavailableReasonV1::Denied => {
                DoctorRemediationVerificationV1::Denied
            }
            _ => DoctorRemediationVerificationV1::Unavailable,
        });
    }
    let findings = report
        .entries()
        .iter()
        .filter(|entry| entry.finding().family() == family)
        .map(tracedecay_application::DoctorReportEntryV1::finding)
        .collect::<Vec<_>>();
    if findings.is_empty() {
        return Ok(DoctorRemediationVerificationV1::Unavailable);
    }
    if findings
        .iter()
        .any(|finding| finding.state() == DoctorEvidenceStateV1::Denied)
    {
        return Ok(DoctorRemediationVerificationV1::Denied);
    }
    if findings.iter().any(|finding| {
        finding
            .remediation()
            .is_some_and(|reference| reference.owning_operation() == &operation.owning_operation)
    }) {
        return Ok(DoctorRemediationVerificationV1::Failed {
            observation_digest: Some(observation_digest),
        });
    }
    let complete = findings.iter().all(|finding| {
        finding.state() == DoctorEvidenceStateV1::HealthyCompleteCoverage
            && finding.coverage().completeness() == DoctorCoverageCompletenessV1::Complete
    });
    Ok(if complete {
        DoctorRemediationVerificationV1::Verified { observation_digest }
    } else {
        DoctorRemediationVerificationV1::Partial {
            observation_digest: Some(observation_digest),
        }
    })
}

async fn dispatch_doctor_owner_operation(
    owners: &ProductionDoctorRemediationOwnersV1,
    scope: tracedecay_application::ResolvedScope,
    command: crate::dashboard::DoctorRemediationDispatchCommandV1,
) -> Result<
    crate::dashboard::DoctorRemediationOperationV1,
    crate::dashboard::DoctorRemediationDispatchErrorV1,
> {
    use crate::application_surface::{ApplicationSurfaceOperation, ConfigurationSurfaceRequest};
    use crate::dashboard::{
        DoctorRemediationDispatchCommandV1, DoctorRemediationDispatchErrorV1,
        DoctorRemediationOperationPhaseV1, DoctorRemediationOperationV1, DoctorRemediationTargetV1,
        DoctorRemediationVerificationV1,
    };

    let operation_id =
        crate::dashboard::doctor_remediation_api::operation_id_for_command(&command)?;
    let request_id = operation_id.request_id().clone();
    let (operation, target, preview_id, idempotency_key, apply, recovering) = match command {
        DoctorRemediationDispatchCommandV1::Preview { operation, target } => {
            (operation, target, None, None, false, false)
        }
        DoctorRemediationDispatchCommandV1::Apply {
            operation,
            target,
            preview_id,
            idempotency_key,
        } => (
            operation,
            target,
            preview_id,
            Some(idempotency_key),
            true,
            false,
        ),
        DoctorRemediationDispatchCommandV1::Resume {
            operation,
            target,
            preview_id,
            idempotency_key,
        } => (
            operation,
            target,
            preview_id,
            Some(idempotency_key),
            true,
            true,
        ),
        DoctorRemediationDispatchCommandV1::Status { .. } => {
            return Err(DoctorRemediationDispatchErrorV1::OwnerUnavailable);
        }
    };
    let started_at = now_micros();
    let mut owner_execution = None;
    let mut owner_effect = None;
    let mut owner_preview = None;
    let mut owner_observation = None;
    let mut effect_unknown = false;
    match (&target, apply) {
        (DoctorRemediationTargetV1::ConfigurationProtectedPreview(request), false) => {
            match owners
                .invocation
                .configuration_runtime_registrar()
                .doctor_execute(
                    &owners.project_root,
                    &request_id,
                    ApplicationSurfaceOperation::ConfigurationProtectedPreview,
                    ConfigurationSurfaceRequest::ProtectedPreview(request.clone()),
                )
                .await
            {
                super::service::invocation::DoctorConfigurationOutcomeV1::Preview {
                    preview_id,
                    execution,
                } => {
                    owner_preview = Some(preview_id);
                    owner_execution = Some(execution);
                }
                super::service::invocation::DoctorConfigurationOutcomeV1::Denied => {
                    return Err(DoctorRemediationDispatchErrorV1::Denied);
                }
                _ => return Err(DoctorRemediationDispatchErrorV1::OwnerUnavailable),
            }
        }
        (DoctorRemediationTargetV1::ConfigurationProtectedApply(request), true) => {
            match owners
                .invocation
                .configuration_runtime_registrar()
                .doctor_execute(
                    &owners.project_root,
                    &request_id,
                    ApplicationSurfaceOperation::ConfigurationProtectedApply,
                    ConfigurationSurfaceRequest::ProtectedApply(request.clone()),
                )
                .await
            {
                super::service::invocation::DoctorConfigurationOutcomeV1::Effect {
                    execution,
                    receipt,
                } => {
                    owner_observation = Some(doctor_owner_observation(
                        "tracedecay.doctor.configuration-protected-apply.v1",
                        &receipt,
                    )?);
                    owner_execution = Some(execution);
                    owner_effect = Some(*receipt);
                }
                super::service::invocation::DoctorConfigurationOutcomeV1::Denied => {
                    return Err(DoctorRemediationDispatchErrorV1::Denied);
                }
                _ => return Err(DoctorRemediationDispatchErrorV1::OwnerUnavailable),
            }
        }
        (DoctorRemediationTargetV1::StorageRetentionCollect, false) => {
            let global = crate::retention::global_retention_report(
                owners.registry.as_ref(),
                &owners.global_retention,
                now_secs(),
            )
            .await
            .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
            let profile =
                run_doctor_session_retention(owners, owners.profile_sessions.as_ref(), false)
                    .await?;
            let project =
                run_doctor_session_retention(owners, owners.project_sessions.as_ref(), false)
                    .await?;
            owner_observation = Some(doctor_owner_observation(
                "tracedecay.doctor.storage-retention.preview.v1",
                &(global, profile, project),
            )?);
        }
        (DoctorRemediationTargetV1::StorageRetentionCollect, true) => {
            let global = crate::retention::prune_global_retention(
                owners.registry.as_ref(),
                &owners.global_retention,
                now_secs(),
            )
            .await
            .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
            let profile =
                run_doctor_session_retention(owners, owners.profile_sessions.as_ref(), true)
                    .await?;
            let project =
                run_doctor_session_retention(owners, owners.project_sessions.as_ref(), true)
                    .await?;
            owner_observation = Some(doctor_owner_observation(
                "tracedecay.doctor.storage-retention.apply.v1",
                &(global, profile, project),
            )?);
        }
        (DoctorRemediationTargetV1::StorageCollectOrphanStore, apply) => {
            let retention_secs =
                retention_window_secs(owners.config.sync.retention.orphan_store_gc_days);
            let orphan = crate::retention::orphan_stores::sweep_orphan_stores(
                owners.registry.as_ref(),
                &owners.profile_root,
                retention_secs,
                now_secs(),
                apply,
            )
            .await
            .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
            let unregistered = crate::retention::orphan_stores::sweep_unregistered_stores(
                owners.registry.as_ref(),
                &owners.profile_root,
                retention_secs,
                now_secs(),
                apply,
            )
            .await
            .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
            owner_observation = Some(doctor_owner_observation(
                "tracedecay.doctor.storage-orphan-store.v1",
                &(
                    apply,
                    orphan.plan.collect.len(),
                    orphan.outcome.collected.len(),
                    orphan.outcome.reclaimed_bytes,
                    orphan.outcome.errors.len(),
                    orphan.relinked_registry_rows,
                    orphan.retired_registry_rows,
                    unregistered.plan.collect.len(),
                    unregistered.outcome.collected.len(),
                    unregistered.outcome.reclaimed_bytes,
                    unregistered.outcome.errors.len(),
                ),
            )?);
        }
        (DoctorRemediationTargetV1::StorageBranchGc, false) => {
            let prepared = crate::branch::prepare_branch_admin_mutation(
                &owners.project_root,
                &owners.layout.data_root,
                crate::branch::BranchAdminAction::Gc,
                owners.config.sync.branch_gc_days,
                owners.config.sync.orphan_db_gc_days,
            )
            .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
            owner_observation = Some(doctor_owner_observation(
                "tracedecay.doctor.storage-branch-gc.preview.v1",
                prepared.report(),
            )?);
        }
        (DoctorRemediationTargetV1::StorageBranchGc, true) => {
            let report = owners
                .store_administration
                .execute_branch_admin_in_layout(
                    &owners.project_root,
                    &owners.layout.data_root,
                    crate::branch::BranchAdminAction::Gc,
                    owners.config.sync.branch_gc_days,
                    owners.config.sync.orphan_db_gc_days,
                )
                .await
                .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
            owner_observation = Some(doctor_owner_observation(
                "tracedecay.doctor.storage-branch-gc.apply.v1",
                &report,
            )?);
        }
        (DoctorRemediationTargetV1::StorageQuarantineAndCollectDebris, apply) => {
            let census = crate::retention::orphan_stores::build_store_census(
                owners.registry.as_ref(),
                &owners.profile_root,
            )
            .await
            .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
            if apply {
                let report = crate::retention::incident_debris::sweep_incident_debris(
                    &census,
                    &owners.profile_root,
                    retention_window_secs(
                        owners.config.sync.retention.incident_debris_retention_days,
                    ),
                    now_secs(),
                );
                if !report.errors.is_empty() {
                    return Err(DoctorRemediationDispatchErrorV1::OwnerUnavailable);
                }
                owner_observation = Some(doctor_owner_observation(
                    "tracedecay.doctor.storage-debris.apply.v1",
                    &(
                        report.quarantined,
                        report.collected,
                        report.retained,
                        report.reclaimed_bytes,
                    ),
                )?);
            } else {
                let mut reports = Vec::with_capacity(census.len());
                for entry in &census {
                    let report = crate::retention::incident_debris::scan_incident_debris(
                        entry,
                        &owners.profile_root,
                        now_secs(),
                    )
                    .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
                    reports.push((
                        entry.store_id.as_str(),
                        report.artifacts.len(),
                        report.listing_complete,
                    ));
                }
                owner_observation = Some(doctor_owner_observation(
                    "tracedecay.doctor.storage-debris.preview.v1",
                    &reports,
                )?);
            }
        }
        (DoctorRemediationTargetV1::ConfigurationPinAuthority, false) => {
            let pinned =
                crate::config::load_runtime_configuration_for_registered_database_read_only(
                    &owners.project_root,
                    &owners.layout,
                    Arc::clone(&owners.project_sessions),
                )
                .await
                .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
            owner_observation = Some(doctor_owner_observation(
                "tracedecay.doctor.configuration-pin.preview.v1",
                &pinned.revision_id,
            )?);
        }
        (DoctorRemediationTargetV1::ConfigurationPinAuthority, true) => {
            let pinned = crate::config::resolve_runtime_configuration_for_registered_database(
                &owners.project_root,
                &owners.layout,
                Arc::clone(&owners.project_sessions),
            )
            .await
            .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
            let revision_id = pinned.revision_id.clone();
            crate::config::install_pinned_runtime_configuration(pinned)
                .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
            owner_observation = Some(doctor_owner_observation(
                "tracedecay.doctor.configuration-pin.apply.v1",
                &revision_id,
            )?);
        }
        (DoctorRemediationTargetV1::RuntimeRecoverDaemon, true) => {
            owner_observation = if recovering {
                Some(doctor_owner_observation(
                    "tracedecay.doctor.runtime-restart.recovery-unknown.v1",
                    &operation_id,
                )?)
            } else {
                let executable = std::env::current_exe()
                    .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
                let child = std::process::Command::new(executable)
                    .args(["daemon", "restart"])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
                Some(doctor_owner_observation(
                    "tracedecay.doctor.runtime-restart.spawned.v1",
                    &child.id(),
                )?)
            };
            // Restart necessarily crosses this daemon process boundary. The
            // durable intent survives it; status remains effect-unknown until
            // an independently observed Doctor report proves recovery.
            effect_unknown = true;
        }
        (DoctorRemediationTargetV1::HostRepairIntegration { host, components }, apply) => {
            owner_observation = Some(execute_host_repair(
                *host,
                components,
                apply,
                &operation_id,
            )?);
        }
        (DoctorRemediationTargetV1::CodeIndexRemount, false) => {
            let mounted = owners
                .invocation
                .code_index_schedulers
                .is_worktree_mounted(&owners.project_root)
                .await;
            owner_observation = Some(doctor_owner_observation(
                "tracedecay.doctor.code-index.preview.v1",
                &mounted,
            )?);
        }
        (DoctorRemediationTargetV1::CodeIndexRemount, true) => {
            owners
                .invocation
                .mount_code_index(
                    owners.project_id.clone(),
                    &owners.project_root,
                    owners.code_index_store_root.clone(),
                    Some(&owners.semantic_runtime),
                    Some(Arc::clone(&owners.semantic_database)),
                    owners.semantic_lifecycle.clone(),
                    Some(owners.semantic_resources),
                )
                .await
                .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
            let mounted = owners
                .invocation
                .code_index_schedulers
                .is_worktree_mounted(&owners.project_root)
                .await;
            owner_observation = Some(doctor_owner_observation(
                "tracedecay.doctor.code-index.apply.v1",
                &mounted,
            )?);
        }
        _ => return Err(DoctorRemediationDispatchErrorV1::InvalidReference),
    }
    let ended_at = now_micros();
    if !apply {
        let execution = owner_execution.unwrap_or(
            completed_doctor_execution(started_at, ended_at)
                .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?,
        );
        let preview_id = match owner_preview {
            Some(preview_id) => preview_id,
            None => PreviewId::new(format!("preview.doctor-remediation.{operation_id}"))
                .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?,
        };
        return Ok(DoctorRemediationOperationV1 {
            operation_id,
            owning_operation: operation,
            phase: DoctorRemediationOperationPhaseV1::Previewed,
            preview_id: Some(preview_id),
            execution: Some(execution),
            effect_receipt: None,
            owner_effect_receipt: None,
            owner_result_digest: owner_observation,
            verification: DoctorRemediationVerificationV1::NotRequired,
        });
    }
    let idempotency_key =
        idempotency_key.ok_or(DoctorRemediationDispatchErrorV1::InvalidReference)?;
    let termination = if effect_unknown {
        EffectTermination::EffectUnknown
    } else {
        EffectTermination::Completed
    };
    let execution = match owner_execution {
        Some(execution) if execution.termination == termination.into() => execution,
        _ => terminal_doctor_execution(started_at, ended_at, termination)
            .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?,
    };
    let owner_result_digest =
        owner_observation.ok_or(DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    let effect_receipt = completed_doctor_effect(
        &operation,
        operation_id.request_id(),
        scope,
        idempotency_key,
        &target,
        owners,
        owner_result_digest.clone(),
        termination,
    )?;
    Ok(DoctorRemediationOperationV1 {
        operation_id,
        owning_operation: operation,
        phase: if effect_unknown {
            DoctorRemediationOperationPhaseV1::EffectUnknown
        } else {
            DoctorRemediationOperationPhaseV1::Completed
        },
        preview_id,
        execution: Some(execution),
        effect_receipt: Some(effect_receipt),
        owner_effect_receipt: owner_effect,
        owner_result_digest: Some(owner_result_digest),
        verification: DoctorRemediationVerificationV1::Pending,
    })
}

fn retention_window_secs(days: Option<u64>) -> i64 {
    days.and_then(|days| i64::try_from(days).ok())
        .and_then(|days| days.checked_mul(24 * 60 * 60))
        .unwrap_or(i64::MAX)
}

async fn run_doctor_session_retention(
    owners: &ProductionDoctorRemediationOwnersV1,
    database: &crate::global_db::RegisteredGlobalDb,
    apply: bool,
) -> Result<
    (
        Vec<crate::retention::RetentionTableReport>,
        Option<crate::sessions::lcm::retention::LcmRetentionReport>,
        Option<crate::global_db::observation::retention::ObservationRetentionReport>,
    ),
    crate::dashboard::DoctorRemediationDispatchErrorV1,
> {
    use crate::dashboard::DoctorRemediationDispatchErrorV1;
    let now = now_secs();
    let global = if apply {
        crate::retention::prune_global_retention(database, &owners.global_retention, now)
            .await
            .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?
    } else {
        crate::retention::global_retention_report(database, &owners.global_retention, now)
            .await
            .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?
    };
    let lcm = if owners.config.sync.retention.session_lcm.enabled {
        Some(
            database
                .run_session_lcm_retention(
                    "all",
                    None,
                    &owners.config.sync.retention.session_lcm,
                    if apply {
                        crate::sessions::lcm::RetentionMode::Apply
                    } else {
                        crate::sessions::lcm::RetentionMode::DryRun
                    },
                    now,
                )
                .await
                .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?,
        )
    } else {
        None
    };
    let observation = if owners.config.sync.retention.observation.enabled {
        Some(
            database
                .run_observation_retention(
                    None,
                    &owners.config.sync.retention.observation,
                    if apply {
                        crate::global_db::observation::retention::RetentionMode::Apply
                    } else {
                        crate::global_db::observation::retention::RetentionMode::DryRun
                    },
                    now,
                )
                .await
                .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?,
        )
    } else {
        None
    };
    Ok((global, lcm, observation))
}

fn completed_doctor_execution(
    started_at: tracedecay_domain::UtcMicros,
    ended_at: tracedecay_domain::UtcMicros,
) -> Result<OperationReceipt, ApplicationContractError> {
    terminal_doctor_execution(started_at, ended_at, EffectTermination::Completed)
}

fn terminal_doctor_execution(
    started_at: tracedecay_domain::UtcMicros,
    ended_at: tracedecay_domain::UtcMicros,
    termination: EffectTermination,
) -> Result<OperationReceipt, ApplicationContractError> {
    let receipt = OperationReceipt {
        started_at,
        ended_at,
        effective_deadline: Deadline::new(tracedecay_domain::UtcMicros(
            ended_at.0.saturating_add(DOCTOR_CONTEXT_HORIZON_MICROS),
        ))?,
        cancellation: None,
        budget: OperationBudgetUsage::default(),
        termination: termination.into(),
    };
    receipt.validate()?;
    Ok(receipt)
}

fn completed_doctor_effect(
    operation: &tracedecay_application::doctor::DoctorOwningOperationRefV1,
    request_id: &RequestId,
    scope: tracedecay_application::ResolvedScope,
    idempotency_key: IdempotencyKey,
    target: &crate::dashboard::DoctorRemediationTargetV1,
    owners: &ProductionDoctorRemediationOwnersV1,
    owner_result_digest: ManifestDigest,
    termination: EffectTermination,
) -> Result<EffectReceipt, crate::dashboard::DoctorRemediationDispatchErrorV1> {
    use crate::dashboard::DoctorRemediationDispatchErrorV1;
    let digest = target
        .digest()
        .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?;
    let operation_name = operation.as_str();
    let expected_state = doctor_orchestration_digest(
        "tracedecay.doctor-remediation.expected-owner-state.v1",
        &scope,
        &digest,
        &operation_name,
    )?;
    let policy_digest = doctor_orchestration_digest(
        "tracedecay.doctor-remediation.authorized-legal-action.v1",
        &scope,
        &digest,
        &(operation_name, &scope),
    )?;
    let configuration_digest = doctor_orchestration_digest(
        "tracedecay.doctor-remediation.mounted-configuration.v1",
        &scope,
        &digest,
        &(&owners.config, &owners.global_retention),
    )?;
    let catalog_digest = doctor_orchestration_digest(
        "tracedecay.doctor-remediation.owner-catalog-entry.v1",
        &scope,
        &digest,
        &(operation_name, target),
    )?;
    let privacy_digest = doctor_orchestration_digest(
        "tracedecay.doctor-remediation.local-scoped-owner-effect.v1",
        &scope,
        &digest,
        &scope,
    )?;
    let receipt = EffectReceipt {
        operation: tracedecay_tool_catalog::UseCaseId::new(operation.as_str().to_owned())
            .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?,
        request_id: request_id.clone(),
        actor: tracedecay_domain::ActorId::new("actor.tracedecay-daemon")
            .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?,
        scope: scope.clone(),
        effect_class: tracedecay_tool_catalog::EffectClass::Administrative,
        idempotency_key,
        input_digest: digest.clone(),
        expected_state,
        policy_digest,
        configuration_digest,
        catalog_digest,
        privacy_digest,
        outcome: termination,
        committed_state: (termination == EffectTermination::Completed)
            .then_some(owner_result_digest),
        external_proof: None,
    };
    receipt
        .validate()
        .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?;
    Ok(receipt)
}

fn doctor_owner_observation(
    domain: &'static str,
    evidence: &impl serde::Serialize,
) -> Result<ManifestDigest, crate::dashboard::DoctorRemediationDispatchErrorV1> {
    tracedecay_domain::canonical_sha256(&(domain, evidence))
        .map_err(|_| crate::dashboard::DoctorRemediationDispatchErrorV1::InvalidReference)
}

fn doctor_orchestration_digest(
    domain: &'static str,
    scope: &tracedecay_application::ResolvedScope,
    target_digest: &ManifestDigest,
    evidence: &impl serde::Serialize,
) -> Result<ManifestDigest, crate::dashboard::DoctorRemediationDispatchErrorV1> {
    tracedecay_domain::canonical_sha256(&(domain, scope, target_digest, evidence))
        .map_err(|_| crate::dashboard::DoctorRemediationDispatchErrorV1::InvalidReference)
}

fn execute_host_repair(
    host: crate::agents::host_bundle_v2::HostKindV1,
    components: &[crate::agents::host_bundle_v2::HostBundleComponentV1],
    apply: bool,
    operation_id: &crate::application::operation_stream::OperationId,
) -> Result<ManifestDigest, crate::dashboard::DoctorRemediationDispatchErrorV1> {
    use crate::agents::host_bundle_v2::{
        HostBundleLifecycleOpV1, HostBundleWriterV1, HostComponentSetExecutionRequestV1,
        HostComponentSetLifecycleRequestV1, HostComponentSetTransactionV1,
    };
    use crate::dashboard::DoctorRemediationDispatchErrorV1;
    let home =
        crate::agents::home_dir().ok_or(DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    let lifecycle_root = crate::agents::host_bundle_v2::resolved_host_bundle_lifecycle_root()
        .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    let now_unix = u64::try_from(now_secs())
        .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    let set = crate::agents::host_bundle_registry::verified_embedded_host_component_set(
        host, components, now_unix,
    )
    .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    let digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.doctor-host-repair-operation.v1",
        operation_id,
    ))
    .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?;
    let bytes = hex::decode(digest.as_str())
        .map_err(|_| DoctorRemediationDispatchErrorV1::InvalidReference)?;
    let mut host_operation_id = [0_u8; 16];
    host_operation_id.copy_from_slice(
        bytes
            .get(..16)
            .ok_or(DoctorRemediationDispatchErrorV1::InvalidReference)?,
    );
    let request = HostComponentSetExecutionRequestV1 {
        lifecycle: HostComponentSetLifecycleRequestV1 {
            operation: HostBundleLifecycleOpV1::Repair,
            expected_host: host,
            expected_components: components.to_vec(),
            explicit_confirmation: apply,
            hermes_profile_bindings: u8::from(
                host == crate::agents::host_bundle_v2::HostKindV1::Hermes,
            ),
        },
        operation_id: host_operation_id,
    };
    let mut writer = HostBundleWriterV1::open_with_lifecycle_root(&home, &lifecycle_root)
        .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    let mut transaction = HostComponentSetTransactionV1::new(&mut writer);
    let mut registration =
        crate::agents::host_component_registration::HostComponentRegistrationDelegate::new(
            crate::agents::integration_id_for_host(host),
            &home,
            &lifecycle_root,
            HostBundleLifecycleOpV1::Repair,
        )
        .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    let preview = transaction
        .preview(&set.component_set, &request, &set, &mut registration)
        .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
    if apply {
        let receipt = transaction
            .execute_confirmed(
                &set.component_set,
                &request,
                &preview,
                &set,
                &mut registration,
            )
            .map_err(|_| DoctorRemediationDispatchErrorV1::OwnerUnavailable)?;
        return doctor_owner_observation("tracedecay.doctor.host-repair.apply.v1", &receipt);
    }
    doctor_owner_observation(
        "tracedecay.doctor.host-repair.preview.v1",
        &(
            preview.operation_id,
            preview.plan_digest,
            preview.base_registration_revision,
            preview.current_registration_revision,
            preview.artifact_state_revision,
            preview.component_plans.len(),
            preview.confirmation_required,
        ),
    )
}

fn merge_storage_reads(
    first: DoctorStorageFamilyReadV1,
    second: DoctorStorageFamilyReadV1,
) -> DoctorStorageFamilyReadV1 {
    match (first, second) {
        (
            DoctorStorageFamilyReadV1::Observed {
                findings: mut first,
            },
            DoctorStorageFamilyReadV1::Observed { findings: second },
        ) => {
            first.extend(second);
            storage_family_read(first)
        }
        (DoctorStorageFamilyReadV1::Observed { findings }, DoctorStorageFamilyReadV1::Absent)
        | (DoctorStorageFamilyReadV1::Absent, DoctorStorageFamilyReadV1::Observed { findings }) => {
            storage_family_read(findings)
        }
        (DoctorStorageFamilyReadV1::Absent, DoctorStorageFamilyReadV1::Absent) => {
            DoctorStorageFamilyReadV1::Absent
        }
        (DoctorStorageFamilyReadV1::Denied, _) | (_, DoctorStorageFamilyReadV1::Denied) => {
            DoctorStorageFamilyReadV1::Denied
        }
        (DoctorStorageFamilyReadV1::Unsupported, _)
        | (_, DoctorStorageFamilyReadV1::Unsupported) => DoctorStorageFamilyReadV1::Unsupported,
        _ => DoctorStorageFamilyReadV1::Unknown,
    }
}

fn doctor_report_request_context(
    scope: tracedecay_application::ResolvedScope,
) -> Result<RequestContext, ApplicationContractError> {
    let observed_at = now_micros();
    let expires_at =
        tracedecay_domain::UtcMicros(observed_at.0.saturating_add(DOCTOR_CONTEXT_HORIZON_MICROS));
    let request_id = mint_global_request_id(GlobalRequestSurface::DaemonDoctor).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "doctor report request identity",
        }
    })?;
    let suffix = request_id.as_str().to_owned();
    let actor = tracedecay_domain::ActorId::new("actor.tracedecay-daemon")?;
    let capability =
        tracedecay_tool_catalog::CapabilityId::new(DOCTOR_REPORT_CAPABILITY.to_owned())?;
    let use_case = tracedecay_tool_catalog::UseCaseId::new(DOCTOR_REPORT_USE_CASE.to_owned())?;
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.daemon.doctor.{suffix}"))?,
        1,
        tracedecay_domain::canonical_sha256(&(
            "tracedecay.daemon.doctor-report-grant.v1",
            &scope,
            &capability,
            &use_case,
            expires_at,
        ))?,
        actor.clone(),
        observed_at,
        expires_at,
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Metadata,
    )?;
    RequestContext::new(
        actor,
        scope,
        grant,
        request_id,
        Deadline::new(expires_at)?,
        CancellationContext::active(format!("cancel.daemon.doctor.{suffix}"))?,
    )
}

fn now_secs() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs()),
    )
    .unwrap_or(i64::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;

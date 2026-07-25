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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_application::doctor::{
    AdvisoryFeedbackDoctorPort, AdvisoryFeedbackFindingReadV1, AdvisoryFeedbackReadV1,
    AdvisoryFeedbackSummaryReadV1, CodeIndexMountDoctorPort, CodeIndexMountReadV1,
    CodeIndexMountStateV1, ConfigurationAuthorityDoctorPort, ConfigurationAuthorityReadV1,
    ConfigurationDriftV1, DoctorCoverageCompletenessV1, DoctorReportComposerV1, DoctorReportV1,
    DoctorSourceFuture, DoctorStorageFamilyReadV1, DoctorStorageFindingV1, HostConformanceV1,
    HostIntegrationDoctorPort, HostIntegrationReadV1, LanguageServerDoctorPort,
    LanguageServerReadV1, LanguageServerStateV1, ObservabilityDoctorPort, ObservabilityReadV1,
    ObservabilityStateV1, RuntimeHealthDoctorPort, RuntimeHealthReadV1, RuntimeLivenessV1,
    StorageDoctorPort,
};
use tracedecay_application::{
    ApplicationContractError, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, RequestContext, RequestId,
};

use crate::config::PinnedRuntimeConfiguration;

const DOCTOR_REPORT_CAPABILITY: &str = "capability.application.doctor.report";
const DOCTOR_REPORT_USE_CASE: &str = "use-case.application.doctor.report";
const DOCTOR_CONTEXT_HORIZON_MICROS: i64 = 30_000_000;
static DOCTOR_REQUEST_NONCE: AtomicU64 = AtomicU64::new(0);

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

    /// Build the adapter from a real host conformance summary.
    #[must_use]
    pub fn from_summary(summary: &HostConformanceSummaryV1) -> Self {
        Self::from_read(host_conformance_read(summary))
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
    AdvisoryFeedbackReadV1::Observed { summary, findings }
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
/// ladder yields a complete generation reports `Mounted` (the ladder already
/// reconciled it, so it is current); a mounted worktree with no published
/// generation is still `Indexing`. No signal is fabricated — the registry is the
/// authority for what is mounted.
pub(in crate::daemon) async fn code_index_read_from_registry(
    registry: &crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: &Path,
) -> CodeIndexMountReadV1 {
    if !registry.is_worktree_mounted(project_root).await {
        return code_index_read(CodeIndexMountSignalV1::Unmounted);
    }
    let signal = if registry.latest_complete_fresh(project_root).await.is_some() {
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
    states: impl IntoIterator<Item = crate::diagnostics::lsp::broker::EngineState>,
) -> LanguageServerReadV1 {
    use crate::diagnostics::lsp::broker::EngineState;

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
    broker: &tokio::sync::Mutex<crate::diagnostics::lsp::broker::DiagnosticBroker>,
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
        Ok(model) if model.total_count == 0 => ObservabilityReadV1::Absent,
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
pub async fn collect_over_budget_store_findings(
    graph: &crate::db::Database,
    registry: &crate::global_db::RegisteredGlobalDb,
    profile_sessions: &crate::global_db::RegisteredGlobalDb,
    project_sessions: &crate::global_db::RegisteredGlobalDb,
    retention: &crate::config::RetentionConfig,
    observed_at_secs: i64,
) -> DoctorStorageFamilyReadV1 {
    use std::collections::BTreeMap;
    use tracedecay_application::storage::{
        StorageTelemetryReadV1, StoreKeyV1, StoreSizeSampleV1, over_budget_finding,
    };

    fn key(path: &Path) -> Option<StoreKeyV1> {
        StoreKeyV1::new(path.file_name()?.to_str()?.to_owned()).ok()
    }

    fn read(
        store: StoreKeyV1,
        counts: crate::errors::Result<(u64, u64, u64)>,
        observed_at_secs: i64,
    ) -> StorageTelemetryReadV1 {
        let Ok((page_size, page_count, freelist_pages)) = counts else {
            return StorageTelemetryReadV1::Unknown { store };
        };
        let Ok(page_size_bytes) = u32::try_from(page_size) else {
            return StorageTelemetryReadV1::Unknown { store };
        };
        StorageTelemetryReadV1::Observed {
            sample: StoreSizeSampleV1 {
                store,
                page_size_bytes,
                page_count,
                freelist_pages,
                observed_at: tracedecay_domain::UtcMicros(
                    observed_at_secs.saturating_mul(1_000_000),
                ),
            },
        }
    }

    let mut reads = BTreeMap::new();
    if let Some(store) = key(graph.database_path()) {
        reads.insert(
            store.as_str().to_owned(),
            read(store, graph.storage_page_counts().await, observed_at_secs),
        );
    }
    for database in [registry, profile_sessions, project_sessions] {
        if let Some(store) = key(database.db_path()) {
            reads
                .entry(store.as_str().to_owned())
                .or_insert_with(|| read(store, database.storage_page_counts(), observed_at_secs));
        }
    }

    let mut findings = Vec::new();
    for configured_store in retention.store_soft_budgets_bytes.keys() {
        let Ok(Some(budget)) = retention.store_soft_budget(configured_store) else {
            return DoctorStorageFamilyReadV1::Unknown;
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
            return DoctorStorageFamilyReadV1::Unknown;
        };
        findings.push(finding);
    }
    storage_family_read(findings)
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
        .with_host(&host)
        .with_advisory_feedback(&advisory_feedback)
        .with_language_server(&language_server)
        .with_code_index(&code_index)
        .with_observability(&observability)
        .with_storage(&storage);

    composer.compose(context).await
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
    retention: crate::config::RetentionConfig,
    schedulers: crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    diagnostic_broker: Arc<tokio::sync::Mutex<crate::diagnostics::lsp::broker::DiagnosticBroker>>,
    feedback_runtimes: crate::daemon::service::invocation::DaemonFeedbackRuntimeRegistrar,
) -> crate::dashboard::DoctorReportReader {
    Arc::new(move || {
        let project_root = project_root.clone();
        let project_id = project_id.clone();
        let layout = layout.clone();
        let graph = graph.clone();
        let registry = Arc::clone(&registry);
        let profile_sessions = Arc::clone(&profile_sessions);
        let project_sessions = Arc::clone(&project_sessions);
        let profile_root = profile_root.clone();
        let retention = retention.clone();
        let schedulers = schedulers.clone();
        let diagnostic_broker = Arc::clone(&diagnostic_broker);
        let feedback_runtimes = feedback_runtimes.clone();
        Box::pin(async move {
            let scope =
                super::project_open_owners::resolved_scope_for_project(&project_root, &project_id)
                    .map_err(|_| ApplicationContractError::Inconsistent {
                        field: "daemon Doctor project scope",
                    })?;
            let context = doctor_report_request_context(scope)?;
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
            let temporal = crate::global_db::session_temporal::session_temporal_doctor_health_at(
                profile_sessions.db_path(),
            )
            .await;
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
            let orphan = collect_orphan_store_findings(
                registry.as_ref(),
                &profile_root,
                retention_secs,
                now,
            )
            .await;
            let unregistered = collect_unregistered_store_findings(
                registry.as_ref(),
                &profile_root,
                retention_secs,
                now,
            )
            .await;
            let over_budget = collect_over_budget_store_findings(
                &graph,
                registry.as_ref(),
                profile_sessions.as_ref(),
                project_sessions.as_ref(),
                &retention,
                now,
            )
            .await;
            let stale_branches = collect_stale_branch_store_findings(&project_root, &layout);
            let incident_debris =
                collect_incident_debris_findings(registry.as_ref(), &profile_root, now).await;
            let profile_retention_backlog =
                collect_retention_backlog_findings(profile_sessions.as_ref(), &retention, now)
                    .await;
            let project_retention_backlog =
                collect_retention_backlog_findings(project_sessions.as_ref(), &retention, now)
                    .await;
            let storage = [
                orphan,
                unregistered,
                over_budget,
                stale_branches,
                incident_debris,
                profile_retention_backlog,
                project_retention_backlog,
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
                .latest_complete_fresh(&project_root)
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
            let inputs = DoctorKernelInputsV1 {
                configuration: configuration_read_from_pin::<crate::errors::TraceDecayError>(
                    &pinned,
                ),
                runtime: runtime_health_read(&DaemonRuntimeHealthSignalV1 {
                    serving: true,
                    startup_converged: graph_authority_current && registered_authority_current,
                    quick_check_ok,
                    authority_audit_ok: Some(
                        graph_authority_current && registered_authority_current,
                    ),
                    temporal_ok,
                }),
                // Host conformance has no single project-scoped runtime owner;
                // retain honest unknown until the profile host registry is
                // injected rather than probing mutable paths here.
                host: HostIntegrationReadV1::Unknown,
                advisory_feedback,
                language_server,
                code_index: code_index_read_from_registry(&schedulers, &project_root).await,
                observability,
                storage,
            };
            let report = compose_doctor_report(&context, &inputs).await?;
            Ok(crate::dashboard::AdmittedDoctorReportV1::new(report))
        })
    })
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
    let nonce = DOCTOR_REQUEST_NONCE.fetch_add(1, Ordering::Relaxed);
    let suffix = format!("{}.{}.{}", std::process::id(), observed_at.0.max(0), nonce);
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
        RequestId::new(format!("request.daemon.doctor.{suffix}"))?,
        Deadline::new(expires_at)?,
        CancellationContext::active(format!("cancel.daemon.doctor.{suffix}"))?,
    )
}

fn now_micros() -> tracedecay_domain::UtcMicros {
    tracedecay_domain::UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_micros()),
        )
        .unwrap_or(i64::MAX),
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

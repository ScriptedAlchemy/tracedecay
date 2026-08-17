//! Daemon-side adapters for the read-only Doctor kernel source ports.
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
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::PinnedRuntimeConfiguration;
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_application::doctor::{
    AdvisoryFeedbackDoctorPort, AdvisoryFeedbackFindingReadV1, AdvisoryFeedbackReadV1,
    AdvisoryFeedbackSummaryReadV1, CodeIndexMountDoctorPort, CodeIndexMountReadV1,
    CodeIndexMountStateV1, ConfigurationAuthorityDoctorPort, ConfigurationAuthorityReadV1,
    ConfigurationDriftV1, DoctorCoverageCompletenessV1, DoctorReportComposerV1, DoctorReportV1,
    DoctorSourceFuture, DoctorStorageFamilyReadV1, DoctorStorageFindingV1,
    DoctorStorageIncompleteReasonV1, HostConformanceV1, HostIntegrationDoctorPort,
    HostIntegrationReadV1, LanguageServerDoctorPort, LanguageServerReadV1, LanguageServerStateV1,
    ObservabilityDoctorPort, ObservabilityReadV1, ObservabilityStateV1, OperationalAuditDoctorPort,
    OperationalAuditReadV1, ProfileAuthorityReadV1, RemoteOperationalReadV1,
    RuntimeHealthDoctorPort, RuntimeHealthReadV1, RuntimeLivenessV1, StorageDoctorPort,
};
use tracedecay_application::{
    ApplicationContractError, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, RequestContext, now_micros,
};

use super::maintenance::GuardedStoreTelemetryPort;

const DOCTOR_REPORT_CAPABILITY: &str = "capability.application.doctor.report";
const DOCTOR_REPORT_USE_CASE: &str = "use-case.application.doctor.report";
const DOCTOR_CONTEXT_HORIZON_MICROS: i64 = 30_000_000;

// === Configuration authority (Configuration family) ==========================

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
        Ok(_) => ConfigurationAuthorityReadV1::Resolved {
            drift: ConfigurationDriftV1::InSync,
            coverage: DoctorCoverageCompletenessV1::Complete,
        },
        Err(_) => ConfigurationAuthorityReadV1::Absent,
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

/// Run the exhaustive observation-authority invariant pass over an already
/// acquired read snapshot of the registered profile authority.
///
/// This is the same pass the `tracedecay_runtime` producers run
/// ([`crate::global_db::schema_stages::validate_observation_authority_connection`]):
/// read-only, so Doctor observes the invariant without owning any repair of it.
/// `true` means the audit ran and every invariant held; `false` means it ran and
/// an invariant failed. "Could not run" is not representable here — the caller
/// owns that distinction.
async fn observation_authority_audit_passed(
    snapshot: &impl crate::db::engine::QueryExecutor,
) -> bool {
    crate::global_db::schema_stages::validate_observation_authority_connection(snapshot)
        .await
        .is_ok()
}

/// Observe the storage authority audit signal the daemon-side Doctor reader
/// reports as [`DaemonRuntimeHealthSignalV1::authority_audit_ok`].
///
/// Tri-state, matching the vocabulary the `tracedecay_runtime` producers already
/// publish: `Some(true)` only when the audit ran and passed, `Some(false)` when
/// it ran and an invariant failed, and `None` when it could not run at all
/// because the registered authority would not yield a read snapshot. A not-run
/// audit weakens runtime coverage to partial rather than claiming health.
async fn observation_authority_audit_ok(
    registry: &crate::global_db::RegisteredGlobalDb,
) -> Option<bool> {
    match registry.read_snapshot().await {
        Ok(snapshot) => Some(observation_authority_audit_passed(&snapshot).await),
        Err(_) => None,
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
        return CodeIndexMountReadV1::Observed {
            state: CodeIndexMountStateV1::Unmounted,
            coverage: DoctorCoverageCompletenessV1::Complete,
        };
    }
    let state = if registry.latest_complete_ready(project_root).await.is_some() {
        CodeIndexMountStateV1::Mounted
    } else {
        CodeIndexMountStateV1::Indexing
    };
    CodeIndexMountReadV1::Observed {
        state,
        coverage: DoctorCoverageCompletenessV1::Complete,
    }
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
        tracedecay_usecases::feedback::observations::FeedbackObservationReadModelV1,
        tracedecay_usecases::feedback::concrete::FeedbackRuntimeError,
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
            use tracedecay_usecases::feedback::observations::FeedbackCoverageV1;
            let (state, coverage) = match model.coverage {
                FeedbackCoverageV1::Known => (
                    ObservabilityStateV1::Current,
                    DoctorCoverageCompletenessV1::Complete,
                ),
                FeedbackCoverageV1::Stale => (
                    ObservabilityStateV1::Stale,
                    DoctorCoverageCompletenessV1::Partial,
                ),
                FeedbackCoverageV1::Partial
                | FeedbackCoverageV1::Sampled
                | FeedbackCoverageV1::Capped => (
                    ObservabilityStateV1::Current,
                    DoctorCoverageCompletenessV1::Partial,
                ),
                FeedbackCoverageV1::Unknown => (
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

fn orphan_store_findings_from_census(
    census: &[crate::retention::orphan_stores::StoreCensusEntry],
    retention_secs: i64,
    now: i64,
) -> DoctorStorageFamilyReadV1 {
    let classified = crate::retention::orphan_stores::classify_stores(census, now);
    let plan = crate::retention::orphan_stores::plan_collection(classified, retention_secs);
    storage_family_read(
        plan.collect
            .iter()
            .chain(plan.retained_immature.iter())
            .chain(plan.relink.iter())
            .filter_map(crate::doctor::registry_drift::orphan_store_doctor_finding)
            .collect(),
    )
}

/// Collect the daemon's read-only unregistered-store-directory Doctor
/// findings for a profile (plan 38 §2's disjoint on-disk-only audit class —
/// a store directory with no `code_projects` row at all, invisible to the
/// registry-driven census performs). Runs the bottom-up sweep in
/// classification-only mode (no collection).
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
        GuardedStoreTelemetryPort,
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
            port.preview_table_growth(context, store).await
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

fn incident_debris_findings_from_census(
    census: &[crate::retention::orphan_stores::StoreCensusEntry],
    profile_root: &Path,
    observed_at_secs: i64,
) -> DoctorStorageFamilyReadV1 {
    let mut findings = Vec::new();
    for entry in census {
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
    let Ok(records) = tracedecay_sessions::runtime::lcm::retention::read_session_retention_backlog(
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
pub(super) async fn collect_code_generation_retention_findings(
    schedulers: &super::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    maintenance_observations: &super::maintenance::StoreTelemetrySamplingRegistry,
    configuration: Option<
        &tracedecay_usecases::semantic_runtime::ProductionSemanticRetrievalConfigurationStoreV1,
    >,
    code_index_store_root: &Path,
    project_root: &Path,
) -> DoctorStorageFamilyReadV1 {
    use crate::retention::code_index_generations::{
        DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS, DEFAULT_SUPERSEDED_GENERATION_FLOOR,
        GenerationDigestVerificationV1, ScopeRootRetentionPlanV1,
        plan_code_generation_retention_with_verification, plan_scope_root_retention,
    };
    use tracedecay_application::storage::{
        CodeGenerationRetentionRecordV1, SemanticVectorRetentionRecordV1, StorageByteSizeV1,
        StoreKeyV1, code_generation_retention_finding, semantic_vector_retention_finding,
    };

    if !code_index_store_root
        .join("active-code-generation-v1.json")
        .is_file()
    {
        return DoctorStorageFamilyReadV1::Absent;
    }
    let Some(configuration) = configuration else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let super::maintenance::SemanticVectorRetentionReadV1::Observed {
        receipt: semantic_census,
    } = maintenance_observations.semantic_vector_retention_read(project_root)
    else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    // Published vectors live in the mounted code graph; without it the
    // protection set cannot be proven and the census reads as Unknown rather
    // than "nothing is pinned".
    let vector_readable_sources =
        match super::code_index_scheduler::semantic_vector_graph::project_vector_readable_sources(
            schedulers,
            project_root,
            configuration,
            semantic_census.revision,
        )
        .await
        {
            super::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Ready {
                sources,
                configured_root_receipt,
                ..
            } => (sources, configured_root_receipt.root_count()),
            // Each of these is a NAMED vector-authority degradation carrying the
            // reason the authority reported. Collapsing them into `Unknown`
            // would claim the state could not be determined when it was in fact
            // determined and explained, so each keeps its name and its reason.
            super::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Unavailable(
                detail,
            ) => return DoctorStorageFamilyReadV1::Unavailable { detail },
            super::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::ResetRequired(
                detail,
            ) => return DoctorStorageFamilyReadV1::ResetRequired { detail },
            super::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Corrupt(
                detail,
            ) => return DoctorStorageFamilyReadV1::Corrupt { detail },
            super::code_index_scheduler::semantic_vector_graph::ProjectVectorReadableSources::Denied(
                _,
            ) => return DoctorStorageFamilyReadV1::Denied,
        };
    let (vector_readable_sources, retained_vector_root_count) = vector_readable_sources;
    let semantic_backlog =
        super::maintenance::SemanticVectorRetentionBacklogV1::from_receipt(&semantic_census);
    if semantic_backlog.published < retained_vector_root_count {
        return DoctorStorageFamilyReadV1::Unknown;
    }
    let Ok(semantic_store) = StoreKeyV1::new("semantic-vector-graph") else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let semantic_record = SemanticVectorRetentionRecordV1 {
        store: semantic_store,
        pending_generation_count: semantic_backlog.pending,
        ready_generation_count: semantic_backlog.ready,
        observed_non_configured_published_generation_count: semantic_backlog
            .published
            .saturating_sub(retained_vector_root_count),
        cancelled_generation_count: semantic_backlog.cancelled,
    };
    let semantic_completeness = DoctorCoverageCompletenessV1::Complete;
    let Ok(semantic_finding) =
        semantic_vector_retention_finding(&semantic_record, semantic_completeness)
    else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let vector_liveness_incomplete = semantic_record.has_backlog()
        || semantic_record.has_in_flight_generations()
        || semantic_record.observed_non_configured_published_generation_count > 0;
    let semantic_only_unknown = || DoctorStorageFamilyReadV1::ObservedIncomplete {
        findings: vec![semantic_finding.clone()],
        reason: DoctorStorageIncompleteReasonV1::Unknown,
    };
    if !permits_synchronous_generation_census(&code_index_store_root.join("code-generations-v1")) {
        return semantic_only_unknown();
    }
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
        return semantic_only_unknown();
    };
    let (plan, scopes) = census;
    let Ok(plan) = plan else {
        return semantic_only_unknown();
    };
    let Ok(store) = StoreKeyV1::new("code-index-v1") else {
        return semantic_only_unknown();
    };
    let completeness = if scopes.is_some() && !vector_liveness_incomplete {
        DoctorCoverageCompletenessV1::Complete
    } else {
        DoctorCoverageCompletenessV1::Partial
    };
    let record = CodeGenerationRetentionRecordV1 {
        store,
        superseded_generation_count: plan.superseded_generations.len() as u64,
        superseded_generation_bytes: StorageByteSizeV1(plan.superseded_generation_bytes()),
        collectable_generation_count: if vector_liveness_incomplete {
            0
        } else {
            plan.collectable_generations.len() as u64
        },
        collectable_generation_bytes: if vector_liveness_incomplete {
            StorageByteSizeV1::ZERO
        } else {
            StorageByteSizeV1(plan.collectable_generation_bytes())
        },
        stranded_scope_count: if vector_liveness_incomplete {
            0
        } else {
            scopes
                .as_ref()
                .map_or(0, ScopeRootRetentionPlanV1::stranded_scope_count)
        },
        stranded_scope_bytes: if vector_liveness_incomplete {
            StorageByteSizeV1::ZERO
        } else {
            StorageByteSizeV1(
                scopes
                    .as_ref()
                    .map_or(0, ScopeRootRetentionPlanV1::stranded_scope_bytes),
            )
        },
    };
    let Ok(finding) = code_generation_retention_finding(&record, completeness) else {
        return semantic_only_unknown();
    };
    let findings = vec![semantic_finding, finding];
    if vector_liveness_incomplete {
        DoctorStorageFamilyReadV1::ObservedIncomplete {
            findings,
            reason: DoctorStorageIncompleteReasonV1::Unknown,
        }
    } else {
        storage_family_read(findings)
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
    registry: crate::global_db::RegisteredGlobalDbLeaseV1,
    profile_sessions: crate::global_db::RegisteredGlobalDbLeaseV1,
    project_sessions: crate::global_db::RegisteredGlobalDbLeaseV1,
    profile_root: PathBuf,
    host_home: Option<PathBuf>,
    remote_operational: RemoteOperationalReadV1,
    retention: crate::config::RetentionConfig,
    schedulers: crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    diagnostic_broker: Arc<tokio::sync::Mutex<tracedecay_lsp::analyzer::broker::DiagnosticBroker>>,
    feedback_runtimes: crate::daemon::service::invocation::DaemonFeedbackRuntimeRegistrar,
    store_telemetry_sampling: super::maintenance::StoreTelemetrySamplingRegistry,
    configuration_runtime: Arc<tracedecay_usecases::configuration::ProjectConfigurationRuntime>,
) -> crate::dashboard::DoctorReportReader {
    Arc::new(move || {
        let project_root = project_root.clone();
        let project_id = project_id.clone();
        let layout = layout.clone();
        let graph = graph.clone();
        let registry = registry.clone();
        let profile_sessions = profile_sessions.clone();
        let project_sessions = project_sessions.clone();
        let profile_root = profile_root.clone();
        let host_home = host_home.clone();
        let remote_operational = remote_operational.clone();
        let retention = retention.clone();
        let schedulers = schedulers.clone();
        let diagnostic_broker = Arc::clone(&diagnostic_broker);
        let feedback_runtimes = feedback_runtimes.clone();
        let store_telemetry_sampling = store_telemetry_sampling.clone();
        let configuration_runtime = Arc::clone(&configuration_runtime);
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
                && let Some(port) =
                    store_telemetry_sampling.registered_port(graph.database_path(), context.scope())
            {
                telemetry_ports.push(port);
            }
            for database in [
                registry.as_ref(),
                profile_sessions.as_ref(),
                project_sessions.as_ref(),
            ] {
                if telemetry_paths.insert(database.db_path().to_path_buf())
                    && let Some(port) = store_telemetry_sampling
                        .registered_port(database.db_path(), context.scope())
                {
                    telemetry_ports.push(port);
                }
            }
            let pinned = crate::config::runtime_configuration_for_layout(&project_root, &layout);
            let graph_authority_current = graph.write_authority().is_ok_and(|authority| {
                authority
                    .require_active_write_scope("read dashboard Doctor graph authority")
                    .is_ok()
            });
            let registered_authority_current = registry.writer_connection().is_ok()
                && profile_sessions.writer_connection().is_ok();
            let retention_secs = retention
                .orphan_store_gc_days
                .and_then(|days| i64::try_from(days).ok())
                .and_then(|days| days.checked_mul(24 * 60 * 60))
                .unwrap_or(i64::MAX);
            let now = now_secs();
            let profile_scan_root = profile_root.join("projects");
            let profile_storage_reads = async {
                let permitted = tokio::task::spawn_blocking(move || {
                    permits_synchronous_exhaustive_scan(&profile_scan_root)
                })
                .await
                .is_ok_and(|permitted| permitted);
                if !permitted {
                    return (None, DoctorStorageFamilyReadV1::Unknown);
                }
                let (registered_census, unregistered) = tokio::join!(
                    crate::retention::orphan_stores::build_store_census(
                        registry.as_ref(),
                        &profile_root,
                    ),
                    collect_unregistered_store_findings(
                        registry.as_ref(),
                        &profile_root,
                        retention_secs,
                        now,
                    ),
                );
                (registered_census.ok(), unregistered)
            };
            let code_index_store_root = super::code_index_scheduler::scoped_code_index_store_root(
                &layout.data_root.join("code-index-v1"),
                &project_root,
            );
            let advisory_feedback_read = async {
                let current_generation = schedulers
                    .latest_complete_ready(&project_root)
                    .await
                    .map(|latest| latest.generation().manifest().generation_id.clone());
                match feedback_runtimes.doctor_read_store(&project_root).await {
                    Some(store) => match store.doctor_latest_publication(&context).await {
                        Ok(publication) => advisory_feedback_read_from_publication(
                            publication.as_ref(),
                            current_generation.as_ref(),
                        ),
                        Err(_) => AdvisoryFeedbackReadV1::Unknown,
                    },
                    None => AdvisoryFeedbackReadV1::Absent,
                }
            };
            let host_project_root = project_root.clone();
            let host_components_root = profile_root.join("host-components");
            let host_scan = tokio::task::spawn_blocking(move || {
                host_home
                    .as_ref()
                    .map_or(HostIntegrationReadV1::Unsupported, |home| {
                        let context = crate::agents::HealthcheckContext {
                            home: home.clone(),
                            project_path: host_project_root,
                        };
                        crate::agents::inspect_receipt_backed_host_components(
                            &context,
                            &host_components_root,
                        )
                        .as_ref()
                        .map_or(
                            HostIntegrationReadV1::Unknown,
                            host_integration_read_from_report,
                        )
                    })
            });
            let semantic_configuration_inventory =
                configuration_runtime.semantic_configuration_inventory_authority();
            let (
                quick_check,
                authority_audit_ok,
                temporal,
                (registered_census, unregistered),
                store_telemetry,
                profile_retention_backlog,
                project_retention_backlog,
                code_generation_retention,
                language_server,
                observability_read,
                advisory_feedback,
                host_read,
                code_index,
            ) = tokio::join!(
                graph.quick_check_report(),
                observation_authority_audit_ok(registry.as_ref()),
                project_sessions.session_temporal_doctor_health(),
                profile_storage_reads,
                collect_over_budget_store_findings(&context, &telemetry_ports, &retention),
                collect_retention_backlog_findings(profile_sessions.as_ref(), &retention, now),
                collect_retention_backlog_findings(project_sessions.as_ref(), &retention, now),
                collect_code_generation_retention_findings(
                    &schedulers,
                    &store_telemetry_sampling,
                    semantic_configuration_inventory.as_ref(),
                    &code_index_store_root,
                    &project_root,
                ),
                language_server_read_from_broker(&diagnostic_broker),
                tracedecay_usecases::feedback::concrete::feedback_observation_read_model(&graph,),
                advisory_feedback_read,
                host_scan,
                code_index_read_from_registry(&schedulers, &project_root),
            );
            let quick_check_ok = quick_check.ok().map(|problem| problem.is_none());
            let temporal_ok = match temporal.status() {
                crate::global_db::session_temporal::SessionTemporalHealthStatus::Complete => {
                    Some(temporal.findings().is_empty())
                }
                crate::global_db::session_temporal::SessionTemporalHealthStatus::Partial
                | crate::global_db::session_temporal::SessionTemporalHealthStatus::Unavailable
                | crate::global_db::session_temporal::SessionTemporalHealthStatus::Locked => None,
            };
            let (orphan, incident_debris) = registered_census.as_deref().map_or(
                (
                    DoctorStorageFamilyReadV1::Unknown,
                    DoctorStorageFamilyReadV1::Unknown,
                ),
                |census| {
                    (
                        orphan_store_findings_from_census(census, retention_secs, now),
                        incident_debris_findings_from_census(census, &profile_root, now),
                    )
                },
            );
            let storage = [
                orphan,
                unregistered,
                store_telemetry.findings,
                incident_debris,
                profile_retention_backlog,
                project_retention_backlog,
                code_generation_retention,
            ]
            .into_iter()
            .reduce(merge_storage_reads)
            .unwrap_or(DoctorStorageFamilyReadV1::Absent);
            let observability = observability_read_from_model(observability_read);
            let host = match host_read {
                Ok(read) => read,
                Err(_) => HostIntegrationReadV1::Unknown,
            };
            let inputs = DoctorKernelInputsV1 {
                configuration: configuration_read_from_pin::<crate::errors::TraceDecayError>(
                    &pinned,
                ),
                runtime: runtime_health_read(&DaemonRuntimeHealthSignalV1 {
                    serving: true,
                    startup_converged: graph_authority_current && registered_authority_current,
                    quick_check_ok,
                    // The exhaustive invariant pass
                    // (`validate_observation_authority_connection`) observed just
                    // above, never a boolean re-derived from schema and write-scope
                    // currency — that is a different question and is already
                    // reported through `startup_converged`. `None` here means the
                    // audit genuinely could not run and drops runtime coverage to
                    // partial, exactly as the coverage split intends.
                    authority_audit_ok,
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
                code_index,
                observability,
                storage,
            };
            let report = compose_doctor_report(&context, &inputs).await?;
            Ok(crate::dashboard::AdmittedDoctorReportV1::new(report)
                .with_table_growth_evidence(store_telemetry.table_growth_evidence))
        })
    })
}

fn merge_storage_reads(
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
        // A named degradation survives the merge with its reason intact: no
        // producer that explained itself is reported as merely undetermined.
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

pub(crate) fn doctor_report_request_context(
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

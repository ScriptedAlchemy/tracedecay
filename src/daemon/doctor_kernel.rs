//! Daemon-side adapters for the Doctor kernel source ports (Plan 09 §PR14).
//!
//! The transport-neutral Doctor kernel
//! ([`tracedecay_application::doctor`]) defines five narrow source ports and one
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
//! The [`compose_doctor_report`] factory wires all five adapters into the kernel
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

use std::path::Path;

use tracedecay_application::ApplicationContractError;
use tracedecay_application::RequestContext;
use tracedecay_application::doctor::{
    CodeIndexMountDoctorPort, CodeIndexMountReadV1, CodeIndexMountStateV1,
    ConfigurationAuthorityDoctorPort, ConfigurationAuthorityReadV1, ConfigurationDriftV1,
    DoctorCoverageCompletenessV1, DoctorReportComposerV1, DoctorReportV1, DoctorSourceFuture,
    DoctorStorageFamilyReadV1, DoctorStorageFindingV1, HostConformanceV1,
    HostIntegrationDoctorPort, HostIntegrationReadV1, RuntimeHealthDoctorPort, RuntimeHealthReadV1,
    RuntimeLivenessV1, StorageDoctorPort,
};

use crate::config::PinnedRuntimeConfiguration;

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

/// The five resolved kernel reads a daemon-owned Doctor report composes from.
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
    /// Code/semantic index mount read (`SemanticIndex` family).
    pub code_index: CodeIndexMountReadV1,
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
            code_index: CodeIndexMountReadV1::Unknown,
            storage: DoctorStorageFamilyReadV1::Unknown,
        }
    }
}

/// Compose a Doctor report from the daemon-owned source adapters.
///
/// Wires all five adapters into the kernel [`DoctorReportComposerV1`] and
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
    let code_index = CodeIndexMountDoctorAdapterV1::from_read(inputs.code_index.clone());
    let storage = StorageDoctorAdapterV1::from_read(inputs.storage.clone());

    let composer = DoctorReportComposerV1::new()
        .with_configuration(&configuration)
        .with_runtime(&runtime)
        .with_host(&host)
        .with_code_index(&code_index)
        .with_storage(&storage);

    composer.compose(context).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;

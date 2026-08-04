//! Doctor source ports and per-source finding producers (Plan 09 §PR14).
//!
//! The one Doctor use case composes findings from several owning authorities.
//! Each authority is reached through a narrow, transport-neutral *source port*
//! defined here — the same seam pattern as
//! [`StoreSizeTelemetryPort`](crate::storage::StoreSizeTelemetryPort): the trait
//! and its typed read model live in this crate, and the implementation is owned
//! by the runtime/host/configuration component that actually reads the source.
//! This crate holds only the trait, the typed read, and the pure producer that
//! maps a read into a [`DoctorFindingV1`].
//!
//! Every read model is *total*: it never fails silently into a healthy or empty
//! result. An unsupported platform reports `Unsupported`, a denied read reports
//! `Denied`, an undetermined read reports `Unknown`, and a supported-but-empty
//! source reports `Absent`. Each maps to a distinct, honest
//! [`DoctorEvidenceStateV1`]; only a genuinely clean, fully covered observation
//! becomes [`DoctorEvidenceStateV1::HealthyCompleteCoverage`].
//!
//! Family mapping (the finding-family enum is fixed; a source never widens it):
//! - configuration authority (resolve/pin health) → [`DoctorFindingFamilyV1::Configuration`]
//! - daemon/runtime health snapshot → [`DoctorFindingFamilyV1::StorageRuntime`]
//! - host/agent integration conformance → [`DoctorFindingFamilyV1::Advisory`] (PR13
//!   host-capability/conformance evidence)
//! - mounted canonical feedback owner → [`DoctorFindingFamilyV1::Advisory`]
//!   (finding/scope/generation/provider/evidence/coverage identity)
//! - code/semantic index mount state → [`DoctorFindingFamilyV1::SemanticIndex`]
//! - live language-server/analyzer state → [`DoctorFindingFamilyV1::LanguageServer`]
//! - durable Plan-26 feedback observations → [`DoctorFindingFamilyV1::Observability`]
//! - storage retention/size → [`DoctorFindingFamilyV1::Storage`] (producers in
//!   [`crate::storage::findings`]; this port collects their typed findings)

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CodeGenerationId, FeedbackCycleId, FeedbackCycleTerminationV1, FeedbackFindingId,
    FeedbackFindingLifecycleV1, FeedbackResultId, FeedbackScopeV1, ProviderEvaluationStateV1,
    RetrievalAnchorId,
};

use crate::RequestContext;
use crate::error::ApplicationContractError;

use super::remediation::operations;
use super::types::{
    DoctorCoverageCompletenessV1, DoctorCoverageStatementV1, DoctorEvidenceRefV1,
    DoctorEvidenceReferenceV1, DoctorEvidenceStateV1, DoctorFindingFamilyV1, DoctorFindingV1,
    DoctorOwningOperationRefV1, DoctorRemediationKindV1, DoctorRemediationRefV1,
    DoctorStorageFindingV1,
};

/// Boxed future returned by a Doctor source port, mirroring the storage and
/// diagnostic-provider port convention (std `Future`, no runtime dependency).
pub type DoctorSourceFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

// --- Shared finding builders -------------------------------------------------

/// Build a single-evidence finding for a source producer.
fn source_finding(
    family: DoctorFindingFamilyV1,
    state: DoctorEvidenceStateV1,
    reference: &str,
    completeness: DoctorCoverageCompletenessV1,
    statement: &str,
    remediation: Option<DoctorRemediationRefV1>,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    let evidence = DoctorEvidenceRefV1::new(family, DoctorEvidenceReferenceV1::new(reference)?);
    DoctorFindingV1::new(
        family,
        state,
        vec![evidence],
        DoctorCoverageStatementV1::new(completeness, statement)?,
        remediation,
    )
}

/// Build an honest non-healthy finding for an unobservable source read, carrying
/// no remediation (nothing is proven to repair).
fn unobservable_finding(
    family: DoctorFindingFamilyV1,
    state: DoctorEvidenceStateV1,
    reference: &str,
    statement: &str,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    source_finding(
        family,
        state,
        reference,
        DoctorCoverageCompletenessV1::Unknown,
        statement,
        None,
    )
}

/// Build a remediation reference (mutating action) to an owning operation.
fn action_remediation(operation: &str) -> Result<DoctorRemediationRefV1, ApplicationContractError> {
    Ok(DoctorRemediationRefV1::new(
        DoctorOwningOperationRefV1::new(operation)?,
        DoctorRemediationKindV1::Action,
    ))
}

/// Map a clean observation into a healthy finding (complete coverage) or an
/// honest `Partial` finding (incomplete coverage). Never carries remediation.
fn clean_finding(
    family: DoctorFindingFamilyV1,
    reference: &str,
    completeness: DoctorCoverageCompletenessV1,
    statement: &str,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    let state = match completeness {
        DoctorCoverageCompletenessV1::Complete => DoctorEvidenceStateV1::HealthyCompleteCoverage,
        DoctorCoverageCompletenessV1::Partial | DoctorCoverageCompletenessV1::Unknown => {
            DoctorEvidenceStateV1::Partial
        }
    };
    source_finding(family, state, reference, completeness, statement, None)
}

// --- Configuration authority (Configuration family) --------------------------

/// The observed drift between desired and effective configuration.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationDriftV1 {
    /// Desired and effective configuration agree.
    InSync,
    /// Effective configuration diverges from the desired/resolved authority.
    Drifted,
    /// A requested pin could not be honored by the authority.
    PinUnavailable,
}

/// One configuration-authority resolve/pin health read (Plan 20).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ConfigurationAuthorityReadV1 {
    /// The authority resolved and reported its drift with the given coverage.
    Resolved {
        drift: ConfigurationDriftV1,
        coverage: DoctorCoverageCompletenessV1,
    },
    /// Configuration resolution is not supported on this build/platform.
    Unsupported,
    /// The authority is reachable but has produced no resolution yet.
    Absent,
    /// Authorization to read the configuration authority was denied.
    Denied,
    /// The configuration state could not be determined.
    Unknown,
}

/// Map a configuration-authority read into its `Configuration`-family finding.
pub fn configuration_finding(
    read: &ConfigurationAuthorityReadV1,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    let family = DoctorFindingFamilyV1::Configuration;
    match read {
        ConfigurationAuthorityReadV1::Resolved { drift, coverage } => match drift {
            ConfigurationDriftV1::InSync => clean_finding(
                family,
                "configuration.resolved.in-sync",
                *coverage,
                "effective configuration matches the resolved authority",
            ),
            ConfigurationDriftV1::Drifted => source_finding(
                family,
                DoctorEvidenceStateV1::Degraded,
                "configuration.resolved.drifted",
                *coverage,
                "effective configuration diverges from the desired authority",
                Some(action_remediation(
                    operations::CONFIGURATION_PROTECTED_APPLY,
                )?),
            ),
            ConfigurationDriftV1::PinUnavailable => source_finding(
                family,
                DoctorEvidenceStateV1::Degraded,
                "configuration.resolved.pin-unavailable",
                *coverage,
                "a requested configuration pin could not be honored",
                Some(action_remediation(operations::CONFIGURATION_PIN_AUTHORITY)?),
            ),
        },
        ConfigurationAuthorityReadV1::Unsupported => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Unsupported,
            "configuration.unsupported",
            "configuration resolution unsupported on this platform",
        ),
        ConfigurationAuthorityReadV1::Absent => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Absent,
            "configuration.absent",
            "configuration authority produced no resolution",
        ),
        ConfigurationAuthorityReadV1::Denied => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Denied,
            "configuration.denied",
            "configuration authority read denied",
        ),
        ConfigurationAuthorityReadV1::Unknown => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Unknown,
            "configuration.unknown",
            "configuration state undetermined",
        ),
    }
}

/// Narrow source port for configuration resolve/pin health (Plan 20).
pub trait ConfigurationAuthorityDoctorPort: Send + Sync {
    /// Read the current configuration authority resolve/pin health.
    fn configuration_health<'a>(
        &'a self,
        context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, ConfigurationAuthorityReadV1>;
}

// --- Daemon/runtime health (StorageRuntime family) ---------------------------

/// The observed liveness of the daemon/runtime.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeLivenessV1 {
    /// The runtime is live and serving.
    Healthy,
    /// The runtime is serving but degraded (for example a lease under pressure).
    Degraded,
    /// The runtime is stuck (for example an unresolvable reader lease).
    Stuck,
    /// The runtime is unreachable.
    Unreachable,
}

/// One daemon/runtime health snapshot read (store, graph, temporal, migration).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RuntimeHealthReadV1 {
    /// The runtime reported liveness with the given coverage.
    Observed {
        liveness: RuntimeLivenessV1,
        coverage: DoctorCoverageCompletenessV1,
    },
    /// Runtime health telemetry is unsupported on this build/platform.
    Unsupported,
    /// The runtime is reachable but reported no health snapshot.
    Absent,
    /// Authorization to read the runtime health was denied.
    Denied,
    /// The runtime health could not be determined.
    Unknown,
}

/// Map a runtime-health read into its `StorageRuntime`-family finding.
pub fn runtime_health_finding(
    read: &RuntimeHealthReadV1,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    let family = DoctorFindingFamilyV1::StorageRuntime;
    match read {
        RuntimeHealthReadV1::Observed { liveness, coverage } => match liveness {
            RuntimeLivenessV1::Healthy => clean_finding(
                family,
                "runtime.health.healthy",
                *coverage,
                "daemon runtime is live and serving",
            ),
            RuntimeLivenessV1::Degraded => source_finding(
                family,
                DoctorEvidenceStateV1::Degraded,
                "runtime.health.degraded",
                *coverage,
                "daemon runtime is serving but degraded",
                Some(action_remediation(operations::RUNTIME_RECOVER_DAEMON)?),
            ),
            RuntimeLivenessV1::Stuck => source_finding(
                family,
                DoctorEvidenceStateV1::Degraded,
                "runtime.health.stuck",
                *coverage,
                "daemon runtime is stuck and awaiting recovery",
                Some(action_remediation(operations::RUNTIME_RECOVER_DAEMON)?),
            ),
            RuntimeLivenessV1::Unreachable => {
                // Unreachable is genuinely undetermined health, not a proven
                // degraded condition, but recovery is still the owning action.
                source_finding(
                    family,
                    DoctorEvidenceStateV1::Unknown,
                    "runtime.health.unreachable",
                    DoctorCoverageCompletenessV1::Unknown,
                    "daemon runtime is unreachable",
                    Some(action_remediation(operations::RUNTIME_RECOVER_DAEMON)?),
                )
            }
        },
        RuntimeHealthReadV1::Unsupported => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Unsupported,
            "runtime.unsupported",
            "runtime health telemetry unsupported on this platform",
        ),
        RuntimeHealthReadV1::Absent => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Absent,
            "runtime.absent",
            "runtime reported no health snapshot",
        ),
        RuntimeHealthReadV1::Denied => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Denied,
            "runtime.denied",
            "runtime health read denied",
        ),
        RuntimeHealthReadV1::Unknown => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Unknown,
            "runtime.unknown",
            "runtime health undetermined",
        ),
    }
}

/// Narrow source port for a daemon/runtime health snapshot.
pub trait RuntimeHealthDoctorPort: Send + Sync {
    /// Read the current daemon/runtime health snapshot.
    fn runtime_health<'a>(
        &'a self,
        context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, RuntimeHealthReadV1>;
}

// --- Operational runtime authorities (StorageRuntime family) ----------------

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteListenerReadV1 {
    Serving,
    Disabled,
    Degraded,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteAuthorityReadV1 {
    Available,
    Partial,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RemoteOperationalReadV1 {
    Observed {
        listener: RemoteListenerReadV1,
        authority: RemoteAuthorityReadV1,
        pending_spool_items: u64,
        quarantined_spool_items: u64,
        replay_coverage_complete: bool,
        backup_verified: bool,
        failover_in_progress: bool,
        recovery_required: bool,
        coverage: DoctorCoverageCompletenessV1,
    },
    Unconfigured,
    Unsupported,
    Denied,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ProfileAuthorityReadV1 {
    Observed {
        registry_attached: bool,
        profile_sessions_attached: bool,
        coverage: DoctorCoverageCompletenessV1,
    },
    Denied,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationalAuditReadV1 {
    pub remote: RemoteOperationalReadV1,
    pub profile_authority: ProfileAuthorityReadV1,
}

pub fn operational_audit_findings(
    read: &OperationalAuditReadV1,
) -> Result<Vec<DoctorFindingV1>, ApplicationContractError> {
    Ok(vec![
        remote_operational_finding(&read.remote)?,
        profile_authority_finding(&read.profile_authority)?,
    ])
}

fn remote_operational_finding(
    read: &RemoteOperationalReadV1,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    let family = DoctorFindingFamilyV1::StorageRuntime;
    match read {
        RemoteOperationalReadV1::Observed {
            listener,
            authority,
            quarantined_spool_items,
            replay_coverage_complete,
            backup_verified,
            failover_in_progress,
            recovery_required,
            coverage,
            ..
        } if *recovery_required || *quarantined_spool_items > 0 => source_finding(
            family,
            DoctorEvidenceStateV1::Degraded,
            "remote.operational.recovery-required",
            *coverage,
            "remote HTTPS authority or spool requires recovery",
            Some(action_remediation(operations::RUNTIME_RECOVER_DAEMON)?),
        ),
        RemoteOperationalReadV1::Observed {
            listener: RemoteListenerReadV1::Serving,
            authority: RemoteAuthorityReadV1::Available,
            replay_coverage_complete: true,
            backup_verified: true,
            failover_in_progress: false,
            coverage,
            ..
        } => clean_finding(
            family,
            "remote.operational.ready",
            *coverage,
            "remote HTTPS listener, authority, spool, replay, and backup are ready",
        ),
        RemoteOperationalReadV1::Observed {
            coverage,
            listener,
            authority,
            replay_coverage_complete,
            backup_verified,
            failover_in_progress,
            ..
        } => {
            let _ = (
                listener,
                authority,
                replay_coverage_complete,
                backup_verified,
                failover_in_progress,
            );
            source_finding(
                family,
                DoctorEvidenceStateV1::Degraded,
                "remote.operational.partial",
                *coverage,
                "remote HTTPS listener, authority, spool, replay, or backup is incomplete",
                Some(action_remediation(operations::RUNTIME_RECOVER_DAEMON)?),
            )
        }
        RemoteOperationalReadV1::Unconfigured => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Absent,
            "remote.operational.unconfigured",
            "optional remote HTTPS capability is unconfigured",
        ),
        RemoteOperationalReadV1::Unsupported => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Unsupported,
            "remote.operational.unsupported",
            "remote HTTPS capability is unsupported on this platform",
        ),
        RemoteOperationalReadV1::Denied => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Denied,
            "remote.operational.denied",
            "remote operational authority read was denied",
        ),
        RemoteOperationalReadV1::Unavailable => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Unknown,
            "remote.operational.unavailable",
            "remote operational authority is unavailable",
        ),
    }
}

fn profile_authority_finding(
    read: &ProfileAuthorityReadV1,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    let family = DoctorFindingFamilyV1::StorageRuntime;
    match read {
        ProfileAuthorityReadV1::Observed {
            registry_attached: true,
            profile_sessions_attached: true,
            coverage,
        } => clean_finding(
            family,
            "profile.authority.registered",
            *coverage,
            "the exact registered profile and profile-session authorities are attached",
        ),
        ProfileAuthorityReadV1::Observed { coverage, .. } => source_finding(
            family,
            DoctorEvidenceStateV1::Degraded,
            "profile.authority.incomplete",
            *coverage,
            "the exact registered profile authority is only partially attached",
            Some(action_remediation(operations::RUNTIME_RECOVER_DAEMON)?),
        ),
        ProfileAuthorityReadV1::Denied => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Denied,
            "profile.authority.denied",
            "the exact registered profile authority read was denied",
        ),
        ProfileAuthorityReadV1::Unavailable => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Unknown,
            "profile.authority.unavailable",
            "the exact registered profile authority is unavailable",
        ),
    }
}

pub trait OperationalAuditDoctorPort: Send + Sync {
    fn operational_audit<'a>(
        &'a self,
        context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, OperationalAuditReadV1>;
}

// --- Host/agent integration conformance (Advisory family) --------------------

/// The observed conformance of a host/agent integration (Plan 27).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum HostConformanceV1 {
    /// The integration matches the expected installed shape.
    Conformant,
    /// The integration is installed but has drifted from the expected shape.
    Drifted,
    /// The integration's executable is absent.
    ExecutableAbsent,
    /// The integration's protocol/version has drifted.
    ProtocolDrift,
    /// A configured fallback is invalid.
    InvalidFallback,
}

/// One host/agent integration conformance read (Plan 27).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum HostIntegrationReadV1 {
    /// The host reported conformance with the given coverage.
    Observed {
        conformance: HostConformanceV1,
        coverage: DoctorCoverageCompletenessV1,
    },
    /// Host conformance probing is unsupported on this build/platform.
    Unsupported,
    /// No host integration is present to probe.
    Absent,
    /// Authorization to probe the host integration was denied.
    Denied,
    /// The host conformance could not be determined.
    Unknown,
}

/// Map a host-integration conformance read into its `Advisory`-family finding.
pub fn host_integration_finding(
    read: &HostIntegrationReadV1,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    let family = DoctorFindingFamilyV1::Advisory;
    let repair = || action_remediation(operations::HOST_REPAIR_INTEGRATION);
    match read {
        HostIntegrationReadV1::Observed {
            conformance,
            coverage,
        } => match conformance {
            HostConformanceV1::Conformant => clean_finding(
                family,
                "host.conformance.conformant",
                *coverage,
                "host integration matches the expected installed shape",
            ),
            HostConformanceV1::Drifted => source_finding(
                family,
                DoctorEvidenceStateV1::Degraded,
                "host.conformance.drifted",
                *coverage,
                "host integration has drifted from the expected shape",
                Some(repair()?),
            ),
            HostConformanceV1::ExecutableAbsent => source_finding(
                family,
                DoctorEvidenceStateV1::Degraded,
                "host.conformance.executable-absent",
                *coverage,
                "host integration executable is absent",
                Some(repair()?),
            ),
            HostConformanceV1::ProtocolDrift => source_finding(
                family,
                DoctorEvidenceStateV1::Degraded,
                "host.conformance.protocol-drift",
                *coverage,
                "host integration protocol/version has drifted",
                Some(repair()?),
            ),
            HostConformanceV1::InvalidFallback => source_finding(
                family,
                DoctorEvidenceStateV1::Degraded,
                "host.conformance.invalid-fallback",
                *coverage,
                "host integration fallback is invalid",
                Some(repair()?),
            ),
        },
        HostIntegrationReadV1::Unsupported => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Unsupported,
            "host.unsupported",
            "host conformance probing unsupported on this platform",
        ),
        HostIntegrationReadV1::Absent => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Absent,
            "host.absent",
            "no host integration present to probe",
        ),
        HostIntegrationReadV1::Denied => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Denied,
            "host.denied",
            "host integration probe denied",
        ),
        HostIntegrationReadV1::Unknown => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Unknown,
            "host.unknown",
            "host conformance undetermined",
        ),
    }
}

/// Narrow source port for host/agent integration conformance (Plan 27).
pub trait HostIntegrationDoctorPort: Send + Sync {
    /// Read the current host/agent integration conformance.
    fn host_conformance<'a>(
        &'a self,
        context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, HostIntegrationReadV1>;
}

// --- Canonical advisory feedback (Advisory family) --------------------------

/// One canonical advisory finding projected from the mounted feedback read
/// model. Identity and scope remain typed until Doctor converts them into
/// durable evidence references.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdvisoryFeedbackFindingReadV1 {
    pub result_id: FeedbackResultId,
    pub cycle_id: FeedbackCycleId,
    pub finding_id: FeedbackFindingId,
    pub scope: FeedbackScopeV1,
    pub generation_id: CodeGenerationId,
    pub generation_current: bool,
    pub lifecycle: FeedbackFindingLifecycleV1,
    pub provider_state: ProviderEvaluationStateV1,
    pub evidence_anchors: Vec<RetrievalAnchorId>,
    pub total_findings: u64,
    pub returned_findings: u64,
    pub omitted_findings: u64,
}

/// Result-level identity and denominator state retained even when a bounded
/// canonical publication returns no finding rows.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdvisoryFeedbackSummaryReadV1 {
    pub result_id: FeedbackResultId,
    pub cycle_id: FeedbackCycleId,
    pub scope: FeedbackScopeV1,
    pub generation_id: CodeGenerationId,
    pub generation_current: bool,
    pub termination: FeedbackCycleTerminationV1,
    pub provider_states: Vec<ProviderEvaluationStateV1>,
    pub total_findings: u64,
    pub returned_findings: u64,
    pub omitted_findings: u64,
}

/// Canonical feedback-owner read for Doctor's advisory source.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AdvisoryFeedbackReadV1 {
    Observed {
        summary: Box<AdvisoryFeedbackSummaryReadV1>,
        findings: Vec<AdvisoryFeedbackFindingReadV1>,
    },
    Unsupported,
    Absent,
    Denied,
    Unknown,
}

const fn feedback_lifecycle_slug(lifecycle: FeedbackFindingLifecycleV1) -> &'static str {
    match lifecycle {
        FeedbackFindingLifecycleV1::Active => "active",
        FeedbackFindingLifecycleV1::Superseded => "superseded",
        FeedbackFindingLifecycleV1::Resolved => "resolved",
        FeedbackFindingLifecycleV1::Cleared => "cleared",
    }
}

const fn feedback_provider_state_slug(state: ProviderEvaluationStateV1) -> &'static str {
    match state {
        ProviderEvaluationStateV1::SupportedCompletedComplete => "supported_completed_complete",
        ProviderEvaluationStateV1::Unsupported => "unsupported",
        ProviderEvaluationStateV1::Absent => "absent",
        ProviderEvaluationStateV1::Indexing => "indexing",
        ProviderEvaluationStateV1::Stale => "stale",
        ProviderEvaluationStateV1::Cancelled => "cancelled",
        ProviderEvaluationStateV1::TimedOut => "timed_out",
        ProviderEvaluationStateV1::Failed => "failed",
        ProviderEvaluationStateV1::Partial => "partial",
        ProviderEvaluationStateV1::Unavailable => "unavailable",
    }
}

fn feedback_counts_coverage(
    total: u64,
    returned: u64,
    omitted: u64,
) -> DoctorCoverageCompletenessV1 {
    if returned > total || omitted != total - returned {
        DoctorCoverageCompletenessV1::Unknown
    } else if omitted == 0 {
        DoctorCoverageCompletenessV1::Complete
    } else {
        DoctorCoverageCompletenessV1::Partial
    }
}

fn feedback_evidence(
    reference: impl Into<String>,
) -> Result<DoctorEvidenceRefV1, ApplicationContractError> {
    Ok(DoctorEvidenceRefV1::new(
        DoctorFindingFamilyV1::Advisory,
        DoctorEvidenceReferenceV1::new(reference)?,
    ))
}

fn feedback_identity_evidence(
    result_id: &FeedbackResultId,
    cycle_id: &FeedbackCycleId,
    scope: &FeedbackScopeV1,
    generation_id: &CodeGenerationId,
) -> Result<Vec<DoctorEvidenceRefV1>, ApplicationContractError> {
    Ok(vec![
        feedback_evidence(format!("feedback.result:{}", result_id.as_str()))?,
        feedback_evidence(format!("feedback.cycle:{}", cycle_id.as_str()))?,
        feedback_evidence(format!(
            "feedback.scope.project:{}",
            scope.project_id.as_str()
        ))?,
        feedback_evidence(format!(
            "feedback.scope.repository:{}",
            scope.repository_id.as_str()
        ))?,
        feedback_evidence(format!(
            "feedback.scope.worktree:{}",
            scope.worktree_id.as_str()
        ))?,
        feedback_evidence(format!("feedback.scope.branch:{}", scope.branch_ref))?,
        feedback_evidence(format!(
            "feedback.scope.head:{}",
            scope.head_commit_id.as_str()
        ))?,
        feedback_evidence(format!("feedback.generation:{}", generation_id.as_str()))?,
    ])
}

fn advisory_feedback_finding(
    read: &AdvisoryFeedbackFindingReadV1,
    summary: &AdvisoryFeedbackSummaryReadV1,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    let count_coverage = feedback_counts_coverage(
        read.total_findings,
        read.returned_findings,
        read.omitted_findings,
    );
    let providers_complete = !summary.provider_states.is_empty()
        && summary
            .provider_states
            .iter()
            .all(|state| *state == ProviderEvaluationStateV1::SupportedCompletedComplete);
    let coverage_complete = read.generation_current
        && count_coverage == DoctorCoverageCompletenessV1::Complete
        && providers_complete;
    let completeness = if count_coverage == DoctorCoverageCompletenessV1::Unknown
        || summary.provider_states.is_empty()
    {
        DoctorCoverageCompletenessV1::Unknown
    } else if coverage_complete {
        DoctorCoverageCompletenessV1::Complete
    } else {
        DoctorCoverageCompletenessV1::Partial
    };
    let state = if !read.generation_current
        || summary.termination == FeedbackCycleTerminationV1::StaleReplanRequired
        || summary
            .provider_states
            .contains(&ProviderEvaluationStateV1::Stale)
    {
        DoctorEvidenceStateV1::Stale
    } else if completeness == DoctorCoverageCompletenessV1::Unknown {
        DoctorEvidenceStateV1::Unknown
    } else {
        match read.provider_state {
            ProviderEvaluationStateV1::SupportedCompletedComplete => {
                if matches!(
                    read.lifecycle,
                    FeedbackFindingLifecycleV1::Resolved
                        | FeedbackFindingLifecycleV1::Cleared
                        | FeedbackFindingLifecycleV1::Superseded
                ) {
                    if coverage_complete {
                        DoctorEvidenceStateV1::HealthyCompleteCoverage
                    } else {
                        DoctorEvidenceStateV1::Partial
                    }
                } else {
                    DoctorEvidenceStateV1::Degraded
                }
            }
            ProviderEvaluationStateV1::Unsupported => DoctorEvidenceStateV1::Unsupported,
            ProviderEvaluationStateV1::Absent => DoctorEvidenceStateV1::Absent,
            ProviderEvaluationStateV1::Indexing | ProviderEvaluationStateV1::Stale => {
                DoctorEvidenceStateV1::Stale
            }
            ProviderEvaluationStateV1::Partial => DoctorEvidenceStateV1::Partial,
            ProviderEvaluationStateV1::Cancelled
            | ProviderEvaluationStateV1::TimedOut
            | ProviderEvaluationStateV1::Failed
            | ProviderEvaluationStateV1::Unavailable => DoctorEvidenceStateV1::Unknown,
        }
    };
    let mut evidence = feedback_identity_evidence(
        &read.result_id,
        &read.cycle_id,
        &read.scope,
        &read.generation_id,
    )?;
    evidence.extend([
        feedback_evidence(format!("feedback.finding:{}", read.finding_id.as_str()))?,
        feedback_evidence(format!(
            "feedback.generation_state:{}",
            if read.generation_current {
                "current"
            } else {
                "stale"
            }
        ))?,
        feedback_evidence(format!(
            "feedback.lifecycle:{}",
            feedback_lifecycle_slug(read.lifecycle)
        ))?,
        feedback_evidence(format!(
            "feedback.provider_state:{}",
            feedback_provider_state_slug(read.provider_state)
        ))?,
    ]);
    for anchor in &read.evidence_anchors {
        evidence.push(feedback_evidence(format!(
            "feedback.anchor:{}",
            anchor.as_str()
        ))?);
    }
    let statement = format!(
        "feedback coverage returned {}/{} findings; omitted {}",
        read.returned_findings, read.total_findings, read.omitted_findings
    );
    DoctorFindingV1::new(
        DoctorFindingFamilyV1::Advisory,
        state,
        evidence,
        DoctorCoverageStatementV1::new(completeness, statement)?,
        None,
    )
}

fn advisory_feedback_summary_finding(
    read: &AdvisoryFeedbackSummaryReadV1,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    let count_completeness = feedback_counts_coverage(
        read.total_findings,
        read.returned_findings,
        read.omitted_findings,
    );
    let completeness = if read.generation_current {
        count_completeness
    } else {
        DoctorCoverageCompletenessV1::Partial
    };
    let coverage_complete =
        read.generation_current && count_completeness == DoctorCoverageCompletenessV1::Complete;
    let providers_complete = !read.provider_states.is_empty()
        && read
            .provider_states
            .iter()
            .all(|state| *state == ProviderEvaluationStateV1::SupportedCompletedComplete);
    let state = if !read.generation_current {
        DoctorEvidenceStateV1::Stale
    } else if completeness == DoctorCoverageCompletenessV1::Unknown {
        DoctorEvidenceStateV1::Unknown
    } else if read.termination == FeedbackCycleTerminationV1::Clean
        && coverage_complete
        && providers_complete
    {
        DoctorEvidenceStateV1::HealthyCompleteCoverage
    } else if read.termination == FeedbackCycleTerminationV1::StaleReplanRequired
        || read
            .provider_states
            .contains(&ProviderEvaluationStateV1::Stale)
    {
        DoctorEvidenceStateV1::Stale
    } else if !coverage_complete
        || read
            .provider_states
            .contains(&ProviderEvaluationStateV1::Partial)
    {
        DoctorEvidenceStateV1::Partial
    } else {
        DoctorEvidenceStateV1::Unknown
    };
    let mut evidence = feedback_identity_evidence(
        &read.result_id,
        &read.cycle_id,
        &read.scope,
        &read.generation_id,
    )?;
    evidence.push(feedback_evidence(format!(
        "feedback.generation_state:{}",
        if read.generation_current {
            "current"
        } else {
            "stale"
        }
    ))?);
    let statement = format!(
        "feedback coverage returned {}/{} findings; omitted {}",
        read.returned_findings, read.total_findings, read.omitted_findings
    );
    DoctorFindingV1::new(
        DoctorFindingFamilyV1::Advisory,
        state,
        evidence,
        DoctorCoverageStatementV1::new(completeness, statement)?,
        None,
    )
}

fn advisory_feedback_observation_is_consistent(
    summary: &AdvisoryFeedbackSummaryReadV1,
    findings: &[AdvisoryFeedbackFindingReadV1],
) -> bool {
    feedback_counts_coverage(
        summary.total_findings,
        summary.returned_findings,
        summary.omitted_findings,
    ) != DoctorCoverageCompletenessV1::Unknown
        && summary.returned_findings == findings.len() as u64
        && summary
            .termination
            .is_consistent_with_provider_states(&summary.provider_states)
        && findings.iter().all(|finding| {
            finding.result_id == summary.result_id
                && finding.cycle_id == summary.cycle_id
                && finding.scope == summary.scope
                && finding.generation_id == summary.generation_id
                && finding.generation_current == summary.generation_current
                && finding.total_findings == summary.total_findings
                && finding.returned_findings == summary.returned_findings
                && finding.omitted_findings == summary.omitted_findings
                && summary.provider_states.contains(&finding.provider_state)
        })
}

/// Map the mounted canonical feedback-owner read into distinct Advisory
/// findings. Host conformance is deliberately not part of this producer.
pub fn advisory_feedback_findings(
    read: &AdvisoryFeedbackReadV1,
) -> Result<Vec<DoctorFindingV1>, ApplicationContractError> {
    match read {
        AdvisoryFeedbackReadV1::Observed { summary, findings } => {
            if !advisory_feedback_observation_is_consistent(summary, findings) {
                return Err(ApplicationContractError::Inconsistent {
                    field: "Doctor advisory feedback read",
                });
            }
            if findings.is_empty() {
                Ok(vec![advisory_feedback_summary_finding(summary)?])
            } else {
                findings
                    .iter()
                    .map(|finding| advisory_feedback_finding(finding, summary))
                    .collect()
            }
        }
        AdvisoryFeedbackReadV1::Absent => Ok(vec![unobservable_finding(
            DoctorFindingFamilyV1::Advisory,
            DoctorEvidenceStateV1::Absent,
            "feedback.absent",
            "canonical advisory feedback produced no findings",
        )?]),
        AdvisoryFeedbackReadV1::Unsupported => Ok(vec![unobservable_finding(
            DoctorFindingFamilyV1::Advisory,
            DoctorEvidenceStateV1::Unsupported,
            "feedback.unsupported",
            "canonical advisory feedback unsupported",
        )?]),
        AdvisoryFeedbackReadV1::Denied => Ok(vec![unobservable_finding(
            DoctorFindingFamilyV1::Advisory,
            DoctorEvidenceStateV1::Denied,
            "feedback.denied",
            "canonical advisory feedback read denied",
        )?]),
        AdvisoryFeedbackReadV1::Unknown => Ok(vec![unobservable_finding(
            DoctorFindingFamilyV1::Advisory,
            DoctorEvidenceStateV1::Unknown,
            "feedback.unknown",
            "canonical advisory feedback undetermined",
        )?]),
    }
}

/// Narrow Doctor port owned by the mounted canonical feedback read model.
pub trait AdvisoryFeedbackDoctorPort: Send + Sync {
    fn advisory_feedback<'a>(
        &'a self,
        context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, AdvisoryFeedbackReadV1>;
}

// --- Code/semantic index mount (SemanticIndex family) ------------------------

/// The observed mount state of the code/semantic index.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CodeIndexMountStateV1 {
    /// The index is mounted and current.
    Mounted,
    /// The index is mounting/indexing and not yet complete.
    Indexing,
    /// The index is mounted but behind the current generation.
    Stale,
    /// The index is not mounted.
    Unmounted,
    /// The mounted index is incompatible with the current schema/generation.
    Incompatible,
}

/// One code/semantic index mount read.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CodeIndexMountReadV1 {
    /// The index reported its mount state with the given coverage.
    Observed {
        state: CodeIndexMountStateV1,
        coverage: DoctorCoverageCompletenessV1,
    },
    /// Index mount inspection is unsupported on this build/platform.
    Unsupported,
    /// No index is present to inspect.
    Absent,
    /// Authorization to inspect the index was denied.
    Denied,
    /// The index mount state could not be determined.
    Unknown,
}

/// Map a code-index mount read into its `SemanticIndex`-family finding.
pub fn code_index_finding(
    read: &CodeIndexMountReadV1,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    let family = DoctorFindingFamilyV1::SemanticIndex;
    let remount = || action_remediation(operations::CODE_INDEX_REMOUNT);
    match read {
        CodeIndexMountReadV1::Observed { state, coverage } => match state {
            CodeIndexMountStateV1::Mounted => clean_finding(
                family,
                "code-index.mount.mounted",
                *coverage,
                "code index is mounted and current",
            ),
            CodeIndexMountStateV1::Indexing => source_finding(
                family,
                DoctorEvidenceStateV1::Partial,
                "code-index.mount.indexing",
                DoctorCoverageCompletenessV1::Partial,
                "code index is still indexing",
                None,
            ),
            CodeIndexMountStateV1::Stale => source_finding(
                family,
                DoctorEvidenceStateV1::Stale,
                "code-index.mount.stale",
                *coverage,
                "code index is behind the current generation",
                Some(remount()?),
            ),
            CodeIndexMountStateV1::Unmounted => source_finding(
                family,
                DoctorEvidenceStateV1::Degraded,
                "code-index.mount.unmounted",
                *coverage,
                "code index is not mounted",
                Some(remount()?),
            ),
            CodeIndexMountStateV1::Incompatible => source_finding(
                family,
                DoctorEvidenceStateV1::Degraded,
                "code-index.mount.incompatible",
                *coverage,
                "mounted code index is incompatible with the current schema",
                Some(remount()?),
            ),
        },
        CodeIndexMountReadV1::Unsupported => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Unsupported,
            "code-index.unsupported",
            "code index inspection unsupported on this platform",
        ),
        CodeIndexMountReadV1::Absent => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Absent,
            "code-index.absent",
            "no code index present to inspect",
        ),
        CodeIndexMountReadV1::Denied => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Denied,
            "code-index.denied",
            "code index inspection denied",
        ),
        CodeIndexMountReadV1::Unknown => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Unknown,
            "code-index.unknown",
            "code index mount state undetermined",
        ),
    }
}

/// Narrow source port for code/semantic index mount state.
pub trait CodeIndexMountDoctorPort: Send + Sync {
    /// Read the current code/semantic index mount state.
    fn code_index_mount<'a>(
        &'a self,
        context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, CodeIndexMountReadV1>;
}

// --- Language server/analyzer (LanguageServer family) ------------------------

/// Aggregate state of the project-active language-server analyzers.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LanguageServerStateV1 {
    /// Every active analyzer is ready.
    Ready,
    /// At least one analyzer is installed but has not produced a ready snapshot.
    Available,
    /// At least one analyzer is currently refreshing/indexing.
    Refreshing,
    /// At least one project analyzer is disabled.
    Disabled,
    /// At least one project analyzer executable is unavailable.
    Unavailable,
    /// At least one analyzer process crashed.
    Crashed,
}

/// One live read from the daemon language-server/analyzer owner.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum LanguageServerReadV1 {
    /// The owner observed all project-active analyzer states.
    Observed {
        state: LanguageServerStateV1,
        coverage: DoctorCoverageCompletenessV1,
    },
    /// Language-server inspection is unsupported on this build/platform.
    Unsupported,
    /// No project-active analyzer is configured.
    Absent,
    /// Authorization to inspect analyzer state was denied.
    Denied,
    /// Analyzer state could not be determined.
    Unknown,
}

/// Map a live analyzer read into its `LanguageServer`-family finding.
pub fn language_server_finding(
    read: &LanguageServerReadV1,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    let family = DoctorFindingFamilyV1::LanguageServer;
    match read {
        LanguageServerReadV1::Observed { state, coverage } => match state {
            LanguageServerStateV1::Ready => clean_finding(
                family,
                "language-server.analyzer.ready",
                *coverage,
                "all project-active language-server analyzers are ready",
            ),
            LanguageServerStateV1::Available => source_finding(
                family,
                DoctorEvidenceStateV1::Partial,
                "language-server.analyzer.available",
                DoctorCoverageCompletenessV1::Partial,
                "project analyzers are available but readiness is not yet observed",
                None,
            ),
            LanguageServerStateV1::Refreshing => source_finding(
                family,
                DoctorEvidenceStateV1::Partial,
                "language-server.analyzer.refreshing",
                DoctorCoverageCompletenessV1::Partial,
                "at least one project analyzer is refreshing or indexing",
                None,
            ),
            LanguageServerStateV1::Disabled => source_finding(
                family,
                DoctorEvidenceStateV1::Degraded,
                "language-server.analyzer.disabled",
                *coverage,
                "at least one project analyzer is disabled",
                None,
            ),
            LanguageServerStateV1::Unavailable => source_finding(
                family,
                DoctorEvidenceStateV1::Degraded,
                "language-server.analyzer.unavailable",
                *coverage,
                "at least one project analyzer executable is unavailable",
                None,
            ),
            LanguageServerStateV1::Crashed => source_finding(
                family,
                DoctorEvidenceStateV1::Degraded,
                "language-server.analyzer.crashed",
                *coverage,
                "at least one project analyzer process crashed",
                None,
            ),
        },
        LanguageServerReadV1::Unsupported => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Unsupported,
            "language-server.unsupported",
            "language-server inspection unsupported on this platform",
        ),
        LanguageServerReadV1::Absent => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Absent,
            "language-server.absent",
            "no project-active language-server analyzer is configured",
        ),
        LanguageServerReadV1::Denied => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Denied,
            "language-server.denied",
            "language-server analyzer inspection denied",
        ),
        LanguageServerReadV1::Unknown => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Unknown,
            "language-server.unknown",
            "language-server analyzer state undetermined",
        ),
    }
}

/// Narrow source port for live language-server/analyzer state.
pub trait LanguageServerDoctorPort: Send + Sync {
    /// Read current project-active analyzer state.
    fn language_server_health<'a>(
        &'a self,
        context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, LanguageServerReadV1>;
}

// --- Durable feedback observations (Observability family) --------------------

/// Freshness state of the canonical durable observation projection.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ObservabilityStateV1 {
    Current,
    Stale,
}

/// One canonical Plan-26 feedback-observation read.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ObservabilityReadV1 {
    /// The durable read model contains observations through this watermark.
    Observed {
        state: ObservabilityStateV1,
        total_count: u64,
        last_observed_at_micros: Option<i64>,
        coverage: DoctorCoverageCompletenessV1,
    },
    /// Durable observation projection is unsupported on this build/platform.
    Unsupported,
    /// The canonical projection contains no observations.
    Absent,
    /// Authorization to read the observation projection was denied.
    Denied,
    /// The observation state could not be determined.
    Unknown,
}

/// Map the canonical Plan-26 read model into its `Observability` finding.
pub fn observability_finding(
    read: &ObservabilityReadV1,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    let family = DoctorFindingFamilyV1::Observability;
    match read {
        ObservabilityReadV1::Observed {
            state,
            total_count,
            last_observed_at_micros,
            coverage,
        } => match state {
            ObservabilityStateV1::Stale => source_finding(
                family,
                DoctorEvidenceStateV1::Stale,
                "observability.plan26.stale",
                *coverage,
                "canonical Plan-26 feedback projection is stale at its retained watermark",
                None,
            ),
            ObservabilityStateV1::Current => {
                let statement = if last_observed_at_micros.is_some() {
                    format!(
                        "canonical Plan-26 feedback projection contains {total_count} retained observations through its latest watermark"
                    )
                } else {
                    format!(
                        "canonical Plan-26 feedback projection contains {total_count} retained observations without a watermark"
                    )
                };
                clean_finding(
                    family,
                    "observability.plan26.feedback-projection",
                    *coverage,
                    &statement,
                )
            }
        },
        ObservabilityReadV1::Unsupported => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Unsupported,
            "observability.unsupported",
            "durable feedback observation projection unsupported on this platform",
        ),
        ObservabilityReadV1::Absent => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Absent,
            "observability.plan26.absent",
            "canonical Plan-26 feedback projection contains no observations",
        ),
        ObservabilityReadV1::Denied => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Denied,
            "observability.denied",
            "durable feedback observation projection read denied",
        ),
        ObservabilityReadV1::Unknown => unobservable_finding(
            family,
            DoctorEvidenceStateV1::Unknown,
            "observability.unknown",
            "durable feedback observation state undetermined",
        ),
    }
}

/// Narrow source port for the canonical durable Plan-26 read model.
pub trait ObservabilityDoctorPort: Send + Sync {
    /// Read current durable feedback-observation state.
    fn observability_health<'a>(
        &'a self,
        context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, ObservabilityReadV1>;
}

// --- Storage retention/size (Storage family) ---------------------------------

/// One storage-source read: either the typed storage findings the runtime
/// produced via [`crate::storage::findings`], or an honest unavailability.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DoctorStorageFamilyReadV1 {
    /// The storage runtime produced these typed findings (may be empty when the
    /// profile has no stores; the composer treats an empty observed read as an
    /// absent family rather than a healthy claim).
    Observed {
        findings: Vec<DoctorStorageFindingV1>,
    },
    /// Storage retention/size telemetry is unsupported on this build/platform.
    Unsupported,
    /// The storage runtime is reachable but produced no findings.
    Absent,
    /// Authorization to read storage telemetry was denied.
    Denied,
    /// The storage state could not be determined.
    Unknown,
}

/// Narrow source port for storage retention/size Doctor findings (Plan 38 §7).
///
/// Unlike the other source ports, storage has several heterogeneous read models
/// (budget/telemetry, orphan, stale-branch, debris, backlog), each with its own
/// landed producer in [`crate::storage::findings`]. Rather than re-derive those,
/// the runtime adapter runs the producers and returns their typed
/// [`DoctorStorageFindingV1`] values through this port.
pub trait StorageDoctorPort: Send + Sync {
    /// Read the current storage retention/size findings.
    fn storage_findings<'a>(
        &'a self,
        context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, DoctorStorageFamilyReadV1>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_in_sync_complete_is_healthy() {
        let finding = configuration_finding(&ConfigurationAuthorityReadV1::Resolved {
            drift: ConfigurationDriftV1::InSync,
            coverage: DoctorCoverageCompletenessV1::Complete,
        })
        .expect("finding");
        assert!(finding.state().is_healthy_complete());
        assert!(finding.remediation().is_none());
        assert_eq!(finding.family(), DoctorFindingFamilyV1::Configuration);
    }

    #[test]
    fn configuration_drift_is_degraded_with_apply_remediation() {
        let finding = configuration_finding(&ConfigurationAuthorityReadV1::Resolved {
            drift: ConfigurationDriftV1::Drifted,
            coverage: DoctorCoverageCompletenessV1::Complete,
        })
        .expect("finding");
        assert_eq!(finding.state(), DoctorEvidenceStateV1::Degraded);
        assert_eq!(
            finding
                .remediation()
                .expect("remediation")
                .owning_operation()
                .as_str(),
            operations::CONFIGURATION_PROTECTED_APPLY
        );
    }

    #[test]
    fn configuration_unavailable_states_map_honestly() {
        for (read, expected) in [
            (
                ConfigurationAuthorityReadV1::Unsupported,
                DoctorEvidenceStateV1::Unsupported,
            ),
            (
                ConfigurationAuthorityReadV1::Absent,
                DoctorEvidenceStateV1::Absent,
            ),
            (
                ConfigurationAuthorityReadV1::Denied,
                DoctorEvidenceStateV1::Denied,
            ),
            (
                ConfigurationAuthorityReadV1::Unknown,
                DoctorEvidenceStateV1::Unknown,
            ),
        ] {
            let finding = configuration_finding(&read).expect("finding");
            assert_eq!(finding.state(), expected);
            assert!(finding.remediation().is_none());
            assert!(!finding.state().is_healthy_complete());
        }
    }

    #[test]
    fn runtime_stuck_is_degraded_with_recover_remediation() {
        let finding = runtime_health_finding(&RuntimeHealthReadV1::Observed {
            liveness: RuntimeLivenessV1::Stuck,
            coverage: DoctorCoverageCompletenessV1::Complete,
        })
        .expect("finding");
        assert_eq!(finding.state(), DoctorEvidenceStateV1::Degraded);
        assert_eq!(
            finding
                .remediation()
                .expect("remediation")
                .owning_operation()
                .as_str(),
            operations::RUNTIME_RECOVER_DAEMON
        );
    }

    #[test]
    fn runtime_healthy_partial_coverage_is_not_healthy() {
        let finding = runtime_health_finding(&RuntimeHealthReadV1::Observed {
            liveness: RuntimeLivenessV1::Healthy,
            coverage: DoctorCoverageCompletenessV1::Partial,
        })
        .expect("finding");
        assert!(!finding.state().is_healthy_complete());
        assert_eq!(finding.state(), DoctorEvidenceStateV1::Partial);
    }

    #[test]
    fn host_drift_maps_to_advisory_family_with_repair() {
        let finding = host_integration_finding(&HostIntegrationReadV1::Observed {
            conformance: HostConformanceV1::ProtocolDrift,
            coverage: DoctorCoverageCompletenessV1::Complete,
        })
        .expect("finding");
        assert_eq!(finding.family(), DoctorFindingFamilyV1::Advisory);
        assert_eq!(finding.state(), DoctorEvidenceStateV1::Degraded);
        assert_eq!(
            finding
                .remediation()
                .expect("remediation")
                .owning_operation()
                .as_str(),
            operations::HOST_REPAIR_INTEGRATION
        );
    }

    #[test]
    fn code_index_indexing_is_partial_without_remediation() {
        let finding = code_index_finding(&CodeIndexMountReadV1::Observed {
            state: CodeIndexMountStateV1::Indexing,
            coverage: DoctorCoverageCompletenessV1::Complete,
        })
        .expect("finding");
        assert_eq!(finding.state(), DoctorEvidenceStateV1::Partial);
        assert!(finding.remediation().is_none());
    }

    #[test]
    fn code_index_stale_is_stale_with_remount() {
        let finding = code_index_finding(&CodeIndexMountReadV1::Observed {
            state: CodeIndexMountStateV1::Stale,
            coverage: DoctorCoverageCompletenessV1::Complete,
        })
        .expect("finding");
        assert_eq!(finding.state(), DoctorEvidenceStateV1::Stale);
        assert_eq!(
            finding
                .remediation()
                .expect("remediation")
                .owning_operation()
                .as_str(),
            operations::CODE_INDEX_REMOUNT
        );
    }

    #[test]
    fn language_server_refreshing_is_partial() {
        let finding = language_server_finding(&LanguageServerReadV1::Observed {
            state: LanguageServerStateV1::Refreshing,
            coverage: DoctorCoverageCompletenessV1::Complete,
        })
        .expect("finding");
        assert_eq!(finding.family(), DoctorFindingFamilyV1::LanguageServer);
        assert_eq!(finding.state(), DoctorEvidenceStateV1::Partial);
        assert!(finding.remediation().is_none());
    }

    #[test]
    fn observability_partial_projection_cannot_claim_health() {
        let finding = observability_finding(&ObservabilityReadV1::Observed {
            state: ObservabilityStateV1::Current,
            total_count: 17,
            last_observed_at_micros: Some(42),
            coverage: DoctorCoverageCompletenessV1::Partial,
        })
        .expect("finding");
        assert_eq!(finding.family(), DoctorFindingFamilyV1::Observability);
        assert_eq!(finding.state(), DoctorEvidenceStateV1::Partial);
        assert!(finding.remediation().is_none());
    }
}

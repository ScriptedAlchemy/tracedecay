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
//! - code/semantic index mount state → [`DoctorFindingFamilyV1::SemanticIndex`]
//! - storage retention/size → [`DoctorFindingFamilyV1::Storage`] (producers in
//!   [`crate::storage::findings`]; this port collects their typed findings)

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

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
}

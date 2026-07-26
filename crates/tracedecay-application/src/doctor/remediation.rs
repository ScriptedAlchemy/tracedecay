//! Doctor remediation-reference registry (Plan 09 §PR14, Plan 11 recovery
//! journey).
//!
//! Doctor never repairs. A [`DoctorFindingV1`](crate::doctor::DoctorFindingV1)
//! carries only a *reference* to an owning application operation. This module is
//! the kernel-side registry that resolves such a reference into a typed
//! [`DoctorRemediationDescriptorV1`] — what the operation would do, which surface
//! owns its dispatch, whether a non-mutating preview exists, and whether an
//! explicit confirmation is required — **without executing anything**. Execution
//! dispatch stays entirely with the owning surface; the kernel only describes a
//! reference and rejects an unknown or inconsistent one with a typed error.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::ApplicationContractError;

use super::types::{DoctorOwningOperationRefV1, DoctorRemediationKindV1, DoctorRemediationRefV1};

/// Canonical owning-operation reference strings.
///
/// These are the single source of truth for the owning-operation identity used
/// by both the Doctor finding producers (which emit
/// [`DoctorRemediationRefV1`](crate::doctor::DoctorRemediationRefV1) values) and
/// [`DoctorRemediationRegistryV1::default_registry`] (which resolves them). A
/// producer and the registry must agree, or a resolvable finding could reference
/// an operation the registry cannot describe.
pub mod operations {
    /// Retention/size collection over an over-budget store or retention backlog.
    pub const STORAGE_RETENTION_COLLECT: &str = "use-case.application.storage.retention-collect";
    /// Collection of an identity-drift orphan store.
    pub const STORAGE_COLLECT_ORPHAN_STORE: &str =
        "use-case.application.storage.collect-orphan-store";
    /// Lifecycle removal of branch-scoped databases whose git refs are gone.
    pub const STORAGE_BRANCH_GC: &str = "use-case.application.storage.branch-gc";
    /// Quarantine and collection of incident/corruption debris beside a store.
    pub const STORAGE_QUARANTINE_AND_COLLECT_DEBRIS: &str =
        "use-case.application.storage.quarantine-and-collect-debris";
    /// Protected apply of desired configuration to reconcile effective drift.
    pub const CONFIGURATION_PROTECTED_APPLY: &str =
        "use-case.application.configuration.protected-apply";
    /// Re-establish a requested configuration pin the authority could not honor.
    pub const CONFIGURATION_PIN_AUTHORITY: &str =
        "use-case.application.configuration.pin-authority";
    /// Recover a stuck, degraded, or unreachable daemon/runtime.
    pub const RUNTIME_RECOVER_DAEMON: &str = "use-case.application.runtime.recover-daemon";
    /// Repair a host/agent integration whose conformance has drifted.
    pub const HOST_REPAIR_INTEGRATION: &str = "use-case.application.host.repair-integration";
    /// Remount or rebuild a code/semantic index that is unmounted or stale.
    pub const CODE_INDEX_REMOUNT: &str = "use-case.application.code-index.remount";
}

/// The application surface that owns dispatch of a remediation operation.
///
/// The kernel names the owner so a transport can route confirmation and dispatch
/// to the correct surface; it never dispatches itself.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum DoctorOwningSurfaceV1 {
    /// The configuration control plane (Plan 20).
    ConfigurationControlPlane,
    /// The storage retention/lifecycle runtime (Plan 38).
    StorageRuntime,
    /// The daemon/runtime supervisor.
    DaemonRuntime,
    /// The host/agent integration installer/repairer (Plan 27).
    HostIntegration,
    /// The semantic/code index runtime (Plan 35 / semantic index).
    SemanticIndexRuntime,
}

/// Whether an owning operation requires an explicit human confirmation before it
/// may perform its admitted mutating effect.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum DoctorConfirmationRequirementV1 {
    /// An explicit confirmation is required before the action may run.
    Required,
    /// No confirmation is required (still dispatched through the owner).
    NotRequired,
}

/// A typed description of what an owner-supplied remediation operation would do.
///
/// This is a *description*, not an effect. It never carries argv, a path, or an
/// inline action, and resolving it performs no work.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DoctorRemediationDescriptorV1 {
    operation: DoctorOwningOperationRefV1,
    surface: DoctorOwningSurfaceV1,
    preview_available: bool,
    action_confirmation: DoctorConfirmationRequirementV1,
    summary: String,
}

impl DoctorRemediationDescriptorV1 {
    /// Validate and construct a remediation descriptor. The `summary` must be
    /// non-empty, trimmed, bounded, and free of control characters — it is a
    /// human-readable statement of what the operation would do, never an effect.
    pub fn new(
        operation: DoctorOwningOperationRefV1,
        surface: DoctorOwningSurfaceV1,
        preview_available: bool,
        action_confirmation: DoctorConfirmationRequirementV1,
        summary: impl Into<String>,
    ) -> Result<Self, ApplicationContractError> {
        let summary = summary.into();
        if summary.is_empty()
            || summary.trim() != summary
            || summary.len() > 256
            || summary.chars().any(char::is_control)
        {
            return Err(ApplicationContractError::InvalidIdentifier {
                field: "doctor remediation summary",
            });
        }
        Ok(Self {
            operation,
            surface,
            preview_available,
            action_confirmation,
            summary,
        })
    }

    /// The owning-operation reference this descriptor describes.
    #[must_use]
    pub fn operation(&self) -> &DoctorOwningOperationRefV1 {
        &self.operation
    }

    /// The application surface that owns dispatch of the operation.
    #[must_use]
    pub fn surface(&self) -> DoctorOwningSurfaceV1 {
        self.surface
    }

    /// Whether the operation offers a non-mutating preview (dry run).
    #[must_use]
    pub fn preview_available(&self) -> bool {
        self.preview_available
    }

    /// Whether the operation's mutating action requires explicit confirmation.
    #[must_use]
    pub fn action_confirmation(&self) -> DoctorConfirmationRequirementV1 {
        self.action_confirmation
    }

    /// Human-readable statement of what the operation would do.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// Typed rejection when a remediation reference cannot be resolved.
///
/// Resolution is pure: it never dispatches. An unrecognised operation, or a
/// preview requested for an operation that offers none, is a typed rejection —
/// never a silent success or a fabricated descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DoctorRemediationResolutionErrorV1 {
    /// No descriptor is registered for the referenced owning operation.
    UnknownOperation { operation: String },
    /// The reference requested a preview, but the operation offers none.
    PreviewUnavailable {
        operation: String,
        requested: DoctorRemediationKindV1,
    },
}

impl fmt::Display for DoctorRemediationResolutionErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOperation { operation } => write!(
                formatter,
                "no remediation descriptor is registered for operation {operation}"
            ),
            Self::PreviewUnavailable {
                operation,
                requested,
            } => write!(
                formatter,
                "operation {operation} offers no preview for requested kind {requested:?}"
            ),
        }
    }
}

impl std::error::Error for DoctorRemediationResolutionErrorV1 {}

/// A registry that resolves a remediation reference to a typed descriptor.
///
/// The registry owns no dispatch and holds no effect. It maps an owning-operation
/// reference to its [`DoctorRemediationDescriptorV1`] and validates a
/// [`DoctorRemediationRefV1`] against it. Operation references are unique within
/// a registry; a duplicate is a contract error rather than a silently shadowed
/// entry.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DoctorRemediationRegistryV1 {
    descriptors: Vec<DoctorRemediationDescriptorV1>,
}

impl DoctorRemediationRegistryV1 {
    /// Validate and construct a registry from descriptors. Operation references
    /// must be unique; a duplicate operation is a contract error.
    pub fn new(
        descriptors: Vec<DoctorRemediationDescriptorV1>,
    ) -> Result<Self, ApplicationContractError> {
        if descriptors.iter().enumerate().any(|(index, current)| {
            descriptors[index.saturating_add(1)..]
                .iter()
                .any(|other| other.operation() == current.operation())
        }) {
            return Err(ApplicationContractError::Duplicate {
                field: "doctor remediation operation",
            });
        }
        Ok(Self { descriptors })
    }

    /// Resolve a remediation reference to its descriptor without executing.
    ///
    /// An unregistered operation is [`DoctorRemediationResolutionErrorV1::UnknownOperation`];
    /// a preview requested for an operation that offers none is
    /// [`DoctorRemediationResolutionErrorV1::PreviewUnavailable`].
    pub fn resolve(
        &self,
        reference: &DoctorRemediationRefV1,
    ) -> Result<&DoctorRemediationDescriptorV1, DoctorRemediationResolutionErrorV1> {
        let descriptor = self
            .descriptors
            .iter()
            .find(|descriptor| descriptor.operation() == reference.owning_operation())
            .ok_or_else(|| DoctorRemediationResolutionErrorV1::UnknownOperation {
                operation: reference.owning_operation().as_str().to_string(),
            })?;
        if reference.kind() == DoctorRemediationKindV1::Preview && !descriptor.preview_available() {
            return Err(DoctorRemediationResolutionErrorV1::PreviewUnavailable {
                operation: reference.owning_operation().as_str().to_string(),
                requested: reference.kind(),
            });
        }
        Ok(descriptor)
    }

    /// The registered descriptors, in registration order.
    #[must_use]
    pub fn descriptors(&self) -> &[DoctorRemediationDescriptorV1] {
        &self.descriptors
    }

    /// The default registry seeded with every owning operation the shipped Doctor
    /// finding producers reference. Every remediation reference a landed producer
    /// emits resolves against this registry.
    #[must_use]
    pub fn default_registry() -> Self {
        // Each entry pairs a canonical operation string with its owning surface,
        // preview-ability, and confirmation requirement. Repair operations are
        // mutating and confirmed.
        let seed: &[(
            &str,
            DoctorOwningSurfaceV1,
            bool,
            DoctorConfirmationRequirementV1,
            &str,
        )] = &[
            (
                operations::STORAGE_RETENTION_COLLECT,
                DoctorOwningSurfaceV1::StorageRuntime,
                true,
                DoctorConfirmationRequirementV1::Required,
                "collect retention-eligible rows or reclaim an over-budget store",
            ),
            (
                operations::STORAGE_COLLECT_ORPHAN_STORE,
                DoctorOwningSurfaceV1::StorageRuntime,
                true,
                DoctorConfirmationRequirementV1::Required,
                "collect a store whose project identity no longer resolves",
            ),
            (
                operations::STORAGE_BRANCH_GC,
                DoctorOwningSurfaceV1::StorageRuntime,
                true,
                DoctorConfirmationRequirementV1::Required,
                "remove branch-scoped databases whose git refs are gone",
            ),
            (
                operations::STORAGE_QUARANTINE_AND_COLLECT_DEBRIS,
                DoctorOwningSurfaceV1::StorageRuntime,
                true,
                DoctorConfirmationRequirementV1::Required,
                "quarantine and collect incident debris beside a live store",
            ),
            (
                operations::CONFIGURATION_PROTECTED_APPLY,
                DoctorOwningSurfaceV1::ConfigurationControlPlane,
                true,
                DoctorConfirmationRequirementV1::Required,
                "apply desired configuration to reconcile effective drift",
            ),
            (
                operations::CONFIGURATION_PIN_AUTHORITY,
                DoctorOwningSurfaceV1::ConfigurationControlPlane,
                true,
                DoctorConfirmationRequirementV1::Required,
                "re-establish a requested configuration pin the authority could not honor",
            ),
            (
                operations::RUNTIME_RECOVER_DAEMON,
                DoctorOwningSurfaceV1::DaemonRuntime,
                false,
                DoctorConfirmationRequirementV1::Required,
                "recover a stuck, degraded, or unreachable daemon runtime",
            ),
            (
                operations::HOST_REPAIR_INTEGRATION,
                DoctorOwningSurfaceV1::HostIntegration,
                true,
                DoctorConfirmationRequirementV1::Required,
                "repair a host/agent integration whose conformance has drifted",
            ),
            (
                operations::CODE_INDEX_REMOUNT,
                DoctorOwningSurfaceV1::SemanticIndexRuntime,
                true,
                DoctorConfirmationRequirementV1::Required,
                "remount or rebuild a code/semantic index that is unmounted or stale",
            ),
        ];
        let descriptors = seed
            .iter()
            .map(|(operation, surface, preview, confirmation, summary)| {
                DoctorRemediationDescriptorV1::new(
                    DoctorOwningOperationRefV1::new(*operation)
                        .expect("seed operation reference is valid"),
                    *surface,
                    *preview,
                    *confirmation,
                    *summary,
                )
                .expect("seed descriptor is valid")
            })
            .collect();
        // Seed operations are unique by construction; `new` re-checks and cannot
        // fail here.
        Self::new(descriptors).expect("seed registry has unique operations")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(operation: &str, kind: DoctorRemediationKindV1) -> DoctorRemediationRefV1 {
        DoctorRemediationRefV1::new(
            DoctorOwningOperationRefV1::new(operation).expect("valid operation"),
            kind,
        )
    }

    #[test]
    fn doctor_default_registry_resolves_every_storage_operation() {
        let registry = DoctorRemediationRegistryV1::default_registry();
        for operation in [
            operations::STORAGE_RETENTION_COLLECT,
            operations::STORAGE_COLLECT_ORPHAN_STORE,
            operations::STORAGE_BRANCH_GC,
            operations::STORAGE_QUARANTINE_AND_COLLECT_DEBRIS,
        ] {
            let descriptor = registry
                .resolve(&reference(operation, DoctorRemediationKindV1::Action))
                .expect("storage operation resolves");
            assert_eq!(descriptor.surface(), DoctorOwningSurfaceV1::StorageRuntime);
            assert_eq!(
                descriptor.action_confirmation(),
                DoctorConfirmationRequirementV1::Required
            );
        }
    }

    #[test]
    fn doctor_default_registry_contains_only_dispatchable_targets() {
        let registry = DoctorRemediationRegistryV1::default_registry();
        assert_eq!(registry.descriptors().len(), 9);
        assert!(
            registry
                .descriptors()
                .iter()
                .all(|descriptor| descriptor.operation().as_str()
                    != crate::feedback::FEEDBACK_GET_USE_CASE_ID_V1),
            "feedback reads are navigation, not remediation effects"
        );
    }

    #[test]
    fn doctor_registry_rejects_unknown_operation() {
        let registry = DoctorRemediationRegistryV1::default_registry();
        let error = registry
            .resolve(&reference(
                "use-case.application.storage.unknown-op",
                DoctorRemediationKindV1::Action,
            ))
            .expect_err("unknown operation rejected");
        assert_eq!(
            error,
            DoctorRemediationResolutionErrorV1::UnknownOperation {
                operation: "use-case.application.storage.unknown-op".to_string(),
            }
        );
    }

    #[test]
    fn doctor_registry_rejects_preview_when_operation_offers_none() {
        let registry = DoctorRemediationRegistryV1::default_registry();
        let error = registry
            .resolve(&reference(
                operations::RUNTIME_RECOVER_DAEMON,
                DoctorRemediationKindV1::Preview,
            ))
            .expect_err("preview unavailable rejected");
        assert_eq!(
            error,
            DoctorRemediationResolutionErrorV1::PreviewUnavailable {
                operation: operations::RUNTIME_RECOVER_DAEMON.to_string(),
                requested: DoctorRemediationKindV1::Preview,
            }
        );
        // The same operation still resolves as an action.
        assert!(
            registry
                .resolve(&reference(
                    operations::RUNTIME_RECOVER_DAEMON,
                    DoctorRemediationKindV1::Action,
                ))
                .is_ok()
        );
    }

    #[test]
    fn doctor_registry_rejects_duplicate_operation() {
        let descriptor = DoctorRemediationDescriptorV1::new(
            DoctorOwningOperationRefV1::new(operations::STORAGE_BRANCH_GC).expect("valid"),
            DoctorOwningSurfaceV1::StorageRuntime,
            true,
            DoctorConfirmationRequirementV1::Required,
            "branch gc",
        )
        .expect("descriptor");
        let error = DoctorRemediationRegistryV1::new(vec![descriptor.clone(), descriptor])
            .expect_err("duplicate operation rejected");
        assert_eq!(
            error,
            ApplicationContractError::Duplicate {
                field: "doctor remediation operation"
            }
        );
    }

    #[test]
    fn doctor_remediation_descriptor_rejects_empty_summary() {
        let error = DoctorRemediationDescriptorV1::new(
            DoctorOwningOperationRefV1::new(operations::CODE_INDEX_REMOUNT).expect("valid"),
            DoctorOwningSurfaceV1::SemanticIndexRuntime,
            true,
            DoctorConfirmationRequirementV1::Required,
            "",
        )
        .expect_err("empty summary rejected");
        assert_eq!(
            error,
            ApplicationContractError::InvalidIdentifier {
                field: "doctor remediation summary"
            }
        );
    }

    #[test]
    fn doctor_remediation_descriptor_serializes_surface_snake_case() {
        assert_eq!(
            serde_json::to_string(&DoctorOwningSurfaceV1::ConfigurationControlPlane)
                .expect("serialize"),
            "\"configuration_control_plane\""
        );
    }
}

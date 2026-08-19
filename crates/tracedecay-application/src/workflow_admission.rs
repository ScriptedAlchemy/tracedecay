//! Tool-catalog semantic admission for workflow definitions.
//!
//! Structural validation ([`tracedecay_domain::WorkflowDefinition::validate`])
//! proves the DAG shape; it says nothing about whether a step's operation is
//! real. Plan 32 requires that "unknown operations, cycles, dangling
//! references, incompatible schemas, unbounded fan-out, privilege expansion,
//! unsupported effects, or recursive generic execution reject before
//! activation", so activation additionally admits every step operation
//! against the canonical Work executable catalog — the registry whose
//! operations workflow fan-out actually lowers steps into
//! ([`crate::prepare_workflow_fan_out`] copies `step.operation` onto the
//! durable plan, and the daemon starts the child Work attempts under it).
//!
//! The schema and capability halves of the check are carried by the catalog
//! digest pin: [`crate::work_executable_catalog_digest`] hashes the complete
//! registry, including every operation's capability manifest and request and
//! result schema authorities, so a definition whose `pinned_catalog_digest`
//! names the live digest was authored against exactly the schemas and
//! capability contracts this build executes. A stale pin is a typed denial,
//! never a silent re-pin.

use std::fmt::{self, Display};

use tracedecay_domain::{ManifestDigest, WorkflowDefinition, WorkflowOperationRef, WorkflowStepId};
use tracedecay_tool_catalog::{CatalogValidationError, OperationId};

use crate::work_catalog::{work_executable_binding_registry, work_executable_catalog_digest};

/// Typed denial produced by workflow catalog admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowCatalogAdmissionError {
    /// The step names an operation the executable catalog does not know.
    UnknownOperation {
        step_id: WorkflowStepId,
        operation: WorkflowOperationRef,
    },
    /// The operation is cataloged but currently has no executable binding.
    OperationUnavailable {
        step_id: WorkflowStepId,
        operation: WorkflowOperationRef,
    },
    /// The definition pins a catalog other than the live executable catalog,
    /// so its operations were authored against different schemas or
    /// capability contracts.
    CatalogPinMismatch {
        pinned: ManifestDigest,
        current: ManifestDigest,
    },
    /// The canonical catalog itself could not be composed.
    CatalogUnavailable(CatalogValidationError),
}

impl Display for WorkflowCatalogAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOperation { step_id, operation } => write!(
                formatter,
                "workflow step {step_id} names unknown catalog operation {operation}"
            ),
            Self::OperationUnavailable { step_id, operation } => write!(
                formatter,
                "workflow step {step_id} names catalog operation {operation} without an executable binding"
            ),
            Self::CatalogPinMismatch { pinned, current } => write!(
                formatter,
                "workflow definition pins catalog {pinned} but the live executable catalog is {current}"
            ),
            Self::CatalogUnavailable(error) => {
                write!(formatter, "workflow executable catalog unavailable: {error}")
            }
        }
    }
}

impl std::error::Error for WorkflowCatalogAdmissionError {}

/// Admit every step operation of one workflow definition against the
/// canonical Work executable catalog.
///
/// Admission holds exactly when the definition pins the live executable
/// catalog digest and every step operation resolves to an available
/// executable binding in that catalog. The first violation is returned as a
/// typed denial naming the offending step and operation.
pub(crate) fn admit_workflow_definition_operations(
    definition: &WorkflowDefinition,
) -> Result<(), WorkflowCatalogAdmissionError> {
    let registry = work_executable_binding_registry()
        .map_err(WorkflowCatalogAdmissionError::CatalogUnavailable)?;
    let current = work_executable_catalog_digest()
        .map_err(WorkflowCatalogAdmissionError::CatalogUnavailable)?;
    if definition.pinned_catalog_digest() != &current {
        return Err(WorkflowCatalogAdmissionError::CatalogPinMismatch {
            pinned: definition.pinned_catalog_digest().clone(),
            current,
        });
    }
    for step in definition.steps() {
        let Ok(operation_id) = OperationId::new(step.operation.as_str().to_owned()) else {
            return Err(WorkflowCatalogAdmissionError::UnknownOperation {
                step_id: step.step_id.clone(),
                operation: step.operation.clone(),
            });
        };
        let Some(availability) = registry.get(&operation_id) else {
            return Err(WorkflowCatalogAdmissionError::UnknownOperation {
                step_id: step.step_id.clone(),
                operation: step.operation.clone(),
            });
        };
        if availability.binding().is_none() {
            return Err(WorkflowCatalogAdmissionError::OperationUnavailable {
                step_id: step.step_id.clone(),
                operation: step.operation.clone(),
            });
        }
    }
    Ok(())
}

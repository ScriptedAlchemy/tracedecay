//! The resident-memory charge of one interactive catalog build.

use std::num::NonZeroU64;
use std::sync::Arc;

use tracedecay_graph_db::{GraphBudgetKind, GraphDbError};
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, ProcessSharedMemoryReservationV1, ResidentMemoryAdmissionFailureV1,
    ResidentMemoryComponentIdV1, ResidentMemoryOperationV1,
};

use super::super::CodeGraphProjectionError;

/// The component a catalog build's reservation reports under.
const CATALOG_BUILD_RESIDENT_COMPONENT_V1: &str = "code_graph.catalog.build.v1";

/// Reserves what one catalog build holds as it grows, the way cold
/// construction charges `code_index.construction.v1`: concurrent admissions
/// see the build, and a build that would grow past the process limit stops
/// with a typed refusal instead. Dropping the charge returns its bytes; the
/// published catalog is then measured as the `graph_catalog` owner.
pub(super) struct CatalogBuildChargeV1 {
    operation: Option<ResidentMemoryOperationV1>,
    charge: Option<ProcessSharedMemoryReservationV1>,
}

impl CatalogBuildChargeV1 {
    pub(super) fn cover_read(&mut self, bytes: usize) -> Result<(), GraphDbError> {
        self.cover(bytes).map_err(|error| match error {
            CodeGraphProjectionError::BudgetExhausted { limit, .. } => {
                GraphDbError::budget_exhausted(GraphBudgetKind::ResidentMemory, limit)
            }
            error => GraphDbError::unavailable(error.to_string()),
        })
    }

    /// A store bound to no authority charges nothing.
    pub(super) fn new(memory: Option<&Arc<ProcessResidentMemoryV1>>) -> Self {
        Self {
            operation: memory.map(ProcessResidentMemoryV1::begin_operation),
            charge: None,
        }
    }

    /// Grow the reservation to `bytes`. It never shrinks: a build's peak is
    /// what it must be admitted for.
    pub(super) fn cover(&mut self, bytes: usize) -> Result<(), CodeGraphProjectionError> {
        let Some(operation) = &self.operation else {
            return Ok(());
        };
        let held = self
            .charge
            .as_ref()
            .map_or(0, ProcessSharedMemoryReservationV1::reserved_bytes);
        let Some(additional) = NonZeroU64::new(
            u64::try_from(bytes)
                .unwrap_or(u64::MAX)
                .saturating_sub(held),
        ) else {
            return Ok(());
        };
        let refused = |error: ResidentMemoryAdmissionFailureV1| {
            tracing::info!(
                event = "code_graph_catalog_build_refused",
                charged_bytes = held,
                attempted_bytes = bytes,
                %error,
                "the interactive catalog build was refused resident memory"
            );
            CodeGraphProjectionError::BudgetExhausted {
                budget: GraphBudgetKind::ResidentMemory.as_str().to_owned(),
                limit: operation.authority().limit_bytes(),
            }
        };
        match self.charge.as_mut() {
            Some(charge) => charge.try_grow(additional).map_err(refused),
            None => {
                let component =
                    ResidentMemoryComponentIdV1::new(CATALOG_BUILD_RESIDENT_COMPONENT_V1)
                        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
                self.charge = Some(
                    operation
                        .reserve_process_shared(component, additional)
                        .map_err(refused)?,
                );
                Ok(())
            }
        }
    }
}

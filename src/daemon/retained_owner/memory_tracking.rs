//! Canonical retrieval telemetry for explicit retained-memory search.

use tracedecay_application::{RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1};
use tracedecay_domain::{FactOwnerV1, ManifestDigest, ProvenanceId};
use tracedecay_store::{
    ProjectMemoryFactIdV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactRetrievalCommandV1,
    ProjectMemoryFactRetrievalReceiptV1, ProjectMemoryFactSearchPageV1,
};
use tracedecay_usecases::memory::MemoryApplication;

use super::memory::fact_write_control;
use super::memory_mapping;
use super::memory_mutation::{MemoryMutationSettlement, memory_mutation_settlement};
use super::memory_stage::bounded_memory_operation;
use crate::store::DatabaseFactStore;

#[derive(Default)]
pub(super) struct TrackedExplicitSearch {
    pub(super) projections: Vec<ProjectMemoryFactProjectionV1>,
    pub(super) receipt: Option<ProjectMemoryFactRetrievalReceiptV1>,
    pub(super) authority_result_invalid: bool,
    pub(super) settled_after_expiry: bool,
}

impl TrackedExplicitSearch {
    pub(super) fn committed_state(&self) -> Option<&ManifestDigest> {
        self.receipt
            .as_ref()
            .map(ProjectMemoryFactRetrievalReceiptV1::committed_state_digest)
    }
}

pub(super) async fn track_explicit_search(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    memory: &MemoryApplication<DatabaseFactStore<'_>>,
    owner: &FactOwnerV1,
    operation_id: ProvenanceId,
    page: &ProjectMemoryFactSearchPageV1,
) -> Result<TrackedExplicitSearch, RetainedSurfaceExecutionErrorV1> {
    if page.hits().is_empty() {
        return Ok(TrackedExplicitSearch::default());
    }
    let targets = page
        .hits()
        .iter()
        .map(|hit| ProjectMemoryFactIdV1::new(owner.clone(), hit.fact().fact_id().clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(memory_mapping::map_store_error)?;
    let command =
        ProjectMemoryFactRetrievalCommandV1::new(owner.clone(), operation_id, targets, true)
            .map_err(memory_mapping::map_store_error)?;
    let write_control = fact_write_control(context);
    let (outcome, settled_after_expiry) =
        bounded_memory_operation(context, memory_mapping::EFFECT_CANCELLATION_STAGES, async {
            Ok(memory
                .record_project_memory_fact_retrieval(command, &write_control)
                .await)
        })
        .await?;
    let (outcome, authority_result_invalid) = match memory_mutation_settlement(outcome)? {
        MemoryMutationSettlement::Validated(outcome) => (outcome, false),
        MemoryMutationSettlement::InvalidAuthority(outcome) => (outcome, true),
    };
    Ok(TrackedExplicitSearch {
        projections: outcome.projections().to_vec(),
        receipt: Some(outcome.receipt().clone()),
        authority_result_invalid,
        settled_after_expiry,
    })
}

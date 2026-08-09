//! TaskId-rooted Work evidence composition over the owning stores.
//!
//! The Work graph and sealed attempts share the registered exact-SQL store.
//! Provider narratives and anchors use the project-open mounted session
//! retrieval authority, preserving its temporal, redaction, and direct-anchor
//! semantics rather than reopening a store here.

use tracedecay_application::{
    ApplicationProblem, RequestContext, WorkEvidenceRetrievalServiceV1, WorkEvidenceRetrievalV1,
    WorkEvidenceRetrieveRequestV1,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::{RegisteredWorkRuntime, work_product_problem};

pub(super) async fn retrieve(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    capability: &str,
    use_case: UseCaseId,
    request: WorkEvidenceRetrieveRequestV1,
) -> Result<WorkEvidenceRetrievalV1, ApplicationProblem> {
    let capability = CapabilityId::new(capability).map_err(|_| {
        work_product_problem(
            tracedecay_application::WorkProductApplicationErrorV1::GraphAuthorityUnavailable,
        )
    })?;
    let binding = tracedecay_application::WorkProductBindingV1::new(capability, use_case);
    let storage = registered.database.work_storage().map_err(|_| {
        work_product_problem(
            tracedecay_application::WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable,
        )
    })?;
    let evidence_retrieval = registered.evidence_retrieval.clone();
    WorkEvidenceRetrievalServiceV1::new(
        storage.clone(),
        storage.clone(),
        storage,
        evidence_retrieval.clone(),
        evidence_retrieval,
        binding,
    )
    .retrieve(context, request)
    .await
    .map_err(work_product_problem)
}

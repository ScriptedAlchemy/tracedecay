//! Canonical executable bindings for the Plan 24 product operations.
//!
//! The root daemon composes this registry with the existing Work attempt
//! registry when it mounts the product router. Keeping the contribution
//! separate prevents an advertised route from preceding its daemon mount.

use tracedecay_domain::{WorkProposalV1, WorkTaskEvidenceV1};
use tracedecay_tool_catalog::{
    CatalogValidationError, EffectClass, ExecutableBindingAvailabilityV1,
    ExecutableBindingRegistryV1,
};

use crate::{
    ExpandWorkEvidenceRequestV1, GenerateWorkProposalRequestV1, WorkEvidenceExpansionV1,
    WorkProductMutationReceiptV1, WorkProductMutationRequestV1, WorkProductProjectionReadV1,
    WorkProductProjectionsRequestV1, WorkProductSnapshotRequestV1, WorkTaskEvidenceRequestV1,
    WorkTopologyReadV1,
};

pub const WORK_PRODUCT_OPERATION_IDS_V1: [(&str, &str, &str); 6] = [
    (
        "product_snapshot",
        "capability.work.product_snapshot",
        "use-case.work.product_snapshot",
    ),
    (
        "product_projections",
        "capability.work.product_projections",
        "use-case.work.product_projections",
    ),
    (
        "task_evidence",
        "capability.work.task_evidence",
        "use-case.work.task_evidence",
    ),
    (
        "expand_task_evidence",
        "capability.work.expand_task_evidence",
        "use-case.work.expand_task_evidence",
    ),
    (
        "generate_work_proposal",
        "capability.work.generate_work_proposal",
        "use-case.work.generate_work_proposal",
    ),
    (
        "apply_work_command",
        "capability.work.apply_work_command",
        "use-case.work.apply_work_command",
    ),
];

pub fn work_product_executable_binding_registry()
-> Result<ExecutableBindingRegistryV1, CatalogValidationError> {
    ExecutableBindingRegistryV1::new(vec![
        product_binding::<WorkProductSnapshotRequestV1, WorkTopologyReadV1>(
            "product_snapshot",
            "/application/work/product/snapshot",
            EffectClass::Read,
        )?,
        product_binding::<WorkProductProjectionsRequestV1, WorkProductProjectionReadV1>(
            "product_projections",
            "/application/work/product/projections",
            EffectClass::Read,
        )?,
        product_binding::<WorkTaskEvidenceRequestV1, WorkTaskEvidenceV1>(
            "task_evidence",
            "/application/work/product/task-evidence",
            EffectClass::Read,
        )?,
        product_binding::<ExpandWorkEvidenceRequestV1, WorkEvidenceExpansionV1>(
            "expand_task_evidence",
            "/application/work/product/expand-task-evidence",
            EffectClass::Read,
        )?,
        product_binding::<GenerateWorkProposalRequestV1, WorkProposalV1>(
            "generate_work_proposal",
            "/application/work/product/generate-proposal",
            EffectClass::Read,
        )?,
        product_binding::<WorkProductMutationRequestV1, WorkProductMutationReceiptV1>(
            "apply_work_command",
            "/application/work/product/apply-command",
            EffectClass::Administrative,
        )?,
    ])
}

fn product_binding<Request, Output>(
    operation: &str,
    route_path: &str,
    effect: EffectClass,
) -> Result<ExecutableBindingAvailabilityV1, CatalogValidationError>
where
    Request: schemars::JsonSchema,
    Output: schemars::JsonSchema,
{
    super::work_catalog::available::<Request, Output>(operation, route_path, effect)
}

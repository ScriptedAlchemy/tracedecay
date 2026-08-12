mod effects;
mod fact_commands;
mod merge;
mod mutations;
mod operations;
mod receipt;
mod validate;

const MAX_PROJECT_MEMORY_CURATION_OPERATIONS: usize = 256;

pub(super) const MAX_PROJECT_MEMORY_CURATION_TARGETS: usize = 256;

pub use effects::{
    ProjectMemoryFactCurationLinkDispositionV1, ProjectMemoryFactCurationLinkEffectV1,
    ProjectMemoryFactCurationOperationEffectV1, ProjectMemoryFactCurationRemoveDispositionV1,
};
pub use fact_commands::{
    ProjectMemoryFactAddCommandV1, ProjectMemoryFactAddDispositionV1,
    ProjectMemoryFactAddMaterialV1, ProjectMemoryFactAddOutcomeV1,
    ProjectMemoryFactFeedbackCommandV1, ProjectMemoryFactFeedbackOutcomeV1,
    ProjectMemoryFactRemoveCommandV1, ProjectMemoryFactRemoveOutcomeV1,
    ProjectMemoryFactUpdateCommandV1, ProjectMemoryFactUpdateOutcomeV1,
    ProjectMemoryFactUpdatePatchV1,
};
pub use merge::{
    ProjectMemoryFactMergeCommandV1, ProjectMemoryFactMergeOutcomeV1,
    ProjectMemoryFactMergeTargetV1,
};
pub use mutations::{
    ProjectMemoryFactCurationAddV1, ProjectMemoryFactCurationEvidenceV1,
    ProjectMemoryFactCurationMergeV1, ProjectMemoryFactCurationRemoveV1,
    ProjectMemoryFactCurationUpdateV1,
};
pub use operations::{
    ProjectMemoryEntityIdV1, ProjectMemoryFactCurationBatchV1,
    ProjectMemoryFactCurationMutationKindV1, ProjectMemoryFactCurationOperationV1,
    ProjectMemoryFactCurationReviewRefV1, ProjectMemoryFactLinkV1,
    ProjectMemoryFactNormalizeTagsV1, derive_project_memory_fact_curation_child_operation_id,
};
pub use receipt::ProjectMemoryFactCurationReceiptV1;

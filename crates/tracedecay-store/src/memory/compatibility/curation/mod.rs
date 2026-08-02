mod fact_commands;
mod merge;
mod operations;
mod validate;

const MAX_COMPATIBILITY_CURATION_OPERATIONS: usize = 256;

pub(super) const MAX_COMPATIBILITY_CURATION_TARGETS: usize = 256;

pub use fact_commands::{
    CompatibilityFactAddCommandV1, CompatibilityFactAddDispositionV1,
    CompatibilityFactAddOutcomeV1, CompatibilityFactFeedbackCommandV1,
    CompatibilityFactFeedbackOutcomeV1, CompatibilityFactRemoveCommandV1,
    CompatibilityFactRemoveOutcomeV1, CompatibilityFactUpdateCommandV1,
    CompatibilityFactUpdateOutcomeV1, CompatibilityFactUpdatePatchV1,
};
pub use merge::{
    CompatibilityFactMergeCommandV1, CompatibilityFactMergeOutcomeV1,
    CompatibilityMemoryRepairCommandV1,
};
pub use operations::{
    CompatibilityFactAddAliasV1, CompatibilityFactCurationBatchV1,
    CompatibilityFactCurationOperationV1, CompatibilityFactCurationReceiptV1,
    CompatibilityFactLinkV1, CompatibilityFactMergeEntitiesV1, CompatibilityFactNormalizeTagsV1,
    CompatibilityFactRelationV1, CompatibilityFactRepairVectorV1,
    CompatibilityLegacyEntityTargetV1,
};

mod fact_commands;
mod merge;
mod operations;
mod validate;

const MAX_FACT_CURATION_OPERATIONS: usize = 256;

pub(super) const MAX_FACT_CURATION_TARGETS: usize = 256;

pub use fact_commands::{
    FactAddCommand, FactAddDisposition, FactAddOutcome, FactFeedbackCommand, FactFeedbackOutcome,
    FactRemoveCommand, FactRemoveOutcome, FactUpdateCommand, FactUpdateOutcome, FactUpdatePatch,
};
pub use merge::{FactMergeCommand, FactMergeOutcome, MemoryRepairCommand};
pub use operations::{
    FactAddAlias, FactCurationBatch, FactCurationOperation, FactCurationReceipt, FactEntityTarget,
    FactLink, FactMergeEntities, FactNormalizeTags, FactRelation, FactRepairVector,
};

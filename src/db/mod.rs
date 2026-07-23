mod access;
mod analytics;
mod connection;
mod coverage;
mod edges;
mod evidence_assembly;
mod files;
mod fingerprints;
pub(crate) mod libsql_local;
mod maintenance;
mod memory_v2;
mod metadata;
pub mod migrations;
mod nodes;
mod redundancy_pairs;
mod retrieval_anchor_authority;
pub(crate) mod retrieval_anchor_schema;
mod rows;
// S11: unreferenced since the dead pre-cutover adapters were removed; the S1
// runtime lane re-wires these graph read/maintenance facades to the registry.
#[allow(dead_code)]
pub(crate) mod runtime_compat;
mod search;
mod sql;
mod stats;
mod tx;
mod unresolved;

#[cfg(test)]
pub(crate) use access::DaemonDatabaseScope;
#[doc(hidden)]
pub use access::enter_maintenance_database_scope;
#[cfg(windows)]
pub(crate) use access::windows_hard_link_count;
pub use access::{DatabaseAuthority, DatabaseAuthorityRole};
pub(crate) use access::{
    DatabaseDeletionFence, DatabaseDeletionStates, WriterOwnership, database_path_is_tombstoned,
    enter_daemon_database_scope, is_lock_contended, probe_writer_owner,
};
pub use connection::{Database, SQLITE_UNSAFE_FAST_ENV};
pub(crate) use connection::{
    DatabaseWriterConnection, platform_safe_journal_mode, platform_safe_synchronous_mode,
};
pub use fingerprints::StoredFingerprint;
pub(crate) use memory_v2::{
    CapturedMemoryV2Frontiers, MemoryV2BackfillBatchOutcome, MemoryV2CutoverOutcome,
    MemoryV2CutoverReceipt, MemoryV2FeedbackHistoryRepairBatchOutcome,
    MemoryV2FeedbackHistoryRepairProgress,
};
pub use redundancy_pairs::{RedundancyPairRow, RedundancyPairWrite};
pub use retrieval_anchor_authority::{
    AnchorDerivativeKindV1, AnchorDispositionAppendOutcomeV1, AnchorDispositionReasonClassV1,
    AnchorDispositionStateV1, RetrievalAnchorDerivativeV1, RetrievalAnchorDispositionRecordV1,
};
pub(crate) use retrieval_anchor_authority::{
    publish_anchor_derivative, publish_fact_feedback_finding_tx, tombstone_fact_derivatives_tx,
};
pub use search::DependencyImportUse;

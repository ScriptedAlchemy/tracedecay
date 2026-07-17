mod access;
mod analytics;
mod connection;
mod coverage;
mod edges;
mod files;
mod fingerprints;
mod maintenance;
mod memory_v2;
mod metadata;
pub mod migrations;
mod nodes;
mod redundancy_pairs;
pub(crate) mod retrieval_anchor_schema;
mod rows;
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
pub use search::DependencyImportUse;

mod access;
mod analytics;
mod connection;
mod coverage;
mod edges;
pub(crate) mod engine;
mod evidence_assembly;
mod external_source;
mod file_identity;
mod files;
mod fingerprints;
mod maintenance;
mod memory_connection;
mod memory_v2;
mod metadata;
pub mod migrations;
mod nodes;
mod redundancy_pairs;
mod retrieval_anchor_authority;
pub(crate) mod retrieval_anchor_schema;
mod rows;
mod search;
mod sql;
mod stats;
mod tx;
mod unresolved;

pub(crate) use access::OwnedMaintenanceDatabaseScope;
#[doc(hidden)]
pub use access::enter_maintenance_database_scope;
#[cfg(not(any(test, feature = "test-transport")))]
pub(crate) use access::enter_owned_maintenance_database_scope;
#[cfg(any(test, feature = "test-transport"))]
pub(crate) use access::is_isolated_test_path;
#[cfg(windows)]
pub(crate) use access::windows_hard_link_count;
pub(crate) use access::{DaemonDatabaseScope, MaintenanceDatabaseScope};
pub use access::{DatabaseAuthority, DatabaseAuthorityRole};
pub(crate) use access::{
    DatabaseDeletionFence, DatabaseDeletionStates, WriterOwnership, database_path_is_tombstoned,
    enter_daemon_database_scope, is_lock_contended, probe_writer_owner,
};
#[cfg(any(test, feature = "test-transport"))]
pub use connection::TestDatabaseRuntimeMode;
pub use connection::{Database, SQLITE_UNSAFE_FAST_ENV};
pub(crate) use connection::{
    DatabaseAccessMode, DatabaseEngineConnection, DatabaseMemoryTransaction,
    DatabaseWriteTransaction,
};
pub(crate) use external_source::install_external_source_schema;
pub(crate) use file_identity::{SqliteFileIdentityError, sqlite_generation_identity};
pub use fingerprints::StoredFingerprint;
pub(crate) use memory_connection::MemoryConnection;
pub use memory_connection::SqliteDriverError;
pub(crate) use memory_v2::{
    CapturedMemoryV2Frontiers, MemoryV2ArchiveDatabase, MemoryV2BackfillBatchOutcome,
    MemoryV2CutoverOutcome, MemoryV2CutoverReceipt, MemoryV2FeedbackHistoryRepairBatchOutcome,
    MemoryV2FeedbackHistoryRepairProgress, MemoryV2LegacyPurgeReceipt,
    export_memory_v2_owner_archive, import_memory_v2_owner_archive, list_memory_v2_archive_owners,
    plan_memory_v2_owner_archive_import,
};
pub use redundancy_pairs::{RedundancyPairRow, RedundancyPairWrite};
pub(crate) use retrieval_anchor_authority::{
    publish_anchor_derivative, publish_fact_feedback_finding_tx, tombstone_fact_derivatives_tx,
};
pub use search::DependencyImportUse;
pub use tracedecay_store::{
    AnchorDerivativeKindV1, AnchorDispositionAppendOutcomeV1, AnchorDispositionReasonClassV1,
    AnchorDispositionStateV1, RetrievalAnchorDerivativeV1, RetrievalAnchorDispositionRecordV1,
    RetrievalAnchorOwnerV1,
};

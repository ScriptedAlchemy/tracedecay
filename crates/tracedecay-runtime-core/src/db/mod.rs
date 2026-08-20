mod access;
mod connection;
pub mod engine;
mod evidence_assembly;
mod external_source;
mod file_identity;
mod graph_publication;
mod memory_connection;
mod memory_v2;
mod metadata;
pub mod migrations;
mod retrieval_anchor_authority;
pub mod retrieval_anchor_schema;
mod semantic_vector_staging;
mod sql;

pub use access::OwnedMaintenanceDatabaseScope;
#[doc(hidden)]
pub use access::enter_maintenance_database_scope;
#[cfg(not(test))]
pub use access::enter_owned_maintenance_database_scope;
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
pub use access::is_isolated_test_path;
#[cfg(windows)]
pub use access::windows_hard_link_count;
pub use access::{DaemonDatabaseScope, MaintenanceDatabaseScope};
pub use access::{DatabaseAuthority, DatabaseAuthorityRole};
pub use access::{
    WriterOwnership, enter_daemon_database_scope, is_lock_contended, probe_writer_owner,
};
pub use connection::Database;
pub(crate) use connection::MemoryGraphReconciliationTaskScheduleV1;
pub use connection::{
    DatabaseAccessMode, DatabaseEngineConnection, DatabaseEngineReadSnapshot,
    DatabaseMemoryTransaction, DatabaseWriteTransaction,
};
pub use connection::{
    MemoryGraphReconciliationRetirementReservationV1, MemoryGraphReconciliationTaskOwnerV1,
};
pub use connection::{
    ProjectMemoryReconciliationTelemetryObserverV1, ProjectMemoryReconciliationTelemetrySnapshotV1,
};
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
pub use connection::{TestDatabaseRuntimeMode, TestDatabaseRuntimeScope};
pub use external_source::install_external_source_schema;
pub use file_identity::{SqliteFileIdentityError, sqlite_generation_identity};
pub use memory_connection::MemoryConnection;
pub use memory_connection::SqliteDriverError;
pub use metadata::BoundedMetadataValue;
pub(crate) use retrieval_anchor_authority::{
    publish_fact_feedback_finding_tx, tombstone_fact_derivatives_tx,
};
pub use sql::{
    CappedRowidScan, FULL_SCAN_PAGE_ROWS, build_qmark_placeholders, collect_rowid_pages,
    collect_rowid_pages_capped, collect_rowid_pages_capped_with, collect_rowid_pages_with,
};
pub use tracedecay_store::{
    AnchorDerivativeKindV1, AnchorDispositionAppendOutcomeV1, AnchorDispositionReasonClassV1,
    AnchorDispositionStateV1, RetrievalAnchorDerivativeV1, RetrievalAnchorDispositionRecordV1,
    RetrievalAnchorOwnerV1,
};

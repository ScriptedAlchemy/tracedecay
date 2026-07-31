//! Root-owned seam onto the extracted global-database crate.
//!
//! The implementation lives in `tracedecay-global-db`. This file exists only so
//! every existing `crate::global_db::…` path keeps resolving. The glob keeps the
//! crate-internal surface `pub(crate)` so extraction does not widen the root's
//! public API; the explicit list below re-publishes exactly the contracts
//! `src/global_db.rs` already exported publicly (integration tests and the SDK
//! reach them as `tracedecay::global_db::…`).

pub(crate) use tracedecay_global_db::*;

pub use tracedecay_global_db::{
    AccountingMode, AnalyticsEventInsert, AnalyticsEventQuery, AnalyticsEventRecord,
    AnalyticsHintCounts, AnalyticsToolCounts, CodeProjectRecord, GraphScopeRecord, GraphScopeUpsert,
    ParseOffset, PendingCodexCompactionSummary, ProjectAliasRecord, ProjectObservationStoreError,
    ProjectObservationStoreResolution, ProjectRegistryContext, ProjectStoreContext,
    ProjectStoreResolution, SavingsDay, SavingsTotal, SessionActivityRow, SessionIngestHealth,
    StoreArtifactRecord, StoreArtifactUpsert, StoreInstanceRecord, StoreInstanceUpsert,
    TranscriptBatch, WorkflowScopeFilter, configuration, env_flag, env_value_truthy,
    global_accounting_enabled, global_accounting_mode, global_db_path, global_db_path_is_overridden,
};

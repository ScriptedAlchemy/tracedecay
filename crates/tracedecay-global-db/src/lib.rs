//! User-level database that tracks all `TraceDecay` projects and their saved tokens.
//!
//! Stored at `~/.tracedecay/global.db`, this database holds one row per project
//! with the project's database path and its cumulative tokens-saved count. Read
//! paths are generally best-effort; authoritative open and maintenance
//! interfaces preserve failures for callers that must fail closed.

use tracedecay_sessions::runtime::SessionMessageSearchResult;
pub use tracedecay_store::ParseOffset;
use tracedecay_store::{SessionMessageRecord, SessionRecord};

mod api_types;
pub mod configuration;
mod git_index_transactions;
pub mod observation;
mod observation_adapter;
mod observation_projection;
mod registered_maintenance;
mod support;
pub use observation_adapter::GlobalDbObservationStore;
pub use observation_projection::{project_observation_with_engine, rebuild_projection_with_engine};
mod observation_store;
mod project_registry;
mod registered;
mod registered_accounting;
mod registered_analytics;
mod registered_dashboard;
mod registered_lcm;
mod registered_sessions;
pub mod schema_contract;
pub mod schema_stages;
pub use schema_stages::ensure_registered_schema;

/// Installs the canonical registered global/session schema installer into the
/// kernel's fail-closed [`tracedecay_runtime_core::ports::registered_schema`]
/// port for dependent test builds.
///
/// The kernel opens a profile- or session-scoped shard through that port when a
/// fixture calls `Database::publish_test_runtime`, but the real schema
/// ([`ensure_registered_schema`]) lives here in `tracedecay-global-db`, above
/// the kernel. Production wires the same installer through
/// `tracedecay-migrate`/the daemon; this helper lets the root crate's
/// integration suites (and this crate's own tests) register the identical real
/// schema without reaching into daemon internals. Idempotent — the port keeps
/// the first registration. Gated behind `test-helpers`, so no production build
/// gains a registrar and the port stays fail-closed when nothing registers.
#[cfg(any(test, feature = "test-helpers"))]
pub fn register_test_schema_installer() {
    tracedecay_runtime_core::ports::registered_schema::register(|connection| {
        Box::pin(ensure_registered_schema(connection))
    });
}

pub mod session_temporal;
pub use session_temporal::operations as session_temporal_operations;
mod transcript;

pub use git_index_transactions::{
    GitIndexReadExecutor, GlobalDbGitIndexTransactionStore, ensure_git_index_transaction_schema,
};
pub use observation_store::{ProjectObservationStoreError, ProjectObservationStoreResolution};
use project_registry::project_path_alias_key;
/// Registry reap contract, moved down beside `plan_registry_reap` — its only
/// producer — so this crate no longer reaches up into the composition root.
pub use project_registry::{
    GIT_COMMON_DIR_ALIAS_PREFIX, ReapEntryKind, RegistryReapEntry, RegistryReapPlan,
    RetainedRegistryEntry, alias_key_path, ephemeral_root_rejection, is_ephemeral_path,
};
pub use registered::{RegisteredGlobalDb, RegisteredGlobalDbWriteTransaction};
pub use session_temporal::{
    SessionTemporalHealthFindingKind, SessionTemporalHealthReport, SessionTemporalHealthStatus,
};
pub use transcript::TranscriptPersistenceError;

const UNIX_TIMESTAMP_MILLIS_THRESHOLD: i64 = 1_000_000_000_000;

pub use api_types::{
    AnalyticsEventInsert, AnalyticsEventQuery, AnalyticsEventRecord, AnalyticsHintCounts,
    AnalyticsToolCounts, CodeProjectRecord, GraphScopeRecord, GraphScopeUpsert,
    PendingCodexCompactionSummary, ProjectAliasRecord, ProjectRegistryContext, ProjectStoreContext,
    ProjectStoreResolution, SavingsDay, SavingsTotal, SessionActivityRow, SessionIngestHealth,
    StoreArtifactRecord, StoreArtifactUpsert, StoreInstanceRecord, StoreInstanceUpsert,
    TranscriptBatch,
};
pub use support::{
    AccountingMode, env_flag, env_value_truthy, estimate_tokens, global_accounting_enabled,
    global_accounting_mode, global_db_path, global_db_path_is_overridden,
};
use support::{
    SESSION_MESSAGE_SEARCH_MAX_FETCH, analytics_scope_query, downrank_inventory_messages,
    ensure_code_project_native_root_columns, ensure_parse_offset_columns,
    ensure_session_parent_columns, ensure_table_columns, git_remote_search_alias,
    global_db_operation_error, global_db_operation_message, interleave_workflow_search_results,
    like_pattern, normalize_git_remote_url, push_optional_analytics_filter, repo_identity_aliases,
    row_to_analytics_event, session_fts_query,
};
/// Compatibility re-export: workflow search filters now live beside the
/// workflow-index contracts in [`tracedecay_sessions::runtime::workflow_index`].
pub use tracedecay_sessions::runtime::workflow_index::WorkflowScopeFilter;
#[cfg(all(test, not(windows)))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod checkpoint_tests;
// The root crate's test suite drives this store through `tests::harness`, so
// the harness must survive being compiled as a dependency. `test-helpers` is
// the explicit opt-in dependent test builds enable.
#[cfg(any(test, feature = "test-helpers"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub mod tests;

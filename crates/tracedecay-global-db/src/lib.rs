//! User-level database that tracks all `TraceDecay` projects and their saved tokens.
//!
//! Stored at `~/.tracedecay/global.db`, this database holds one row per project
//! with the project's database path and its cumulative tokens-saved count. Read
//! paths are generally best-effort; authoritative open and maintenance
//! interfaces preserve failures for callers that must fail closed.
//!
//! ## Dependency edges
//!
//! Depends on `tracedecay-runtime-core` (kernel db/errors/storage/config),
//! `tracedecay-sessions` (session runtime, `lcm::contracts`,
//! `retrieval_content`), and `tracedecay-semantic` (resource ceilings, default
//! embedding model). All three are proven acyclic — `cargo tree -p <dep> -e
//! normal` never names this crate. `RuntimeExternalSourceStore` and
//! `GlobalDbObservationStore` is deliberately a root-owned adapter. It takes
//! a guarded database client issued by the registered owner, so the composition
//! root retains its own typed client without receiving raw runtime authority.

use tracedecay_sessions::runtime::SessionMessageSearchResult;
pub use tracedecay_store::ParseOffset;
use tracedecay_store::{SessionMessageRecord, SessionRecord};

mod api_types;
pub mod configuration;
#[cfg(test)]
mod delivery_settlement_tests;
mod discovery_queue;
mod git_index_transactions;
mod git_topology_anchor;
mod native_integration;
mod observability_rollup;
pub mod observation;
mod observation_adapter;
mod observation_projection;
mod registered_maintenance;
pub use registered_maintenance::{
    REGISTERED_WAL_RECLAIM_TRIGGER_BYTES, RegisteredWalCheckpointReceiptV1, RegisteredWalReclaimV1,
};
mod registered_provider_usage;
#[cfg(test)]
mod stack_delivery_tests;
mod support;
pub use discovery_queue::HostDiscoveryQueueEntry;
pub use git_topology_anchor::RegisteredGitTopologyAnchorAuthorityV2;
pub use observability_rollup::{
    ObservabilityRollupCompactionCandidateV1, ObservabilityRollupCompactionReceiptV1,
    ObservabilityRollupCompactionV1, ObservabilityRollupDirtyDayClaimV1,
    ObservabilityRollupEmptyDayClaimOutcomeV1, ObservabilityRollupEmptyDayClaimV1,
    ObservabilityRollupFragmentPageV1, ObservabilityRollupFragmentQueryV1,
    ObservabilityRollupFragmentRecordV1, ObservabilityRollupFrontierV1,
    ObservabilityRollupRebuildReceiptV1, ObservabilityRollupRebuildV1,
    ObservabilityRollupRetentionReceiptV1, ensure_observability_rollup_schema,
};
pub use observation_adapter::GlobalDbObservationStore;
pub use observation_projection::{project_observation, rebuild_projection};
#[cfg(test)]
pub use observation_projection::{project_observation_with_engine, rebuild_projection_with_engine};
pub use tracedecay_domain::CoverageStateV1;
mod observation_store;
mod project_registry;
mod registered;
mod registered_accounting;
mod registered_analytics;
mod registered_dashboard;
mod registered_lcm;
mod registered_lcm_privacy;
mod registered_legacy_relations;
mod registered_session_sync;
mod registered_sessions;
pub mod registry_maintenance;
mod remote_deletion;
pub mod schema_contract;
pub mod schema_stages;
mod stack_delivery;
pub use schema_stages::ensure_registered_schema;
pub use stack_delivery::{
    GitHubStackDeliveryKeyV1, GitHubStackDeliveryRecordV1, GitHubStackDeliveryStateV1,
    GitHubStackSignalAppendOutcomeV1, GitHubStackSignalRecordV1,
    MAX_GITHUB_STACK_ACTIVE_PENDING_V1, MAX_GITHUB_STACK_DELIVERY_BATCH_V1,
};

/// Installs the canonical registered global/session schema installer into the
/// kernel's fail-closed [`tracedecay_runtime_core::ports::registered_schema`]
/// port for dependent test builds.
///
/// The kernel opens a profile- or session-scoped shard through that port when a
/// fixture calls `Database::publish_test_runtime`, but the real schema
/// ([`ensure_registered_schema`]) lives here in `tracedecay-global-db`, above
/// the kernel. Production wires the same installer through the daemon
/// (`register_registered_schema_installer`); this helper lets the root crate's
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
    GitIndexPreviewInputGcResult, GitIndexReadExecutor, GlobalDbGitIndexTransactionStore,
    ensure_git_index_transaction_schema,
};
pub use native_integration::{GlobalDbNativeIntegrationStore, ensure_native_integration_schema};
pub use observation_store::{ProjectObservationStoreError, ProjectObservationStoreResolution};
use project_registry::project_path_alias_key;
/// Registry reap contract, moved down beside `plan_registry_reap` — its only
/// producer — so this crate no longer reaches up into the composition root.
pub use project_registry::{
    EPHEMERAL_PROJECT_ROOT_REASON_CODE, GIT_COMMON_DIR_ALIAS_PREFIX, PROJECT_REGISTRY_AUTHORITY,
    ReapEntryKind, RegistryReapEntry, RegistryReapPlan, RetainedRegistryEntry, alias_key_path,
    ephemeral_root_rejection, is_ephemeral_path,
};
pub use registered::{
    DeliveryAttemptClaimV1, DeliverySourceReceiptReadV1, DurableDeliverySettlementReceiptV1,
    MAX_PENDING_RECEIPTED_DELIVERIES_V1, MAX_WORK_ATTEMPT_DELIVERY_FANOUTS_V1,
    PendingDeliverySourceReceiptV1, RegisteredGlobalDb, RegisteredGlobalDbLeaseV1,
    RegisteredGlobalDbOwnerV1, RegisteredGlobalDbWeakLeaseIssuerV1,
    RegisteredGlobalDbWriteTransaction, RegisteredWorkApplicationServicesV1,
    RegisteredWorkProductServicesV1, RegisteredWorkflowApplicationServicesV1,
    WorkAttemptDeliveryCensusReadV1,
};
pub use registered_analytics::ObservabilityRetentionReceiptV1;
pub use registered_lcm_privacy::{LcmPrivacyRescanOutcomeV1, LcmPrivacyRescanReceiptV1};
pub use remote_deletion::{
    RemoteDeletionCleanupState, RemoteDeletionFailureCode, RemoteDeletionPhase,
    RemoteDeletionTarget, RemoteDeletionTombstone, RemoteDeletionTombstoneRecordOutcome,
    RemoteDeletionTombstoneTransitionOutcome,
};
pub use session_temporal::{
    SessionTemporalHealthFindingKind, SessionTemporalHealthReport, SessionTemporalHealthStatus,
};
pub use tracedecay_runtime_core::store_runtime::{
    VerifiedGraphRuntimePortV1, VerifiedGraphRuntimeWeakProxyV1,
};
pub use transcript::TranscriptPersistenceError;

const UNIX_TIMESTAMP_MILLIS_THRESHOLD: i64 = 1_000_000_000_000;

pub use api_types::{
    AnalyticsEventInsert, AnalyticsEventQuery, AnalyticsEventRecord, AnalyticsHintCounts,
    AnalyticsToolCounts, CodeProjectRecord, GraphScopeRecord, GraphScopeUpsert,
    ObservabilityEmissionClaimV1, ObservabilityEmissionOutboxRecordV1, ProjectAliasRecord,
    ProjectRegistryContext, ProjectStoreContext, ProjectStoreResolution,
    RegisteredProjectRootInventoryV1, SavingsDay, SavingsTotal, SessionActivityRow,
    SessionIngestHealth, SessionProviderCoverage, SessionProviderCoverageState,
    StoreArtifactRecord, StoreArtifactUpsert, StoreInstanceRecord, StoreInstanceUpsert,
    TranscriptBatch,
};
pub use support::{
    AccountingMode, env_flag, env_value_truthy, estimate_tokens, global_accounting_enabled,
    global_accounting_mode, global_db_path, global_db_path_is_overridden,
};
use support::{
    SESSION_MESSAGE_SEARCH_MAX_FETCH, analytics_scope_query, downrank_inventory_messages,
    ensure_code_project_primary_root_columns, ensure_parse_offset_columns,
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
#[cfg(test)]
mod observability_outbox_tests;
#[cfg(test)]
mod observability_rollup_tests;
#[cfg(any(test, feature = "test-helpers"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub mod tests;

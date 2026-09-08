//! The canonical TraceDecay store runtime.
//!
//! This crate is the single owner of store, shard, and session lifecycle for
//! a daemon: the session registry that implements
//! [`tracedecay_application::tracedecay::ProjectStoreRuntimeV1`] (project-store
//! attachment, retirement, schema convergence, maintenance, graph runtime
//! binding, and shutdown), the store locator resolver it opens shards through,
//! the Remote Brain credential authority it mounts, and the remote-replay
//! transaction worker. The composition root (`tracedecay`) wires these against
//! daemon engine state; this crate never depends on the root aggregate.
//!
//! Database kernels sit below it and hold no lifecycle policy of their own:
//! `tracedecay_runtime_core::shard_runtime` is the per-shard `SQLite` runtime
//! and registry this crate drives through `StoreRuntimeResolver` and
//! `ShardRuntimePublisher`, `tracedecay-rusqlite-runtime` the engine,
//! `tracedecay-global-db` the registered schema, and `tracedecay-graph-db`
//! the graph store.
//!
//! The `tracedecay-application` dependency is only for that store-runtime port
//! and for implementing [`tracedecay_code_index_runtime::CodeGraphSeatLeaseV1`]
//! / verified semantic-vector adapters, whose signatures already name
//! usecases semantic-runtime types. Observation cancellation comes from
//! `tracedecay_sessions::observation`.
//!
//! `RemoteRecoveryProjectLifecycleV1` stays in the root crate: it holds
//! daemon invocation, project-open, and retirement state that cannot be
//! severed through an existing recovery port.

pub mod remote_credentials;
pub mod remote_replay_transaction;
pub mod session_registry;
pub mod store_locator_resolver;

mod schema;

pub use remote_credentials::{
    DaemonRemoteCredentialAuthorityV1, DaemonRemoteCredentialLookupV1,
    DaemonRemoteCredentialRegistryErrorV1, MAX_REGISTERED_REMOTE_NODES,
    RegisteredRemoteNodeStoreV1, RemoteOperationalStatusProviderV1,
};
pub use remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1;
pub use schema::register_registered_schema_installer;
#[cfg(any(test, feature = "test-helpers"))]
pub use session_registry::maintenance::RegisteredSchemaConvergenceTestGate;
pub use session_registry::maintenance::{
    ForegroundProjectOpenAdmission, RegisteredSchemaConvergenceStatus,
};
pub use session_registry::{
    DaemonSessionRuntimeRegistryV1, MAX_RETAINED_GRAPH_DB_OWNERS, RemoteRecoveryAdmission,
    RemoteRecoveryProjectLifecycle, RemoteRecoveryQuiescence,
    mark_process_long_lived_for_session_maintenance, open_user_memory_db,
    process_runtime_generation, registry_open_error, release_process_allocator_memory,
};

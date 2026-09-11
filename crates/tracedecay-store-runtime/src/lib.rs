//! The canonical TraceDecay store runtime.
//!
//! This crate is the single owner of store, shard, and session lifecycle for
//! a daemon: the concrete session registry owns project-store attachment,
//! retirement, schema convergence, maintenance, graph runtime binding, and
//! shutdown; the store locator resolver opens shards; the Remote Brain
//! credential authority mounts nodes; and the remote-replay transaction worker
//! applies replay. The composition root (`tracedecay`) wires these against
//! daemon engine state; this crate never depends on the root aggregate.
//!
//! Database kernels sit below it and hold no lifecycle policy of their own:
//! `tracedecay_runtime_core::shard_runtime` is the per-shard `SQLite` runtime
//! and registry this crate drives through `StoreRuntimeResolver` and
//! `ShardRuntimePublisher`, `tracedecay-rusqlite-runtime` the engine,
//! `tracedecay-global-db` the registered schema, and `tracedecay-graph-db`
//! the graph store.
//!
//! The `tracedecay-application` dependency remains for implementing
//! [`tracedecay_code_index_runtime::CodeGraphSeatLeaseV1`] and verified
//! semantic-vector adapters whose signatures name application runtime types.
//! Observation cancellation comes from
//! `tracedecay_sessions::observation`.
//!
//! `RemoteRecoveryProjectLifecycleV1` stays in the root crate: it holds
//! daemon invocation, project-open, and retirement state that cannot be
//! severed through an existing recovery port.

pub mod remote_credentials;
pub mod remote_frame_transfer;
pub mod remote_query;
pub mod remote_replay_transaction;
pub mod retained_memory;
pub mod semantic_artifact_gc;
pub mod session_registry;
pub mod standalone_session;
pub mod store_locator_resolver;
pub mod store_shutdown;
pub mod writer_gate;

pub use remote_credentials::{
    DaemonRemoteCredentialAuthorityV1, DaemonRemoteCredentialLookupV1,
    DaemonRemoteCredentialRegistryErrorV1, MAX_REGISTERED_REMOTE_NODES,
    RegisteredRemoteNodeStoreV1,
};
pub use remote_frame_transfer::DaemonRemoteFrameTransferProtocolPortV1;
pub use remote_query::DaemonRemoteExactObservationQueryPortV1;
pub use remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1;
pub use semantic_artifact_gc::{
    SemanticArtifactGcMaintenanceTask, spawn_semantic_artifact_gc_maintenance,
};
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
pub use standalone_session::join_standalone_session_registry;
pub use store_shutdown::{
    ShutdownStatus, ShutdownTaskOutcome, ShutdownTaskReceipt, ShutdownTaskStatus,
    join_shutdown_tasks_until,
};
pub use writer_gate::{StoreWriterClass, StoreWriterGates, WriterAdmissionGuard, WriterScope};

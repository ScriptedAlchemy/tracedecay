//! Seat-gate port over `session_registry/code_graph`.
//!
//! The code-index scheduler seats a sealed generation by retaining a code-graph
//! runtime and then publishing/loading through that lease. Those two calls used
//! to name [`DaemonSessionRuntimeRegistryV1`] and
//! [`RetainedCodeGraphRuntimeV1`] on the scheduler side, which is what blocks
//! extracting `code_index_scheduler` (slice 10).
//!
//! This module is the smallest typed inversion of that seam:
//! - [`CodeGraphSeatRuntimePortV1`] is what the scheduler mounts and activates
//!   through.
//! - [`CodeGraphSeatLeaseV1`] is the short-lived activation handle the port
//!   returns.
//! - [`CodeGraphReplayBindingV1`] is the retain input that previously lived on
//!   the scheduler and was imported *back* by the registry.
//!
//! Home: the root `store_runtime` boundary, not `tracedecay-code-index` or
//! `tracedecay-application`. The lease surface names both
//! `CanonicalCodeGraphStoreLeaseV1` (runtime-core) and
//! `CodeIndexPublishedGenerationV1` (code-index). Application cannot take the
//! code-index type (`code-index` already depends on it). Runtime-core cannot
//! take the code-index type (that would put grammars on the kernel path).
//! Code-index does not depend on runtime-core today; adding that edge just to
//! host this port would be a new spine compile key. Slice 10 moves this module
//! into `tracedecay-code-index-runtime`; the registry stays in root and
//! implements the imported traits. Recovery/CAS marker semantics stay on the
//! registry implementation and are not restated here.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, atomic::AtomicBool};

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::{CodeGenerationId, ProjectId, RefId, RepositoryId, WorktreeId};
use tracedecay_graph_db::{
    GraphDbError, SealedGraphStateDigest, SealedReadBundleArtifactStateV1, VerifiedGraphSnapshot,
};
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::errors::Result;
use tracedecay_runtime_core::store_runtime::registry::CanonicalCodeGraphStoreLeaseV1;

/// Sealed-generation replay identity the seat port needs to retain a runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CodeGraphReplayBindingV1 {
    pub generations_root: PathBuf,
    pub sealed_state_digest: SealedGraphStateDigest,
}

/// Short-lived activation lease returned by [`CodeGraphSeatRuntimePortV1`].
///
/// The serving slot keeps only [`Self::authority`]; the lease itself is dropped
/// at the end of persistent graph activation, matching the pre-port inherent
/// `RetainedCodeGraphRuntimeV1` lifetime.
pub(crate) trait CodeGraphSeatLeaseV1: Send {
    fn sweep_aborted_read_bundle_temporaries(&self) -> std::result::Result<(), GraphDbError>;

    fn authority(&self) -> Arc<CanonicalCodeGraphStoreLeaseV1>;

    fn publish_verified_snapshot(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
        request_cancelled: Arc<AtomicBool>,
    ) -> std::result::Result<VerifiedGraphSnapshot, GraphDbError>;

    fn load_sealed_read_bundle_catalog(
        &self,
        request_cancelled: &Arc<AtomicBool>,
    ) -> std::result::Result<SealedReadBundleArtifactStateV1, GraphDbError>;
}

/// Registry-side seat gate the code-index scheduler consumes.
///
/// Object-safe so `CodeGraphActivationAuthorityV1::Persistent` can hold one
/// `Arc<dyn …>` instead of the whole session-registry aggregate.
pub(crate) trait CodeGraphSeatRuntimePortV1: Send + Sync {
    fn retain_code_graph_runtime(
        &self,
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: WorktreeId,
        reference: Option<RefId>,
        generation_id: CodeGenerationId,
        project_database: Arc<Database>,
        replay_binding: CodeGraphReplayBindingV1,
        decoded_generation: Option<Arc<CodeIndexPublishedGenerationV1>>,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn CodeGraphSeatLeaseV1 + Send>>> + Send + '_>>;
}

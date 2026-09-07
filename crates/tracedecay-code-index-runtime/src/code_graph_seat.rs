//! Seat-gate port over `session_registry/code_graph`.
//!
//! The code-index scheduler seats a sealed generation by retaining a code-graph
//! runtime and then publishing/loading through that lease. Those two calls used
//! to name `DaemonSessionRuntimeRegistryV1` and `RetainedCodeGraphRuntimeV1` on
//! the scheduler side.
//!
//! This crate owns the port:
//! - [`CodeGraphSeatRuntimePortV1`] is what the scheduler mounts and activates
//!   through.
//! - [`CodeGraphSeatLeaseV1`] is the short-lived activation handle the port
//!   returns.
//! - [`CodeGraphReplayBindingV1`] is the retain input.
//!
//! The root registry implements these traits. Recovery/CAS marker semantics
//! stay on the registry implementation and are not restated here.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, atomic::AtomicBool};

use tracedecay_code_index::production::CodeIndexPublishedGenerationV1;
use tracedecay_domain::errors::Result;
use tracedecay_domain::{CodeGenerationId, ProjectId, RefId, RepositoryId, WorktreeId};
use tracedecay_graph_db::{
    GraphDbError, GraphGenerationDependency, SealedGraphStateDigest,
    SealedReadBundleArtifactStateV1, VerifiedGraphSnapshot,
};
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::store_runtime::registry::CanonicalCodeGraphStoreLeaseV1;
use tracedecay_store::{StoreRuntimeBindingV1, StoreShardIdV1};
use tracedecay_usecases::semantic_runtime::{
    SemanticVectorGraphScopeV1, VerifiedSemanticVectorGraphRuntimeV1,
};

/// Sealed-generation replay identity the seat port needs to retain a runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeGraphReplayBindingV1 {
    pub generations_root: PathBuf,
    pub sealed_state_digest: SealedGraphStateDigest,
}

/// Short-lived activation lease returned by [`CodeGraphSeatRuntimePortV1`].
///
/// The serving slot keeps [`Self::authority`]. The activation lease may remain
/// alive in the detached catalog-restore task so optional read artifacts never
/// delay occurrence graph publication.
pub trait CodeGraphSeatLeaseV1: Send {
    fn sweep_aborted_read_bundle_temporaries(&self) -> std::result::Result<(), GraphDbError>;

    fn authority(&self) -> Arc<CanonicalCodeGraphStoreLeaseV1>;

    fn publish_verified_snapshot(
        &self,
        generation: &CodeIndexPublishedGenerationV1,
        request_cancelled: Arc<AtomicBool>,
    ) -> std::result::Result<VerifiedGraphSnapshot, GraphDbError>;

    fn recover_verified_snapshot_from_head(
        &self,
        request_cancelled: Arc<AtomicBool>,
    ) -> std::result::Result<VerifiedGraphSnapshot, GraphDbError>;

    fn load_sealed_read_bundle_catalog(
        &self,
        request_cancelled: &Arc<AtomicBool>,
    ) -> std::result::Result<SealedReadBundleArtifactStateV1, GraphDbError>;

    fn semantic_vector_identity(
        &self,
    ) -> std::result::Result<
        (
            ProjectId,
            RepositoryId,
            WorktreeId,
            CodeGenerationId,
            GraphGenerationDependency,
        ),
        GraphDbError,
    >;

    fn semantic_vector_staging_binding(&self) -> (StoreShardIdV1, StoreRuntimeBindingV1);

    /// Convert this lease into the verified semantic-vector runtime adapter.
    ///
    /// The adapter lives on the registry side (it forwards the retained
    /// runtime's semantic-vector methods). The scheduler provider only holds
    /// the seat port.
    fn into_semantic_vector_runtime(
        self: Box<Self>,
        scope: SemanticVectorGraphScopeV1,
    ) -> Arc<dyn VerifiedSemanticVectorGraphRuntimeV1>;
}

/// Registry-side seat gate the code-index scheduler consumes.
///
/// Object-safe so `CodeGraphActivationAuthorityV1::Persistent` can hold one
/// `Arc<dyn …>` instead of the whole session-registry aggregate.
/// Boxed lease future returned by [`CodeGraphSeatRuntimePortV1`].
pub type CodeGraphSeatLeaseFutureV1<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn CodeGraphSeatLeaseV1 + Send>>> + Send + 'a>>;

pub trait CodeGraphSeatRuntimePortV1: Send + Sync {
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
    ) -> CodeGraphSeatLeaseFutureV1<'_>;
}

//! Root-crate persistence adapters for store-facing contracts.
//!
//! Adapters in this module borrow already-open authoritative stores. They do
//! not discover paths, open connections, or own transaction state.

use std::future::Future;
use std::path::Path;

use tracedecay_store::{ParseOffset, TranscriptStore, TranscriptStoreResult, TranscriptWriteBatch};

use crate::sessions::SessionRecord;
use crate::sessions::git_correlation::{CommitSessionRecord, SpanObservation};

pub mod git_correlation;
pub mod global_db;
pub mod memory;
pub mod observation;
pub mod session;
pub mod workflow;
pub(crate) mod vector_generations;

pub use git_correlation::GlobalDbGitCorrelationStore;
pub use global_db::GlobalDbTranscriptStore;
pub use memory::DatabaseFactStore;
pub use observation::GlobalDbObservationStore;
pub use session::{
    GlobalDbSessionTemporalStore, SessionRefreshRecoveryV1, SessionRefreshRestartStateV1,
};
pub use workflow::GlobalDbWorkflowStore;

/// Typed integration-test surface for the vector-generation state machine.
///
/// This keeps tests on the same nominal projector and store types used by the
/// product without exposing database-engine connections or SQL primitives.
#[doc(hidden)]
pub mod vector_generation_test_support {
    pub use crate::semantic_code::projector::{
        CanonicalChunkVectorEncoderV1, PreparedVectorGenerationV1, ProjectedChunkVectorV1,
        SemanticProjectionErrorV1, prepare_vector_generation, prepare_vector_generation_async,
    };

    pub use super::vector_generations::{
        DatabaseVectorGenerationStoreV1, FakeVectorGenerationStoreV1, PublishedVectorGenerationV1,
        VectorGenerationBuildIdV1, VectorGenerationIdV1, VectorGenerationPlanV1,
        VectorGenerationPublicationV1, VectorGenerationStoreErrorV1, VectorProjectionCheckpointV1,
    };

    /// Inject one failure immediately before the oracle's publication swap.
    pub fn fail_before_publication_swap_once(store: &mut FakeVectorGenerationStoreV1) {
        store.fail_before_publication_swap_once();
    }
}

/// Application boundary required by production transcript ingestion.
///
/// The portable store contract owns cursor and transcript writes. The root
/// application extends it with session merge reads and git evidence that must
/// commit in the same authoritative transaction.
pub(crate) trait TranscriptIngestStore: TranscriptStore {
    fn advance_parse_offset_monotonic(
        &self,
        cursor_path: &Path,
        offset: ParseOffset,
    ) -> impl Future<Output = TranscriptStoreResult<()>> + Send {
        async move {
            let expected = self.get_parse_offset(cursor_path).await?;
            let batch =
                TranscriptWriteBatch::advance_offset(cursor_path.to_path_buf(), expected, offset)?;
            self.persist_transcript_batch(batch).await
        }
    }

    fn record_session_ingest_activity(
        &self,
        _project_root: &Path,
        _units: u64,
        _provider: &'static str,
    ) -> impl Future<Output = ()> + Send {
        async {}
    }

    fn get_session(
        &self,
        provider: &str,
        session_id: &str,
    ) -> impl Future<Output = TranscriptStoreResult<Option<SessionRecord>>> + Send;

    fn persist_transcript_batch_with_git_evidence(
        &self,
        batch: TranscriptWriteBatch,
        commit_records: &[CommitSessionRecord],
        span_observations: &[SpanObservation],
    ) -> impl Future<Output = TranscriptStoreResult<()>> + Send;
}

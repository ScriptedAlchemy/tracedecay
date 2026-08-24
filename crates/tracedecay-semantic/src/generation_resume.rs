use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};

use tracedecay_domain::{CodeGenerationId, ProjectionKeyV1};

use crate::fastembed_adapter::FastEmbedEmbeddingRuntime;
use crate::runtime_query::CurrentSemanticQueryRuntimeV1;
use crate::{
    PreparedSemanticRuntimeCommitV1, SemanticRuntimeScheduleFailureV1, SemanticRuntimeService,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticProjectionResumeOutcomeV1 {
    ReplayFromStart,
    CompletedBatches(u64),
    AlreadyPublished,
}

pub(super) type SemanticProjectionResumeFutureV1 = Pin<
    Box<
        dyn Future<
                Output = Result<
                    SemanticProjectionResumeOutcomeV1,
                    SemanticRuntimeScheduleFailureV1,
                >,
            > + Send
            + 'static,
    >,
>;

/// Opens a staged projection before encoder work and distinguishes exact
/// terminal publication from resumable batch progress.
pub(super) type SemanticProjectionResumeV1 =
    Box<dyn FnOnce() -> SemanticProjectionResumeFutureV1 + Send + 'static>;

pub(super) fn completed_batch_offset(
    outcome: SemanticProjectionResumeOutcomeV1,
    batch_count: usize,
) -> Result<Option<usize>, SemanticRuntimeScheduleFailureV1> {
    match outcome {
        SemanticProjectionResumeOutcomeV1::ReplayFromStart => Ok(Some(0)),
        SemanticProjectionResumeOutcomeV1::CompletedBatches(completed) => {
            let completed = usize::try_from(completed)
                .map_err(|_| SemanticRuntimeScheduleFailureV1::Publication)?;
            if completed > batch_count {
                return Err(SemanticRuntimeScheduleFailureV1::Publication);
            }
            Ok(Some(completed))
        }
        SemanticProjectionResumeOutcomeV1::AlreadyPublished => Ok(None),
    }
}

pub(super) fn install_candidate_on_success(
    commit: PreparedSemanticRuntimeCommitV1,
    expected_source: CodeGenerationId,
    expected_projection: ProjectionKeyV1,
    runtime: Arc<RwLock<Option<CurrentSemanticQueryRuntimeV1<FastEmbedEmbeddingRuntime>>>>,
    candidate: Arc<SemanticRuntimeService<FastEmbedEmbeddingRuntime>>,
    query_in_flight: Arc<AtomicBool>,
) -> PreparedSemanticRuntimeCommitV1 {
    commit.on_success(move |pointer| {
        if pointer.source_generation != expected_source
            || pointer.projection_key != expected_projection
        {
            return Err(SemanticRuntimeScheduleFailureV1::Publication);
        }
        *runtime
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(CurrentSemanticQueryRuntimeV1::new_with_admission(
                pointer.clone(),
                candidate,
                query_in_flight,
            ));
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::{SemanticProjectionResumeOutcomeV1, completed_batch_offset};

    #[test]
    fn published_resume_is_distinct_from_a_large_batch_count() {
        assert_ne!(
            SemanticProjectionResumeOutcomeV1::AlreadyPublished,
            SemanticProjectionResumeOutcomeV1::CompletedBatches(u64::MAX)
        );
    }

    #[test]
    fn completed_batch_offset_rejects_progress_beyond_the_canonical_plan() {
        assert_eq!(
            completed_batch_offset(SemanticProjectionResumeOutcomeV1::CompletedBatches(2), 1),
            Err(super::SemanticRuntimeScheduleFailureV1::Publication)
        );
        assert_eq!(
            completed_batch_offset(SemanticProjectionResumeOutcomeV1::AlreadyPublished, 1),
            Ok(None)
        );
    }
}

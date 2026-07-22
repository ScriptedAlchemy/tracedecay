//! Production bridge between daemon semantic scheduling and application search.
//!
//! Saved code generations call [`schedule_saved_code_generation`] without waiting
//! for FastEmbed download/indexing. Application search admits a semantic lane
//! only through [`query_factory`] once a complete compatible generation is
//! atomically current. Status projection carries indexing progress, degraded
//! reason, and prior generation for Doctor/`tracedecay_runtime`.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, UtcMicros, VectorGenerationIdV1, canonical_sha256,
};

use crate::code_index::production::CodeIndexPublishedGenerationV1;
use crate::query::retrieval::semantic::{
    CompleteSemanticGenerationV1, SemanticCalibrationProfileV1, SemanticIndexStateV1,
    SemanticLaneReadinessV1, SemanticRetrievalRequestV1,
};
use crate::semantic_code::projector::PreparedVectorGenerationV1;
use crate::semantic_code::{
    DaemonSemanticQueryFactoryV1, DaemonSemanticRuntimeHandleV1,
    FastEmbedSemanticGenerationRequestV1, LoadedSemanticArtifactV1,
    PreparedSemanticRuntimeCommitV1, SemanticGenerationPointerV1,
    SemanticRuntimeScheduleFailureV1, SemanticRuntimeScheduleStatusV1,
    SemanticRuntimeStatusProjectionV1,
};

use super::ports::{
    SemanticActivationCommandV1, SemanticActivationReceiptV1, SemanticActivationRequestV1,
    SemanticConfigurationPinV1, SemanticFallbackReasonV1, SemanticRollbackCommandV1,
    SemanticRollbackReceiptV1, SemanticRuntimeBackendErrorV1, SemanticRuntimeBackendV1,
    SemanticRuntimeFuture, SemanticRuntimeStateV1, SemanticRuntimeStatusV1,
};

/// Map daemon schedule projection into the application/Doctor status shape.
///
/// Indexing never blocks exact/lexical/graph; the route remains lexical until
/// [`SemanticRuntimeStateV1::Current`].
pub fn application_status_from_projection(
    projection: &SemanticRuntimeStatusProjectionV1,
    configuration: Option<SemanticConfigurationPinV1>,
) -> SemanticRuntimeStatusV1 {
    let state = match &projection.status {
        SemanticRuntimeScheduleStatusV1::Unavailable => SemanticRuntimeStateV1::Unavailable {
            reason: projection
                .degraded_reason
                .unwrap_or(SemanticFallbackReasonV1::RuntimeUnavailable),
        },
        SemanticRuntimeScheduleStatusV1::Indexing {
            target_generation,
            completed_units,
            total_units,
            ..
        } => SemanticRuntimeStateV1::Indexing {
            target_generation: provisional_vector_generation(target_generation),
            completed_units: *completed_units,
            total_units: *total_units,
        },
        SemanticRuntimeScheduleStatusV1::Failed {
            reason,
            prior_generation,
        } => SemanticRuntimeStateV1::Degraded {
            active_generation: prior_generation.clone().or_else(|| {
                projection.prior_generation.clone()
            }),
            reason: match reason {
                SemanticRuntimeScheduleFailureV1::Artifact => {
                    SemanticFallbackReasonV1::ArtifactUnavailable
                }
                SemanticRuntimeScheduleFailureV1::Cancelled => {
                    SemanticFallbackReasonV1::RuntimeUnavailable
                }
                SemanticRuntimeScheduleFailureV1::Runtime
                | SemanticRuntimeScheduleFailureV1::Projection
                | SemanticRuntimeScheduleFailureV1::Publication => {
                    SemanticFallbackReasonV1::RuntimeFailure
                }
            },
        },
        SemanticRuntimeScheduleStatusV1::Current { generation } => {
            match configuration
                .as_ref()
                .and_then(|pin| synthesize_current_receipt(pin, generation))
            {
                Some(receipt) => SemanticRuntimeStateV1::Current { receipt },
                None => SemanticRuntimeStateV1::Degraded {
                    active_generation: Some(generation.clone()),
                    reason: SemanticFallbackReasonV1::InvalidRuntimeStatus,
                },
            }
        }
    };
    SemanticRuntimeStatusV1::new(configuration, state)
}

/// Schedule FastEmbed projection for one published code generation.
///
/// Returns immediately after enqueueing; artifact load, model download, and
/// indexing run asynchronously and never join into ordinary search.
pub fn schedule_saved_code_generation<LoadArtifact, StageProjection, StageFuture>(
    handle: &DaemonSemanticRuntimeHandleV1,
    generation: &CodeIndexPublishedGenerationV1,
    load_artifact: LoadArtifact,
    stage_projection: StageProjection,
) -> bool
where
    LoadArtifact: FnOnce() -> Result<LoadedSemanticArtifactV1, SemanticRuntimeScheduleFailureV1>
        + Send
        + 'static,
    StageProjection: FnOnce(PreparedVectorGenerationV1) -> StageFuture + Send + 'static,
    StageFuture: Future<
            Output = Result<PreparedSemanticRuntimeCommitV1, SemanticRuntimeScheduleFailureV1>,
        > + Send
        + 'static,
{
    let Ok(request) = FastEmbedSemanticGenerationRequestV1::new(
        generation.manifest().generation_id.clone(),
        generation.projection().request().clone(),
        generation.chunks().chunks().to_vec(),
        load_artifact,
        stage_projection,
    ) else {
        return false;
    };
    // Enqueue only — callers must not await download/index completion.
    handle.schedule_generation(request)
}

/// Application search admits semantics only when `query_factory` observes the
/// atomically current compatible generation.
pub fn semantic_lane_readiness_for_request<'a>(
    handle: &DaemonSemanticRuntimeHandleV1,
    request: &'a SemanticRetrievalRequestV1<'a>,
    generation: &'a CompleteSemanticGenerationV1,
    calibration: Option<&'a SemanticCalibrationProfileV1>,
) -> SemanticLaneReadinessV1<'a> {
    match handle.query_factory(
        &request.code_generation,
        &request.vector_generation,
        request.projection.projection_key(),
    ) {
        Some(_) => SemanticLaneReadinessV1::Ready {
            request,
            generation,
            calibration,
        },
        None => SemanticLaneReadinessV1::Unavailable(index_state_from_status(handle.status())),
    }
}

/// Obtain a query factory only for the atomically current generation.
pub fn current_query_factory(
    handle: &DaemonSemanticRuntimeHandleV1,
) -> Option<(SemanticGenerationPointerV1, DaemonSemanticQueryFactoryV1)> {
    let pointer = handle.current()?;
    let factory = handle.query_factory(
        &pointer.source_generation,
        &pointer.generation,
        &pointer.projection_key,
    )?;
    Some((pointer, factory))
}

/// Daemon backend that surfaces schedule projection through the application port.
pub struct DaemonSemanticRuntimeBackendV1 {
    handle: DaemonSemanticRuntimeHandleV1,
    configuration: Mutex<Option<SemanticConfigurationPinV1>>,
}

impl DaemonSemanticRuntimeBackendV1 {
    pub fn new(handle: DaemonSemanticRuntimeHandleV1) -> Self {
        Self {
            handle,
            configuration: Mutex::new(None),
        }
    }

    pub fn handle(&self) -> &DaemonSemanticRuntimeHandleV1 {
        &self.handle
    }

    pub fn bind_configuration(&self, pin: SemanticConfigurationPinV1) {
        *self
            .configuration
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(pin);
    }

    pub fn application_status(&self) -> SemanticRuntimeStatusV1 {
        let configuration = self
            .configuration
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        application_status_from_projection(&self.handle.status_projection(), configuration)
    }
}

impl SemanticRuntimeBackendV1 for DaemonSemanticRuntimeBackendV1 {
    fn status<'a>(
        &'a self,
        configuration: &'a SemanticConfigurationPinV1,
    ) -> SemanticRuntimeFuture<'a, Result<SemanticRuntimeStateV1, SemanticRuntimeBackendErrorV1>>
    {
        Box::pin(async move {
            self.bind_configuration(configuration.clone());
            Ok(self.application_status().state)
        })
    }

    fn activate<'a>(
        &'a self,
        command: &'a SemanticActivationCommandV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<SemanticActivationReceiptV1, SemanticRuntimeBackendErrorV1>,
    > {
        Box::pin(async move {
            self.bind_configuration(command.configuration.clone());
            let Some(current) = self.handle.current() else {
                return Err(SemanticRuntimeBackendErrorV1::Unavailable);
            };
            if current.generation != command.request.target_generation {
                return Err(SemanticRuntimeBackendErrorV1::Rejected);
            }
            SemanticActivationReceiptV1::issue(command, now_micros())
                .map_err(|_| SemanticRuntimeBackendErrorV1::Rejected)
        })
    }

    fn rollback<'a>(
        &'a self,
        command: &'a SemanticRollbackCommandV1,
    ) -> SemanticRuntimeFuture<
        'a,
        Result<SemanticRollbackReceiptV1, SemanticRuntimeBackendErrorV1>,
    > {
        Box::pin(async move {
            let _ = command;
            Err(SemanticRuntimeBackendErrorV1::Unavailable)
        })
    }
}

fn index_state_from_status(status: SemanticRuntimeScheduleStatusV1) -> SemanticIndexStateV1 {
    match status {
        SemanticRuntimeScheduleStatusV1::Unavailable => SemanticIndexStateV1::Unavailable,
        SemanticRuntimeScheduleStatusV1::Indexing { .. } => SemanticIndexStateV1::Indexing,
        SemanticRuntimeScheduleStatusV1::Failed { .. } => SemanticIndexStateV1::Failed,
        SemanticRuntimeScheduleStatusV1::Current { .. } => SemanticIndexStateV1::Incompatible,
    }
}

fn provisional_vector_generation(source: &CodeGenerationId) -> VectorGenerationIdV1 {
    let digest = canonical_sha256(&("semantic.indexing.target", source))
        .unwrap_or_else(|_| ManifestDigest::new(format!("sha256:{}", "0".repeat(64))).expect("digest"));
    VectorGenerationIdV1::new(digest)
}

fn synthesize_current_receipt(
    configuration: &SemanticConfigurationPinV1,
    generation: &VectorGenerationIdV1,
) -> Option<SemanticActivationReceiptV1> {
    let request = SemanticActivationRequestV1::new(generation.clone(), None, None).ok()?;
    let command = SemanticActivationCommandV1::new(configuration.clone(), request).ok()?;
    SemanticActivationReceiptV1::issue(&command, now_micros()).ok()
}

fn now_micros() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_micros() as i64)
        .unwrap_or(0);
    UtcMicros(micros)
}

/// Process-local registry so Doctor/`tracedecay_runtime` can observe the
/// daemon-private scheduler without a wire operation.
fn project_semantic_handles() -> &'static Mutex<BTreeMap<PathBuf, DaemonSemanticRuntimeHandleV1>> {
    static HANDLES: OnceLock<Mutex<BTreeMap<PathBuf, DaemonSemanticRuntimeHandleV1>>> =
        OnceLock::new();
    HANDLES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Retain a project semantic handle for status/search composition.
pub fn register_project_semantic_runtime(
    project_root: PathBuf,
    handle: DaemonSemanticRuntimeHandleV1,
) {
    project_semantic_handles()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(project_root, handle);
}

/// Drop a retained project semantic handle.
pub fn unregister_project_semantic_runtime(project_root: &Path) {
    project_semantic_handles()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(project_root);
}

/// Application status for a mounted project semantic scheduler, if any.
pub fn project_semantic_application_status(
    project_root: &Path,
) -> Option<SemanticRuntimeStatusV1> {
    let handle = project_semantic_handles()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(project_root)
        .cloned()?;
    Some(application_status_from_projection(
        &handle.status_projection(),
        None,
    ))
}

/// Hook invoked after a code generation publishes; must not block search.
pub type SavedCodeGenerationScheduleHookV1 =
    Arc<dyn Fn(&CodeIndexPublishedGenerationV1) -> bool + Send + Sync>;

/// Build a non-blocking schedule hook bound to one daemon semantic handle.
pub fn saved_generation_schedule_hook<LoadArtifact, StageProjection, StageFuture>(
    handle: DaemonSemanticRuntimeHandleV1,
    load_artifact: LoadArtifact,
    stage_projection: StageProjection,
) -> SavedCodeGenerationScheduleHookV1
where
    LoadArtifact: Fn() -> Result<LoadedSemanticArtifactV1, SemanticRuntimeScheduleFailureV1>
        + Send
        + Sync
        + 'static,
    StageProjection: Fn(PreparedVectorGenerationV1) -> StageFuture + Send + Sync + 'static,
    StageFuture: Future<
            Output = Result<PreparedSemanticRuntimeCommitV1, SemanticRuntimeScheduleFailureV1>,
        > + Send
        + 'static,
{
    let load_artifact = Arc::new(load_artifact);
    let stage_projection = Arc::new(stage_projection);
    Arc::new(move |generation| {
        let load = Arc::clone(&load_artifact);
        let stage = Arc::clone(&stage_projection);
        schedule_saved_code_generation(
            &handle,
            generation,
            move || load(),
            move |prepared| stage(prepared),
        )
    })
}

/// Production hook: enqueue semantic projection for each saved generation.
///
/// Artifact admission remains owned by the model lifecycle. Until a complete
/// compatible artifact is available the background task fails closed without
/// joining into exact/lexical/graph search.
pub fn production_saved_generation_schedule_hook(
    handle: DaemonSemanticRuntimeHandleV1,
) -> SavedCodeGenerationScheduleHookV1 {
    saved_generation_schedule_hook(
        handle,
        || Err(SemanticRuntimeScheduleFailureV1::Artifact),
        |_prepared| async move { Err(SemanticRuntimeScheduleFailureV1::Publication) },
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    use tokio::sync::oneshot;
    use tracedecay_domain::{
        ChangedCodeChunkSetV1, CodeGenerationId, CodeSearchChunkV1, ManifestDigest,
        ProjectionBatchRequestV1, ProjectionReplayReasonV1, VectorGenerationIdV1,
    };

    use crate::semantic_code::{
        DaemonSemanticRuntimeHandleV1, FastEmbedSemanticGenerationRequestV1,
        PreparedSemanticRuntimeCommitV1, SemanticGenerationPointerV1,
        SemanticRuntimeScheduleFailureV1, SemanticRuntimeScheduleStatusV1, SemanticRuntimeWorkV1,
    };

    use super::*;

    fn source_generation(value: char) -> CodeGenerationId {
        CodeGenerationId::new(format!("code-generation.{value}")).expect("source generation")
    }

    fn vector_generation(value: char) -> VectorGenerationIdV1 {
        VectorGenerationIdV1::new(
            ManifestDigest::new(format!("sha256:{}", value.to_string().repeat(64)))
                .expect("manifest digest"),
        )
    }

    fn pointer(vector: char, source: char) -> SemanticGenerationPointerV1 {
        let authority = crate::semantic_code::session_pool::tests::authority();
        SemanticGenerationPointerV1 {
            generation: vector_generation(vector),
            source_generation: source_generation(source),
            projection_key: authority.projection().projection_key().clone(),
        }
    }

    fn projection_request(source: char) -> ProjectionBatchRequestV1 {
        ProjectionBatchRequestV1 {
            request_digest: ManifestDigest::new(format!("sha256:{}", "c".repeat(64)))
                .expect("request digest"),
            changes: ChangedCodeChunkSetV1 {
                from_generation: None,
                to_generation: source_generation(source),
                manifest_digest: ManifestDigest::new(format!("sha256:{}", "d".repeat(64)))
                    .expect("source manifest"),
                added_or_changed: Vec::new(),
                deleted: Vec::new(),
                reused: Vec::new(),
            },
            previous_projection_key: None,
            target_projection_key: crate::semantic_code::session_pool::tests::authority()
                .projection()
                .projection_key()
                .clone(),
            replay_reason: ProjectionReplayReasonV1::SourceEdit,
        }
    }

    #[tokio::test]
    async fn saved_edit_schedules_fastembed_without_blocking_exact_search() {
        let handle =
            DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("semantic handle");
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let exact_ready = AtomicBool::new(false);

        let request = FastEmbedSemanticGenerationRequestV1::new(
            source_generation('a'),
            projection_request('a'),
            Vec::<CodeSearchChunkV1>::new(),
            move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
                Err(SemanticRuntimeScheduleFailureV1::Projection)
            },
            move |_| async move { Err(SemanticRuntimeScheduleFailureV1::Publication) },
        )
        .expect("saved generation request");
        assert!(handle.schedule_generation(request));
        started_rx.await.expect("background schedule started");

        // Ordinary exact search proceeds while FastEmbed work is parked.
        exact_ready.store(true, Ordering::SeqCst);
        assert!(exact_ready.load(Ordering::SeqCst));
        assert!(matches!(
            handle.status(),
            SemanticRuntimeScheduleStatusV1::Indexing { .. }
        ));
        release_tx.send(()).expect("release artifact loader");
    }

    #[tokio::test]
    async fn runtime_reports_semantic_indexing_progress() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('a'),
            4,
            move |progress| async move {
                progress.set_completed_units(2);
                let _ = started_tx.send(());
                let _ = release_rx.await;
                Err(SemanticRuntimeScheduleFailureV1::Projection)
            },
        ));
        started_rx.await.expect("indexing started");
        let projection = handle.status_projection();
        let status = application_status_from_projection(&projection, None);
        match status.state {
            SemanticRuntimeStateV1::Indexing {
                completed_units,
                total_units,
                ..
            } => {
                assert_eq!(completed_units, 2);
                assert_eq!(total_units, 4);
            }
            other => panic!("expected indexing status, got {other:?}"),
        }
        let _ = release_tx.send(());
    }

    #[tokio::test]
    async fn runtime_reports_degraded_reason_and_prior_generation() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let prior_pointer = pointer('a', 'a');
        let prior = prior_pointer.generation.clone();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('a'),
            1,
            move |_progress| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    Ok(prior_pointer)
                }))
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while handle.current().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("prior generation published");

        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('b'),
            1,
            move |_progress| async move { Err(SemanticRuntimeScheduleFailureV1::Artifact) },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if matches!(
                    handle.status(),
                    SemanticRuntimeScheduleStatusV1::Failed { .. }
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("failure observed");

        let projection = handle.status_projection();
        assert_eq!(
            projection.degraded_reason,
            Some(SemanticFallbackReasonV1::ArtifactUnavailable)
        );
        assert_eq!(projection.prior_generation.as_ref(), Some(&prior));
        let status = application_status_from_projection(&projection, None);
        match status.state {
            SemanticRuntimeStateV1::Degraded {
                active_generation,
                reason,
            } => {
                assert_eq!(active_generation.as_ref(), Some(&prior));
                assert_eq!(reason, SemanticFallbackReasonV1::ArtifactUnavailable);
            }
            other => panic!("expected degraded status, got {other:?}"),
        }
        // Prior generation remains queryable / current for compatible reads.
        assert_eq!(handle.current().map(|pointer| pointer.generation), Some(prior));
    }

    #[tokio::test]
    async fn atomically_current_generation_enables_semantic_lane() {
        let handle = DaemonSemanticRuntimeHandleV1::new(1, 8, 1 << 20).expect("handle");
        let published = pointer('c', 'c');
        let source = published.source_generation.clone();
        let vector = published.generation.clone();
        let projection_key = published.projection_key.clone();
        handle.schedule(SemanticRuntimeWorkV1::new(
            source_generation('c'),
            1,
            move |_progress| async move {
                Ok(PreparedSemanticRuntimeCommitV1::new(move || async move {
                    Ok(published)
                }))
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while handle.current().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("current generation published");

        assert!(
            handle
                .query_factory(&source, &vector, &projection_key)
                .is_some(),
            "atomically current generation must enable query_factory"
        );
        assert!(
            handle
                .query_factory(&source_generation('x'), &vector, &projection_key)
                .is_none(),
            "incompatible source must not enable semantics"
        );
        let backend = DaemonSemanticRuntimeBackendV1::new(handle.clone());
        let status = backend.application_status();
        assert!(matches!(
            status.route(),
            crate::application::semantic_runtime::SemanticRuntimeRouteV1::LexicalFallback { .. }
        ));
    }
}

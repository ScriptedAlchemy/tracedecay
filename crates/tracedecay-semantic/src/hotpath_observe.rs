//! Opt-in hotpath labels for the semantic crate.
//!
//! Every `hotpath::*` macro expands to a no-op unless the `hotpath` feature is
//! selected. Recorded values are model-lifecycle state names, failure classes,
//! and numeric batch/count gauges only. Query text, model identifiers, artifact
//! digests, paths, and error detail strings must never enter a label or value.
//!
//! This crate has no reqwest 0.12 client. Acquisition uses hf-hub/ureq and an
//! explicit HTTPS transport trait, so `hotpath::http!` stays unused; download
//! and decode are timed as separate `measure_block!` labels instead.

use tracedecay_query::retrieval::rerank::LocalRerankFailureV1;

#[cfg(any(feature = "hotpath", test))]
use crate::artifact_store::ArtifactImportErrorV1;
use crate::artifact_store::SemanticCapabilityDisabledV1;
#[cfg(any(
    feature = "hotpath",
    feature = "semantic-fastembed",
    feature = "semantic-model2vec",
    test
))]
use crate::fastembed_adapter::EmbedError;
#[cfg(any(feature = "hotpath", test))]
use crate::fastembed_adapter::RuntimeFailureKindV1;
use crate::model_lifecycle::ModelLifecycleErrorV1;
use crate::session_pool::SessionAcquireError;
use tracedecay_semantic_contracts::SemanticModelLifecycleStateV1;

// Gated to match `record_lifecycle_state`, its only caller, which is itself
// compiled only when profiling is on.
#[cfg(feature = "hotpath")]
pub(crate) fn lifecycle_state_name(state: &SemanticModelLifecycleStateV1) -> &'static str {
    match state {
        SemanticModelLifecycleStateV1::SelectedNotDownloaded { .. } => "selected_not_downloaded",
        SemanticModelLifecycleStateV1::Downloading { .. } => "downloading",
        SemanticModelLifecycleStateV1::Verifying { .. } => "verifying",
        SemanticModelLifecycleStateV1::Installed { .. } => "installed",
        SemanticModelLifecycleStateV1::Loading { .. } => "loading",
        SemanticModelLifecycleStateV1::Indexing { .. } => "indexing",
        SemanticModelLifecycleStateV1::Ready { .. } => "ready",
        SemanticModelLifecycleStateV1::Failed {
            retryable: true, ..
        } => "failed_retryable",
        SemanticModelLifecycleStateV1::Failed {
            retryable: false, ..
        } => "failed",
    }
}

#[cfg(any(feature = "hotpath", test))]
pub(crate) fn lifecycle_error_class(error: &ModelLifecycleErrorV1) -> &'static str {
    match error {
        ModelLifecycleErrorV1::Catalog(_) => "catalog",
        ModelLifecycleErrorV1::StoreUnavailable => "store_unavailable",
        ModelLifecycleErrorV1::Rejected => "rejected",
        ModelLifecycleErrorV1::DownloadFailed
        | ModelLifecycleErrorV1::DownloadFailedWithReason(_) => "download_failed",
        ModelLifecycleErrorV1::VerificationFailed => "verification_failed",
        ModelLifecycleErrorV1::RerankerUnavailable => "reranker_unavailable",
        ModelLifecycleErrorV1::InstallFailed => "install_failed",
        ModelLifecycleErrorV1::WorkerJoinFailed => "worker_join_failed",
        ModelLifecycleErrorV1::Cancelled => "cancelled",
        ModelLifecycleErrorV1::CancellationCleanupQuarantined(_) => "cancellation_quarantined",
        ModelLifecycleErrorV1::CancellationCleanupFailed(_) => "cancellation_cleanup_failed",
        ModelLifecycleErrorV1::ArtifactImport(inner) => import_error_class(inner),
    }
}

#[cfg(any(feature = "hotpath", test))]
pub(crate) fn embed_error_class(error: &EmbedError) -> &'static str {
    match error {
        EmbedError::Cancelled => "cancelled",
        EmbedError::DeadlineExceeded => "deadline_exceeded",
        EmbedError::EmptyBatch => "empty_batch",
        EmbedError::TooManyTexts { .. } => "too_many_texts",
        EmbedError::BatchBytesExceeded { .. } => "batch_bytes_exceeded",
        EmbedError::DimensionMismatch { .. } => "dimension_mismatch",
        EmbedError::NonFiniteVectorValue => "non_finite_vector",
        EmbedError::Runtime(failure) => runtime_failure_class(failure.kind),
    }
}

#[cfg(any(feature = "hotpath", test))]
pub(crate) fn runtime_failure_class(kind: RuntimeFailureKindV1) -> &'static str {
    match kind {
        RuntimeFailureKindV1::LoadFailed => "load_failed",
        RuntimeFailureKindV1::OutOfMemory => "out_of_memory",
        RuntimeFailureKindV1::CorruptArtifact => "corrupt_artifact",
        RuntimeFailureKindV1::RevokedArtifact => "revoked_artifact",
        RuntimeFailureKindV1::IncompatibleRuntime => "incompatible_runtime",
        RuntimeFailureKindV1::EmbedFailed => "embed_failed",
    }
}

#[cfg(any(feature = "hotpath", test))]
pub(crate) fn session_acquire_error_class(error: &SessionAcquireError) -> &'static str {
    match error {
        SessionAcquireError::Exhausted { .. } => "exhausted",
        SessionAcquireError::QueueFull { .. } => "queue_full",
        SessionAcquireError::MemoryCeilingExceeded { .. } => "memory_ceiling",
        SessionAcquireError::Cancelled => "cancelled",
        SessionAcquireError::DeadlineExceeded { .. } => "deadline_exceeded",
        SessionAcquireError::LoadDeadlineExceeded { .. } => "load_deadline_exceeded",
        SessionAcquireError::ResidentCeilingExceeded { .. } => "resident_ceiling_exceeded",
        SessionAcquireError::Open(inner) => embed_error_class(inner),
        SessionAcquireError::Closed => "closed",
    }
}

#[cfg(any(feature = "hotpath", test))]
pub(crate) fn capability_disabled_class(error: &SemanticCapabilityDisabledV1) -> &'static str {
    match error {
        SemanticCapabilityDisabledV1::MissingArtifact => "missing_artifact",
        SemanticCapabilityDisabledV1::CorruptArtifact => "corrupt_artifact",
        SemanticCapabilityDisabledV1::RevokedArtifact => "revoked_artifact",
        SemanticCapabilityDisabledV1::QuarantinedArtifact => "quarantined_artifact",
        SemanticCapabilityDisabledV1::IncompatibleRuntime => "incompatible_runtime",
        SemanticCapabilityDisabledV1::IncompatiblePlatform => "incompatible_platform",
        SemanticCapabilityDisabledV1::ResourceCeilingExceeded => "resource_ceiling",
        SemanticCapabilityDisabledV1::LeaseUnavailable => "lease_unavailable",
        SemanticCapabilityDisabledV1::IdentityMismatch => "identity_mismatch",
        SemanticCapabilityDisabledV1::StorageFailure => "storage_failure",
    }
}

#[cfg(any(feature = "hotpath", test))]
pub(crate) fn import_error_class(error: &ArtifactImportErrorV1) -> &'static str {
    match error {
        ArtifactImportErrorV1::ManifestRejected => "manifest_rejected",
        ArtifactImportErrorV1::SizeExpansionBeyondDeclared => "size_expansion",
        ArtifactImportErrorV1::LengthMismatch => "length_mismatch",
        ArtifactImportErrorV1::DigestMismatch => "digest_mismatch",
        ArtifactImportErrorV1::MemberMismatch => "member_mismatch",
        ArtifactImportErrorV1::UnsafePackageEntry => "unsafe_package_entry",
        ArtifactImportErrorV1::UndeclaredMember => "undeclared_member",
        ArtifactImportErrorV1::InvalidHttpsSource => "invalid_https_source",
        ArtifactImportErrorV1::ImmutableRangeMismatch => "immutable_range_mismatch",
        ArtifactImportErrorV1::InterruptedResumable { .. } => "interrupted_resumable",
        ArtifactImportErrorV1::SourceInterrupted => "source_interrupted",
        ArtifactImportErrorV1::StagingUnavailable => "staging_unavailable",
        ArtifactImportErrorV1::ResumeIdentityMismatch => "resume_identity_mismatch",
        ArtifactImportErrorV1::UnsafeStagingHandle => "unsafe_staging_handle",
        ArtifactImportErrorV1::UnsafeStorePath => "unsafe_store_path",
        ArtifactImportErrorV1::StoreBusy => "store_busy",
        ArtifactImportErrorV1::LeaseConflict => "lease_conflict",
        ArtifactImportErrorV1::StorageFailure => "storage_failure",
    }
}

#[cfg(any(feature = "hotpath", test))]
pub(crate) fn rerank_failure_class(error: &LocalRerankFailureV1) -> &'static str {
    match error {
        LocalRerankFailureV1::Unavailable(_) => "unavailable",
        LocalRerankFailureV1::Rejected(_) => "rejected",
        LocalRerankFailureV1::TimedOut => "timed_out",
        LocalRerankFailureV1::Cancelled => "cancelled",
    }
}

#[inline(always)]
pub(crate) fn record_model_state(name: &'static str) {
    #[cfg(feature = "hotpath")]
    hotpath::val!("semantic_model_state").set(&name);
    #[cfg(not(feature = "hotpath"))]
    let _ = name;
}

#[inline(always)]
pub(crate) fn record_model_failure(class: &'static str) {
    #[cfg(feature = "hotpath")]
    hotpath::val!("semantic_model_failure").set(&class);
    #[cfg(not(feature = "hotpath"))]
    let _ = class;
}

#[inline(always)]
pub(crate) fn record_remote_failure(class: &'static str) {
    #[cfg(feature = "hotpath")]
    hotpath::val!("semantic_remote_failure").set(&class);
    #[cfg(not(feature = "hotpath"))]
    let _ = class;
}

#[inline(always)]
pub(crate) fn record_session_acquire(kind: &'static str) {
    #[cfg(feature = "hotpath")]
    hotpath::val!("semantic_session_acquire").set(&kind);
    #[cfg(not(feature = "hotpath"))]
    let _ = kind;
}

#[inline(always)]
#[cfg(feature = "hotpath")]
pub(crate) fn record_session_failure(class: &'static str) {
    hotpath::val!("semantic_session_failure").set(&class);
}

#[inline(always)]
pub(crate) fn record_artifact_cache(hit: bool) {
    #[cfg(feature = "hotpath")]
    if hit {
        hotpath::gauge!("semantic_artifact_cache_hit").inc(1);
    } else {
        hotpath::gauge!("semantic_artifact_cache_miss").inc(1);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = hit;
}

#[inline(always)]
pub(crate) fn record_lifecycle_state(state: &SemanticModelLifecycleStateV1) {
    #[cfg(feature = "hotpath")]
    record_model_state(lifecycle_state_name(state));
    #[cfg(not(feature = "hotpath"))]
    let _ = state;
}

#[inline(always)]
pub(crate) fn record_lifecycle_error(error: &ModelLifecycleErrorV1) {
    #[cfg(feature = "hotpath")]
    record_model_failure(lifecycle_error_class(error));
    #[cfg(not(feature = "hotpath"))]
    let _ = error;
}

/// Every call site lives in an embedding adapter, compiled only when that
/// adapter's backend feature is selected.
#[cfg(any(feature = "semantic-fastembed", feature = "semantic-model2vec"))]
#[inline(always)]
pub(crate) fn record_embed_error(error: &EmbedError) {
    #[cfg(feature = "hotpath")]
    record_model_failure(embed_error_class(error));
    #[cfg(not(feature = "hotpath"))]
    let _ = error;
}

#[inline(always)]
pub(crate) fn record_session_error(error: &SessionAcquireError) {
    #[cfg(feature = "hotpath")]
    record_session_failure(session_acquire_error_class(error));
    #[cfg(not(feature = "hotpath"))]
    let _ = error;
}

#[inline(always)]
pub(crate) fn record_capability_error(error: &SemanticCapabilityDisabledV1) {
    #[cfg(feature = "hotpath")]
    record_model_failure(capability_disabled_class(error));
    #[cfg(not(feature = "hotpath"))]
    let _ = error;
}

#[inline(always)]
pub(crate) fn record_rerank_error(error: &LocalRerankFailureV1) {
    #[cfg(feature = "hotpath")]
    record_model_failure(rerank_failure_class(error));
    #[cfg(not(feature = "hotpath"))]
    let _ = error;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fastembed_adapter::RuntimeFailureV1;

    #[test]
    fn classifiers_emit_static_classes_without_payloads() {
        assert_eq!(
            embed_error_class(&EmbedError::Runtime(RuntimeFailureV1 {
                kind: RuntimeFailureKindV1::OutOfMemory,
                detail: "must-not-appear-in-the-class".to_owned(),
            })),
            "out_of_memory"
        );
        assert_eq!(
            lifecycle_error_class(&ModelLifecycleErrorV1::DownloadFailedWithReason(
                "member 'pytorch_model.onnx' is absent".to_owned(),
            )),
            "download_failed"
        );
        assert_eq!(
            import_error_class(&ArtifactImportErrorV1::InterruptedResumable {
                staging_id: "staging-must-not-leak".to_owned(),
            }),
            "interrupted_resumable"
        );
        assert_eq!(
            session_acquire_error_class(&SessionAcquireError::Exhausted { active: 3, max: 3 }),
            "exhausted"
        );
        assert_eq!(
            capability_disabled_class(&SemanticCapabilityDisabledV1::MissingArtifact),
            "missing_artifact"
        );
        assert_eq!(
            rerank_failure_class(&LocalRerankFailureV1::Cancelled),
            "cancelled"
        );
    }
}

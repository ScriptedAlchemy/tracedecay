use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use tracedecay::{
    application::code_index::open_production_code_index_owner_v1,
    code_index::{
        production::{
            CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
            CodeIndexExecutionControlV1, CodeIndexInterruptionV1, CodeIndexProductionConfigV1,
            CodeIndexProductionErrorV1, CodeIndexPublicationStoreErrorV1,
            CodeIndexPublishedGenerationV1,
        },
        projection::{
            ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionSinkErrorV1,
            build_batch_receipt,
        },
        provider::GenerationTestAttributionJoinReadPort,
    },
};
use tracedecay_domain::{
    ChunkerRevision, CodeGenerationId, FileOccurrenceId, LanguageId, ManifestDigest,
    PolicyRevisionId, PrivacyDomainId, ProjectionBatchReceiptV1, ProjectionBatchRequestV1,
    ProjectionKeyV1, ProjectionKindV1, ProjectionOperationV1, ProjectionOutcomeV1,
    ProviderEvaluationStateV1, RepositoryId, SanitizationReceiptId, SanitizedCodeFileV1,
    SanitizedCodeSnapshotV1, SanitizerRevision, SnapshotFileDispositionV1,
    TestAttributionEvidenceClassV1, UtcMicros,
};

use crate::support::{RUST_SOURCE, id};

#[derive(Clone, Default)]
struct SharedPublicationStore {
    active: Arc<Mutex<Option<CodeIndexPublishedGenerationV1>>>,
}

impl CodeIndexAtomicPublicationPort for SharedPublicationStore {
    fn load_active(
        &self,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        Ok(self.active.lock().expect("publication lock").clone())
    }

    fn publish_atomically(
        &mut self,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: CodeIndexPublishedGenerationV1,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut active = self.active.lock().expect("publication lock");
        if active
            .as_ref()
            .map(|current| current.manifest().generation_id.clone())
            .as_ref()
            != expected_active_generation
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        *active = Some(generation);
        Ok(())
    }
}

#[derive(Default)]
struct ApplyingProjectionSink;

impl CodeChunkProjectionSink for ApplyingProjectionSink {
    fn project_changed_chunks(
        &mut self,
        request: ProjectionBatchRequestV1,
    ) -> Result<ProjectionBatchReceiptV1, ProjectionSinkErrorV1> {
        let decisions: Vec<ChunkProjectionDecisionV1> = request
            .changes
            .added_or_changed
            .iter()
            .map(|change| ChunkProjectionDecisionV1 {
                chunk_id: change.chunk_id.clone(),
                prior_chunk_digest: change.prior_digest.clone(),
                current_chunk_digest: change.current_digest.clone(),
                operation: if change.prior_digest.is_some() {
                    ProjectionOperationV1::Updated
                } else {
                    ProjectionOperationV1::Added
                },
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: Some(
                    change
                        .current_digest
                        .clone()
                        .expect("added or changed chunks have a current digest"),
                ),
            })
            .collect();
        build_batch_receipt(&request, &decisions)
            .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
    }
}

struct RejectingProjectionSink;

impl CodeChunkProjectionSink for RejectingProjectionSink {
    fn project_changed_chunks(
        &mut self,
        _request: ProjectionBatchRequestV1,
    ) -> Result<ProjectionBatchReceiptV1, ProjectionSinkErrorV1> {
        Err(ProjectionSinkErrorV1::Rejected(
            "projection is intentionally unavailable".to_owned(),
        ))
    }
}

struct ActiveControl;

impl CodeIndexExecutionControlV1 for ActiveControl {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

struct CancelledControl;

impl CodeIndexExecutionControlV1 for CancelledControl {
    fn is_cancelled(&self) -> bool {
        true
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

struct ExpiredControl;

impl CodeIndexExecutionControlV1 for ExpiredControl {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        true
    }
}

fn config() -> CodeIndexProductionConfigV1 {
    CodeIndexProductionConfigV1 {
        repository: id::<RepositoryId>("repository.production"),
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
        policy_revision: id::<PolicyRevisionId>("policy.v1"),
        chunker_revision: id::<ChunkerRevision>("chunker.v1"),
        privacy_domain: id::<PrivacyDomainId>("privacy.production"),
        privacy_key_epoch: 7,
        max_snapshot_age_micros: None,
    }
}

fn projection_key() -> ProjectionKeyV1 {
    ProjectionKeyV1 {
        kind: ProjectionKindV1::Lexical,
        schema_revision: "lexical.v1".to_owned(),
        profile_digest: id::<ManifestDigest>(&format!("sha256:{}", "e".repeat(64))),
    }
}

fn request(file_occurrence: &str, sealed_at: i64) -> CodeIndexBuildRequestV1 {
    request_at_path(file_occurrence, "src/lib.rs", sealed_at)
}

fn request_at_path(
    file_occurrence: &str,
    logical_path: &str,
    sealed_at: i64,
) -> CodeIndexBuildRequestV1 {
    let source = RUST_SOURCE.as_bytes();
    let file = SanitizedCodeFileV1 {
        file_occurrence_id: id::<FileOccurrenceId>(file_occurrence),
        logical_path: logical_path.to_owned(),
        language: Some(id::<LanguageId>("rust")),
        content_digest: tracedecay::code_index::chunks::content_digest(source),
        disposition: SnapshotFileDispositionV1::Present,
    };
    let snapshot = SanitizedCodeSnapshotV1 {
        repository: id::<RepositoryId>("repository.production"),
        worktree: None,
        reference: None,
        source_revision: None,
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
        sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.production")],
        content_identity: tracedecay::code_index::chunks::content_digest(source),
        captured_at: UtcMicros(1_000_000),
        files: vec![file.clone()],
    };

    CodeIndexBuildRequestV1 {
        snapshot,
        captured_files: vec![CodeIndexCapturedFileV1 {
            file_occurrence_id: file.file_occurrence_id,
            sanitized_bytes: source.to_vec(),
            sensitivity_level: tracedecay_domain::SensitivityLevelV1::Public,
        }],
        changed_files: BTreeSet::new(),
        invalidations: BTreeSet::new(),
        sealed_at: UtcMicros(sealed_at),
        target_projection_key: projection_key(),
    }
}

#[test]
fn published_generation_serves_current_conservative_test_attribution() {
    let store = SharedPublicationStore::default();
    let mut owner = open_production_code_index_owner_v1(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let generation = owner
        .build_and_publish(
            request_at_path("file.production.test", "tests/production.rs", 1_100_000),
            &ActiveControl,
        )
        .expect("test generation publishes");
    let authority = generation
        .test_attribution_authority()
        .expect("attribution authority");

    let read = authority.read_test_attribution(&generation.manifest().generation_id);

    assert!(matches!(
        read.provider_state,
        ProviderEvaluationStateV1::SupportedCompletedComplete | ProviderEvaluationStateV1::Partial
    ));
    let join = read.evidence.expect("generation attribution");
    assert_eq!(join.generation_id, generation.manifest().generation_id);
    assert_eq!(
        join.test_watermark.snapshot_digest,
        generation.manifest().snapshot_digest
    );
    assert!(!join.records.is_empty());
    assert!(join.records.iter().all(|record| {
        record.attribution.evidence_class
            == TestAttributionEvidenceClassV1::ConservativeDependencyCandidates
    }));
}

#[test]
fn production_owner_publishes_complete_generation_and_restores_it_after_restart() {
    let store = SharedPublicationStore::default();
    let mut owner =
        open_production_code_index_owner_v1(config(), store.clone(), ApplyingProjectionSink)
            .expect("production owner");

    let first = owner
        .build_and_publish(request("file.production.1", 1_100_000), &ActiveControl)
        .expect("first generation publishes");

    assert_eq!(first.coverage().files_eligible, 1);
    assert!(first.edges().len() + first.edge_abstentions().len() > 0);
    assert!(
        !first
            .admitted_chunks()
            .expect("parser-backed exact authority")
            .is_empty()
    );
    assert_eq!(
        first.projection().receipt().source_generation,
        first.manifest().generation_id
    );

    let mut restarted =
        open_production_code_index_owner_v1(config(), store.clone(), ApplyingProjectionSink)
            .expect("restart owner");
    let restored = restarted
        .active_generation()
        .expect("active generation loads")
        .expect("published generation survives restart");
    assert_eq!(restored.manifest(), first.manifest());

    let second = restarted
        .build_and_publish(request("file.production.2", 1_200_000), &ActiveControl)
        .expect("unchanged source carries forward");
    assert_eq!(
        second.manifest().parent_generation,
        Some(first.manifest().generation_id.clone())
    );
    assert!(
        second
            .projection()
            .request()
            .changes
            .added_or_changed
            .is_empty()
    );
    assert!(second.projection().request().changes.deleted.is_empty());
    assert!(!second.projection().request().changes.reused.is_empty());
    assert!(
        !second
            .admitted_chunks()
            .expect("carry-forward retains parser-backed exact authority")
            .is_empty()
    );
}

#[test]
fn production_owner_abstains_without_publication_on_cancellation_or_deadline() {
    let store = SharedPublicationStore::default();
    let mut owner =
        open_production_code_index_owner_v1(config(), store.clone(), ApplyingProjectionSink)
            .expect("production owner");

    let cancelled = owner
        .build_and_publish(
            request("file.production.cancelled", 1_100_000),
            &CancelledControl,
        )
        .expect_err("cancelled run must not publish");
    assert!(matches!(
        cancelled,
        CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::Cancelled)
    ));
    assert!(
        store
            .load_active()
            .expect("read publication state")
            .is_none()
    );

    let expired = owner
        .build_and_publish(
            request("file.production.expired", 1_100_000),
            &ExpiredControl,
        )
        .expect_err("expired run must not publish");
    assert!(matches!(
        expired,
        CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::DeadlineExceeded)
    ));
    assert!(
        store
            .load_active()
            .expect("read publication state")
            .is_none()
    );
}

#[test]
fn production_owner_never_activates_a_generation_after_projection_failure() {
    let store = SharedPublicationStore::default();
    let mut owner =
        open_production_code_index_owner_v1(config(), store.clone(), RejectingProjectionSink)
            .expect("production owner");

    let error = owner
        .build_and_publish(
            request("file.production.rejected", 1_100_000),
            &ActiveControl,
        )
        .expect_err("rejected projection must not publish");
    assert!(matches!(error, CodeIndexProductionErrorV1::Projection(_)));
    assert!(
        store
            .load_active()
            .expect("read publication state")
            .is_none()
    );
}

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use tracedecay_code_index::{
    chunks::content_digest,
    production::{
        CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
        CodeIndexExecutionControlV1, CodeIndexGenerationScopeV1, CodeIndexInterruptionV1,
        CodeIndexProductionConfigV1, CodeIndexProductionErrorV1, CodeIndexProductionOwnerV1,
        CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
    },
    projection::{
        ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionSinkErrorV1,
        build_batch_receipt,
    },
    provider::GenerationTestAttributionJoinReadPort,
};
use tracedecay_domain::{
    BranchStackNodeV1, ChunkerRevision, CodeGenerationId, CommitId, FileOccurrenceId, LanguageId,
    ManifestDigest, PolicyRevisionId, PrivacyDomainId, ProjectId, ProjectionBatchReceiptV1,
    ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionKindV1, ProjectionOperationV1,
    ProjectionOutcomeV1, ProviderEvaluationStateV1, RefId, RepositoryId, SanitizationReceiptId,
    SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision, SnapshotFileDispositionV1,
    StackNodeId, TestAttributionEvidenceClassV1, UtcMicros, WorktreeId, canonical_sha256,
};

use crate::support::{RUST_SOURCE, id};

#[derive(Clone, Default)]
struct SharedPublicationStore {
    active: Arc<Mutex<BTreeMap<CodeIndexGenerationScopeV1, CodeIndexPublishedGenerationV1>>>,
}

impl SharedPublicationStore {
    fn scope_count(&self) -> usize {
        self.active.lock().expect("publication lock").len()
    }

    fn shares_repository_store(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.active, &other.active)
    }
}

impl CodeIndexAtomicPublicationPort for SharedPublicationStore {
    fn load_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        Ok(self
            .active
            .lock()
            .expect("publication lock")
            .get(scope)
            .cloned())
    }

    fn publish_atomically(
        &mut self,
        scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: CodeIndexPublishedGenerationV1,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut active = self.active.lock().expect("publication lock");
        if active
            .get(scope)
            .map(|current| current.manifest().generation_id.clone())
            .as_ref()
            != expected_active_generation
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        active.insert(scope.clone(), generation);
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
        project_id: id::<ProjectId>("project.production"),
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

fn request_in_scope(
    file_occurrence: &str,
    sealed_at: i64,
    reference: &str,
    worktree: Option<&str>,
    source_revision: &str,
) -> CodeIndexBuildRequestV1 {
    let mut request = request(file_occurrence, sealed_at);
    request.snapshot.reference = Some(id::<RefId>(reference));
    request.snapshot.worktree = worktree.map(id::<WorktreeId>);
    request.snapshot.source_revision = Some(id::<CommitId>(source_revision));
    request
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
        content_digest: content_digest(source),
        disposition: SnapshotFileDispositionV1::Present,
    };
    let snapshot = SanitizedCodeSnapshotV1 {
        repository: id::<RepositoryId>("repository.production"),
        worktree: None,
        reference: None,
        source_revision: None,
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
        sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.production")],
        content_identity: content_digest(source),
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
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
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
        CodeIndexProductionOwnerV1::new(config(), store.clone(), ApplyingProjectionSink)
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
        CodeIndexProductionOwnerV1::new(config(), store.clone(), ApplyingProjectionSink)
            .expect("restart owner");
    let restored = restarted
        .active_generation(&CodeIndexGenerationScopeV1::for_snapshot(
            &request("file.production.scope", 1_100_000).snapshot,
        ))
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
fn sealed_generation_validation_is_memoized_but_decode_stays_fail_closed() {
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let generation = owner
        .build_and_publish(request("file.project-binding", 1_200_000), &ActiveControl)
        .expect("valid generation publishes");
    let sealed = generation.encode_sealed().expect("valid generation seals");
    assert_eq!(
        generation
            .encode_sealed()
            .expect("memoized generation seals again"),
        sealed
    );

    let restored =
        CodeIndexPublishedGenerationV1::decode_sealed(&sealed).expect("valid generation restores");
    assert_eq!(restored.manifest().project_id, config().project_id);
    assert_eq!(
        restored
            .encode_sealed()
            .expect("restored generation retains successful validation"),
        sealed
    );

    let mut envelope: serde_json::Value =
        serde_json::from_slice(&sealed).expect("sealed generation JSON");
    envelope["generation"]["files"][0]["authority"]["project_id"] =
        serde_json::Value::String("project.foreign".to_owned());
    let state_digest =
        canonical_sha256(&envelope["generation"]).expect("forged payload has canonical digest");
    envelope["state_digest"] = serde_json::Value::String(state_digest.as_str().to_owned());
    let forged = serde_json::to_vec(&envelope).expect("forged sealed generation JSON");

    let error = CodeIndexPublishedGenerationV1::decode_sealed(&forged)
        .expect_err("foreign file authority must fail sealed restoration");
    assert!(
        error
            .to_string()
            .contains("file authority project does not match the generation manifest"),
        "unexpected project mismatch error: {error}"
    );
}

/// The published-generation integrity gate is an amortized load-time check.
/// Verifying once per loaded generation must reach exactly the verdict a fresh
/// verification reaches, and must stay fail-closed for a generation that has
/// never validated.
#[test]
fn published_generation_validation_is_amortized_per_loaded_generation() {
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let generation = owner
        .build_and_publish(request("file.validation.memo", 1_300_000), &ActiveControl)
        .expect("valid generation publishes");

    // Publication already ran the full gate, so the generation carries a
    // verified mark and later seals reuse it instead of re-verifying.
    assert!(
        generation.is_validated(),
        "publishing a generation must run its integrity gate"
    );
    let sealed = generation.encode_sealed().expect("valid generation seals");
    let resealed = generation
        .encode_sealed()
        .expect("an already-verified generation reseals");
    assert_eq!(
        sealed, resealed,
        "the memoized gate must reach the same verdict and payload as the first check"
    );

    // Restoring re-reads bytes from the sealed store, so it must verify fresh
    // rather than trust any carried mark.
    let restored =
        CodeIndexPublishedGenerationV1::decode_sealed(&sealed).expect("valid generation restores");
    assert!(
        restored.is_validated(),
        "a restored generation must be fully verified before it can serve"
    );
    assert_eq!(restored.manifest(), generation.manifest());
    assert_eq!(
        restored
            .encode_sealed()
            .expect("restored generation reseals"),
        sealed,
        "a restored generation must reseal to identical bytes"
    );

    // Repeat exact admission is memoized and must stay byte-identical.
    let first_admitted = restored
        .admitted_chunks()
        .expect("parser-backed exact authority");
    let second_admitted = restored
        .admitted_chunks()
        .expect("repeat exact admission is amortized");
    assert!(!first_admitted.is_empty());
    assert_eq!(first_admitted.len(), second_admitted.len());
    assert!(
        first_admitted
            .iter()
            .zip(&second_admitted)
            .all(|(first, second)| first.chunk() == second.chunk()),
        "amortized admission must return the same chunks as the first admission"
    );

    // Repeat attribution reads are memoized and must stay identical.
    let first_attribution = restored
        .test_attribution_authority()
        .expect("test attribution authority");
    let second_attribution = restored
        .test_attribution_authority()
        .expect("repeat attribution read is amortized");
    let generation_id = restored.manifest().generation_id.clone();
    assert_eq!(
        format!(
            "{:?}",
            first_attribution.read_test_attribution(&generation_id)
        ),
        format!(
            "{:?}",
            second_attribution.read_test_attribution(&generation_id)
        ),
        "amortized attribution must return the same evidence as the first read"
    );
}

/// Corruption of chunk evidence must still be caught by the very first
/// validation of a generation. Memoizing a verdict must never let a generation
/// that has not validated serve.
#[test]
fn corrupted_chunk_evidence_fails_the_first_validation_of_a_restored_generation() {
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let generation = owner
        .build_and_publish(
            request("file.validation.corrupt", 1_400_000),
            &ActiveControl,
        )
        .expect("valid generation publishes");
    let sealed = generation.encode_sealed().expect("valid generation seals");

    let mut envelope: serde_json::Value =
        serde_json::from_slice(&sealed).expect("sealed generation JSON");
    let chunk = &mut envelope["generation"]["files"][0]["artifacts"]["chunks"]["chunks"][0];
    assert!(
        !chunk.is_null(),
        "the fixture generation must contain at least one chunk"
    );
    // Break the chunk's canonical identity so it no longer matches the document
    // membership its file artifact claims.
    chunk["id"] = serde_json::Value::String("chunk.tampered".to_owned());
    // Re-seal the envelope so the outer state digest cannot be what rejects it.
    let state_digest =
        canonical_sha256(&envelope["generation"]).expect("forged payload has canonical digest");
    envelope["state_digest"] = serde_json::Value::String(state_digest.as_str().to_owned());
    let forged = serde_json::to_vec(&envelope).expect("forged sealed generation JSON");

    let error = CodeIndexPublishedGenerationV1::decode_sealed(&forged)
        .expect_err("corrupted chunk evidence must fail the first validation");
    let message = error.to_string();
    assert!(
        message.contains("chunk") || message.contains("digest") || message.contains("canonical"),
        "unexpected corrupted-chunk error: {message}"
    );
}

#[test]
fn linked_worktrees_share_one_repository_store_but_isolate_active_generations() {
    let store = SharedPublicationStore::default();
    let primary_store = store.clone();
    let linked_store = store.clone();
    assert!(primary_store.shares_repository_store(&linked_store));

    let mut owner =
        CodeIndexProductionOwnerV1::new(config(), store.clone(), ApplyingProjectionSink)
            .expect("production owner");
    let primary_request = request_in_scope(
        "file.primary.1",
        1_100_000,
        "refs/heads/main",
        None,
        "commit.main.1",
    );
    let primary_scope = CodeIndexGenerationScopeV1::for_snapshot(&primary_request.snapshot);
    let primary = owner
        .build_and_publish(primary_request, &ActiveControl)
        .expect("primary generation publishes");

    let linked_request = request_in_scope(
        "file.linked.1",
        1_200_000,
        "refs/heads/feature",
        Some("worktree.feature"),
        "commit.feature.1",
    );
    let linked_scope = CodeIndexGenerationScopeV1::for_snapshot(&linked_request.snapshot);
    let linked = owner
        .build_and_publish(linked_request, &ActiveControl)
        .expect("linked-worktree generation publishes");

    assert_ne!(primary_scope, linked_scope);
    assert_ne!(
        primary.manifest().generation_id,
        linked.manifest().generation_id
    );
    assert!(linked.manifest().parent_generation.is_none());
    assert_eq!(store.scope_count(), 2);
    assert_eq!(
        store
            .load_active(&primary_scope)
            .expect("primary scope read")
            .expect("primary remains active")
            .manifest(),
        primary.manifest()
    );
    assert_eq!(
        store
            .load_active(&linked_scope)
            .expect("linked scope read")
            .expect("linked remains active")
            .manifest(),
        linked.manifest()
    );
}

#[test]
fn linked_worktree_no_op_reuses_only_its_compatible_generation() {
    let store = SharedPublicationStore::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(config(), store.clone(), ApplyingProjectionSink)
            .expect("production owner");
    let first = owner
        .build_and_publish(
            request_in_scope(
                "file.linked.1",
                1_100_000,
                "refs/heads/feature",
                Some("worktree.feature"),
                "commit.feature.1",
            ),
            &ActiveControl,
        )
        .expect("first linked-worktree generation");
    let second = owner
        .build_and_publish(
            request_in_scope(
                "file.linked.2",
                1_200_000,
                "refs/heads/feature",
                Some("worktree.feature"),
                "commit.feature.1",
            ),
            &ActiveControl,
        )
        .expect("no-op linked-worktree generation");

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
    assert_eq!(store.scope_count(), 1);
}

#[test]
fn branch_stack_nodes_and_snapshots_derive_the_same_path_free_scope() {
    let request = request_in_scope(
        "file.branch-stack.1",
        1_100_000,
        "refs/heads/feature",
        Some("worktree.feature"),
        "commit.feature.1",
    );
    let node = BranchStackNodeV1 {
        node_id: id::<StackNodeId>("stack-node.feature"),
        project_id: id("project.fixture"),
        repository_id: request.snapshot.repository.clone(),
        reference: request
            .snapshot
            .reference
            .clone()
            .expect("branch reference"),
        tip: request
            .snapshot
            .source_revision
            .clone()
            .expect("branch tip"),
        worktree_id: request.snapshot.worktree.clone(),
    };

    assert_eq!(
        CodeIndexGenerationScopeV1::for_branch_stack_node(&node),
        CodeIndexGenerationScopeV1::for_snapshot(&request.snapshot)
    );
}

#[test]
fn production_owner_abstains_without_publication_on_cancellation_or_deadline() {
    let store = SharedPublicationStore::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(config(), store.clone(), ApplyingProjectionSink)
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
            .load_active(&CodeIndexGenerationScopeV1::for_snapshot(
                &request("file.production.cancelled.scope", 1_100_000).snapshot,
            ))
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
            .load_active(&CodeIndexGenerationScopeV1::for_snapshot(
                &request("file.production.expired.scope", 1_100_000).snapshot,
            ))
            .expect("read publication state")
            .is_none()
    );
}

#[test]
fn production_owner_never_activates_a_generation_after_projection_failure() {
    let store = SharedPublicationStore::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(config(), store.clone(), RejectingProjectionSink)
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
            .load_active(&CodeIndexGenerationScopeV1::for_snapshot(
                &request("file.production.rejected.scope", 1_100_000).snapshot,
            ))
            .expect("read publication state")
            .is_none()
    );
}

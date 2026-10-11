//! Configuration, fact, diagnostics, retrieval-anchor, external-source and
//! git-index payloads.

use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::configuration::{
    ConfigurationAuditEvent, ConfigurationAuditEventKindV1, ConfigurationRevisionId,
    ConfigurationSnapshotV1,
};
use tracedecay_domain::feedback::{
    FeedbackScopeV1, GitHubPullRequestIdV1, GitHubReviewReadOperationV1,
};
use tracedecay_domain::test_fixtures::{digest, id, repeated_sha256_text};
use tracedecay_domain::{
    AccessPolicyDigest, ActorId, CapabilityId, CommitId, ComponentVersion,
    DiagnosticEvidenceClassV1, DiagnosticProducerKindV1, DiagnosticProvenanceV1,
    DiagnosticRecordStateV1, DiagnosticSeverityV1, FactAssertionKindV1, FactAssertionV1,
    FactCategoryV1, FactId, FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1,
    FactLineageEventV1, FactOwnerV1, FactPayloadV1, GenerationDiagnosticV1, GitBlobExpectationV1,
    GitCoverageV1, GitHeadStateV1, GitIndexEntryExpectationV1, GitIndexIdempotencyKey,
    GitIndexPreviewDispositionV1, GitIndexPreviewId, GitIndexPreviewV1, GitIndexTransactionId,
    GitIndexTransactionJournalV1, GitIndexTransactionOperationV1, GitObjectFormatV1, GitOidV1,
    HunkDirectionV1, HunkRefV1, LocatorDigest, PrivacyDomainBoundLocatorDigest, PrivacyDomainId,
    ProjectId, ProvenanceId, ProviderId, RepositoryId, RepositoryIndexSnapshotV1,
    RepositoryIndexStateV1, RepositoryStateSnapshotV1, RepositoryWorkingTreeSnapshotV1,
    RepositoryWorkingTreeStateV1, ResolutionAuthorizationV1, RetentionClass, RetrievalAnchorId,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, ScopeResolutionId, SensitivityV1, SourceAcquisitionCapabilitiesV1,
    SourceAcquisitionContractV1, SourceAggregateFrontierV1, SourceBindingOwnerV1, SourceBindingV1,
    SourceCaptureModeV1, SourceContentStateV1, SourceCoverageV1, SourceCursorV1,
    SourceDefinitionV1, SourceDeletionSemanticsV1, SourceEventAdmissionDispositionV1,
    SourceEventAdmissionReceiptV1, SourceEventV1, SourceInstanceId, SourceNativeObjectIdV1,
    SourceObjectObservationV1, SourceObjectRevisionV1, SourcePartitionFrontierV1,
    SourcePartitionIdV1, SourceRefetchStrategyV1, SourceRefreshCauseV1, SourceRefreshReceiptV1,
    SourceSnapshotCompletionV1, SourceSnapshotIdV1, SourceSpan, UtcMicros, WorktreeId,
};
use tracedecay_store::{
    AnchorDerivativeKindV1, AnchorDispositionReasonClassV1, AnchorDispositionStateV1,
    ConfigurationCommitV1, ConfigurationMutationReceiptV1, ConfigurationRevisionRecordV1,
    FactWriteBatch, GitIndexTransactionRecordV1, RetrievalAnchorDerivativeV1,
    RetrievalAnchorDispositionRecordV1, SanitizedCleanDiagnosticSnapshotV1,
    SourceAcquisitionQueueCasV1, SourceAcquisitionQueueStateV1, SourceAcquisitionRequestV1,
    SourceCommitApplyOutcomeV1, SourceCommitV1, SourceObjectMutationV1, SourceObjectTransitionV1,
    SourceObservationEvidenceV1, SourcePendingProjectionV1, SourceProjectionCommitV1,
    SourceScheduledRefetchV1, SourceStoreStateV1, apply_source_commit, build_source_projection,
};

use super::PROJECT;

pub const CONFIG_ROOT_REVISION: &str = "configuration.revision.root";
pub const SOURCE_ANCHOR: &str = "retrieval.source.parity";

pub fn project_owner() -> FactOwnerV1 {
    FactOwnerV1::Project {
        project_id: ProjectId::new(PROJECT).unwrap(),
    }
}

pub fn empty_configuration_snapshot() -> ConfigurationSnapshotV1 {
    ConfigurationSnapshotV1::new(BTreeMap::new(), BTreeMap::new()).unwrap()
}

/// A child of the seeded root revision.
pub fn configuration(result_revision: &str, operation_digit: char) -> ConfigurationCommitV1 {
    let base: ConfigurationRevisionId = id(CONFIG_ROOT_REVISION);
    let next: ConfigurationRevisionId = id(result_revision);
    let actor: ActorId = id("actor.configuration.parity");
    let receipt = ConfigurationMutationReceiptV1 {
        receipt_id: id("configuration.receipt.parity"),
        actor_id: actor.clone(),
        idempotency_key: id("configuration.idempotency.parity"),
        base_revision_id: base.clone(),
        result_revision_id: next.clone(),
        operation_digest: digest(operation_digit),
        authorization_policy_epoch: 7,
        authorization_policy_digest: id::<AccessPolicyDigest>(&repeated_sha256_text('e')),
        authority_revalidated_at: UtcMicros(20),
        receipt_digest: digest('9'),
        created_at: UtcMicros(21),
        effective_deadline_at: UtcMicros(100),
    };
    ConfigurationCommitV1 {
        expected_base_revision_id: base.clone(),
        next_revision: ConfigurationRevisionRecordV1 {
            revision_id: next.clone(),
            parent_revision_id: Some(base.clone()),
            snapshot: empty_configuration_snapshot(),
            actor_id: actor.clone(),
            operation_kind: "direct_mutation".to_owned(),
            created_at: UtcMicros(20),
        },
        audit_event: ConfigurationAuditEvent {
            event_id: id("configuration.audit.parity"),
            event_kind: ConfigurationAuditEventKindV1::Applied,
            actor_id: actor,
            idempotency_key: Some(receipt.idempotency_key.clone()),
            base_revision_id: base,
            result_revision_id: Some(next),
            operation_digest: receipt.operation_digest.clone(),
            target_commitment: digest('8'),
            receipt_id: Some(receipt.receipt_id.clone()),
            safe_reason_code: None,
            occurred_at: UtcMicros(22),
        },
        receipt,
        change_plan: None,
    }
}

fn fact_payload(content: &str) -> FactPayloadV1 {
    let mut tags = vec!["parity".to_owned()];
    let mut entities = vec!["TraceDecay".to_owned()];
    let metadata = serde_json::json!({});
    let reference = FactPayloadV1::canonicalize_material(
        content,
        FactCategoryV1::Project,
        &mut tags,
        &mut entities,
        &metadata,
        None,
    )
    .unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.parity.fact").unwrap(),
            ComponentVersion::new("sanitizer.parity.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(reference),
    )
    .unwrap();
    FactPayloadV1::new(
        content.to_owned(),
        FactCategoryV1::Project,
        tags,
        entities,
        metadata,
        None,
        receipt,
        RetentionClass::new("durable.parity").unwrap(),
    )
    .unwrap()
}

/// A fresh fact; another `content` keeps the fact id and changes the lineage.
pub fn fact(content: &str) -> FactWriteBatch {
    let owner = project_owner();
    let identity = FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Application {
            operation_id: ProvenanceId::new("operation.parity.fact").unwrap(),
        },
    )
    .unwrap();
    let fact_id = FactId::derive(&identity).unwrap();
    let assertion = FactAssertionV1::new(
        fact_id.clone(),
        owner.clone(),
        FactAssertionKindV1::Initial,
        fact_payload(content),
        vec![],
        UtcMicros(5),
        None,
    )
    .unwrap();
    let recorded = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        UtcMicros(5),
        None,
    )
    .unwrap();
    FactWriteBatch::new(
        fact_id,
        owner,
        Some(assertion),
        vec![recorded],
        vec![],
        vec![],
        None,
    )
    .unwrap()
    .with_identity_material(identity)
    .unwrap()
}

fn diagnostic_record(generation: &str, anchor: &str, message: &str) -> GenerationDiagnosticV1 {
    let mut record = GenerationDiagnosticV1 {
        diagnostic_anchor: id(anchor),
        generation_id: id(generation),
        repository: id("repository.parity"),
        worktree: Some(id("worktree.parity")),
        reference: Some(id("ref.main")),
        source_revision: Some(id("commit.abc123")),
        file_occurrence_id: id("file.occurrence.1"),
        content_digest: id(&repeated_sha256_text('a')),
        span: SourceSpan {
            start_byte: 10,
            end_byte: 42,
        },
        symbol_occurrence_id: Some(id("symbol.occurrence.1")),
        code: "E0308".to_owned(),
        severity: DiagnosticSeverityV1::Error,
        message: message.to_owned(),
        message_digest: id(&repeated_sha256_text('b')),
        provenance: DiagnosticProvenanceV1 {
            producer_kind: DiagnosticProducerKindV1::UpstreamCompiler,
            producer: id("producer.rustc"),
            analyzer_revision: id("analyzer.v1"),
            configuration_revision: id("config.v1"),
            sanitization_receipt: Some(id("receipt.sanitization.1")),
        },
        evidence_class: DiagnosticEvidenceClassV1::ProducerReported,
        collected_at: UtcMicros(1_700_000_000_000_000),
        state: DiagnosticRecordStateV1::Current,
    };
    record.message_digest = record.compute_message_digest().unwrap();
    record
}

pub fn diagnostics(generation: &str, message: &str) -> SanitizedCleanDiagnosticSnapshotV1 {
    SanitizedCleanDiagnosticSnapshotV1::new(
        id(generation),
        vec![diagnostic_record(
            generation,
            "anchor.diagnostic.parity",
            message,
        )],
    )
    .unwrap()
}

pub fn anchor_disposition(active: bool) -> RetrievalAnchorDispositionRecordV1 {
    let (state, reason, at) = if active {
        (
            AnchorDispositionStateV1::Active,
            AnchorDispositionReasonClassV1::Correction,
            7,
        )
    } else {
        (
            AnchorDispositionStateV1::Unavailable,
            AnchorDispositionReasonClassV1::SourceUnavailable,
            8,
        )
    };
    RetrievalAnchorDispositionRecordV1::new(
        "disposition.parity",
        RetrievalAnchorId::new(SOURCE_ANCHOR).unwrap(),
        project_owner(),
        state,
        None,
        reason,
        UtcMicros(at),
    )
    .unwrap()
}

pub fn anchor_derivative(direct_evidence: bool) -> RetrievalAnchorDerivativeV1 {
    RetrievalAnchorDerivativeV1::new(
        RetrievalAnchorId::new(SOURCE_ANCHOR).unwrap(),
        project_owner(),
        AnchorDerivativeKindV1::Span,
        "span.parity",
        direct_evidence,
    )
    .unwrap()
}

pub fn external_source() -> SourceCommitV1 {
    let definition = SourceDefinitionV1::new(
        SourceInstanceId::new("source.parity").unwrap(),
        1,
        SourceAcquisitionContractV1::new(
            ProviderId::new("github").unwrap(),
            SourceAcquisitionCapabilitiesV1::new(
                BTreeSet::from([SourceCaptureModeV1::Poll]),
                BTreeSet::from([SourceRefetchStrategyV1::WholeRoot]),
                BTreeSet::from([SourceDeletionSemanticsV1::CompleteSnapshotAbsence]),
            )
            .unwrap(),
        )
        .unwrap(),
        SourceCaptureModeV1::Poll,
        SourceRefetchStrategyV1::WholeRoot,
        SourceDeletionSemanticsV1::CompleteSnapshotAbsence,
        1,
    )
    .unwrap();
    let binding = SourceBindingV1::new(
        &definition,
        SourceBindingOwnerV1::Project(ProjectId::new(PROJECT).unwrap()),
        PrivacyDomainId::new("privacy.parity").unwrap(),
        LocatorDigest::new(digest('a').as_str()).unwrap(),
        1,
    )
    .unwrap();
    let identity = binding.immutable_identity().unwrap();
    let partition = SourcePartitionIdV1::new(digest('b'));
    let snapshot = SourceSnapshotIdV1::new(digest('c'));
    let observation = SourceObjectObservationV1::new(
        SourceNativeObjectIdV1::new(digest('d')),
        SourceObjectRevisionV1::new(digest('e')),
        digest('f'),
        SourceContentStateV1::Live,
    )
    .unwrap();
    let frontier = SourcePartitionFrontierV1::new(
        identity.clone(),
        partition.clone(),
        None,
        Some(snapshot.clone()),
        None,
        SourceCoverageV1::Complete,
        1,
        None,
        digest('1'),
    )
    .unwrap();
    let aggregate =
        SourceAggregateFrontierV1::with_updated_partition(identity.clone(), None, frontier)
            .unwrap();
    let completion = SourceSnapshotCompletionV1::new(
        partition.clone(),
        snapshot,
        BTreeSet::from([observation.native_object().clone()]),
    )
    .unwrap();
    let evidence = SourceObservationEvidenceV1::new(
        identity.clone(),
        partition.clone(),
        &observation,
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.external-source.parity").unwrap(),
            ComponentVersion::new("sanitizer.external-source.v1").unwrap(),
        )
        .unwrap(),
        RetrievalAnchorId::new("retrieval.external-source.parity").unwrap(),
        ResolutionAuthorizationV1 {
            resolved_scope_id: ScopeResolutionId::new("scope.external-source.parity").unwrap(),
            privacy_domain_id: identity.privacy_domain.clone(),
            access_policy_digest: AccessPolicyDigest::new(digest('4').as_str()).unwrap(),
            capability_id: CapabilityId::new("capability.external-source.parity").unwrap(),
            canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(digest('5').as_str())
                .unwrap(),
        },
        digest('6'),
    )
    .unwrap();
    let mutation = SourceObjectMutationV1::new(
        observation,
        None,
        SourceObjectTransitionV1::Initial,
        evidence,
    )
    .unwrap();
    SourceCommitV1::new(
        definition,
        binding,
        partition,
        digest('2'),
        digest('3'),
        None,
        aggregate,
        vec![mutation],
        Some(completion),
    )
    .unwrap()
}

fn committed_state(commit: SourceCommitV1) -> Box<SourceStoreStateV1> {
    match apply_source_commit(None, commit).unwrap() {
        SourceCommitApplyOutcomeV1::Committed(state) => state,
        other => panic!("expected a fresh commit, got {other:?}"),
    }
}

fn empty_successor(prior: &SourceStoreStateV1, sequence: u64, seed: char) -> SourceCommitV1 {
    let partition = prior.receipt().partition().clone();
    let next_partition = SourcePartitionFrontierV1::new(
        prior.binding().immutable_identity().unwrap(),
        partition.clone(),
        None,
        None,
        Some(SourceCursorV1::new(digest(seed))),
        SourceCoverageV1::Partial,
        sequence,
        prior
            .source_frontier()
            .partition(&partition)
            .and_then(SourcePartitionFrontierV1::last_complete_snapshot),
        digest(seed),
    )
    .unwrap();
    let next_frontier = SourceAggregateFrontierV1::with_updated_partition(
        prior.binding().immutable_identity().unwrap(),
        Some(prior.source_frontier()),
        next_partition,
    )
    .unwrap();
    SourceCommitV1::new(
        prior.definition().clone(),
        prior.binding().clone(),
        partition,
        digest(seed),
        digest('a'),
        Some(prior.source_frontier().clone()),
        next_frontier,
        Vec::new(),
        None,
    )
    .unwrap()
}

/// The fresh commit (an exact duplicate once committed) and its successor.
pub fn external_source_batch() -> Box<[SourceCommitV1]> {
    let first = external_source();
    let state = committed_state(first.clone());
    vec![first, empty_successor(&state, 2, '7')].into_boxed_slice()
}

/// Requires `external_source` committed: the executor rebuilds the projection
/// from the oldest pending receipt and requires equality.
pub fn external_source_projection() -> SourceProjectionCommitV1 {
    let state = committed_state(external_source());
    let pending = SourcePendingProjectionV1::from_state(
        &state,
        state.definition().clone(),
        state.binding().clone(),
        state.receipt().clone(),
    )
    .unwrap();
    build_source_projection(
        &pending,
        ComponentVersion::new("external-source-projector-v1").unwrap(),
    )
    .unwrap()
}

pub fn external_source_acquisition() -> SourceAcquisitionQueueCasV1 {
    let definition = SourceDefinitionV1::new(
        SourceInstanceId::new("source.parity-acquisition").unwrap(),
        1,
        SourceAcquisitionContractV1::new(
            ProviderId::new("github").unwrap(),
            SourceAcquisitionCapabilitiesV1::new(
                BTreeSet::from([SourceCaptureModeV1::Event]),
                BTreeSet::from([SourceRefetchStrategyV1::WholeRoot]),
                BTreeSet::from([SourceDeletionSemanticsV1::ExplicitOnly]),
            )
            .unwrap(),
        )
        .unwrap(),
        SourceCaptureModeV1::Event,
        SourceRefetchStrategyV1::WholeRoot,
        SourceDeletionSemanticsV1::ExplicitOnly,
        1,
    )
    .unwrap();
    let request = SourceAcquisitionRequestV1::github_review(
        definition.provider.clone(),
        LocatorDigest::new(digest('9').as_str()).unwrap(),
        FeedbackScopeV1 {
            project_id: ProjectId::new(PROJECT).unwrap(),
            repository_id: RepositoryId::new("repository.parity").unwrap(),
            worktree_id: WorktreeId::new("worktree.parity").unwrap(),
            branch_ref: "refs/heads/parity".to_owned(),
            head_commit_id: CommitId::new("9".repeat(40)).unwrap(),
        },
        GitHubReviewReadOperationV1::RestListPullRequestReviewComments,
        GitHubPullRequestIdV1::new("pr.parity").unwrap(),
    )
    .unwrap();
    let binding = SourceBindingV1::new(
        &definition,
        SourceBindingOwnerV1::Project(ProjectId::new(PROJECT).unwrap()),
        PrivacyDomainId::new("privacy.parity").unwrap(),
        request.binding_native_root().unwrap(),
        1,
    )
    .unwrap();
    let identity = binding.immutable_identity().unwrap();
    let event = SourceEventV1::new(identity.clone(), digest('a')).unwrap();
    let refresh = SourceRefreshReceiptV1::new(
        identity.clone(),
        definition.provider.clone(),
        digest('b'),
        SourceRefreshCauseV1::Event,
        SourceCaptureModeV1::Event,
        SourceRefetchStrategyV1::WholeRoot,
    )
    .unwrap();
    let receipt = SourceEventAdmissionReceiptV1::new(
        &event,
        event.event_key().clone(),
        refresh,
        SourceEventAdmissionDispositionV1::Enqueued,
    )
    .unwrap();
    let scheduled = SourceScheduledRefetchV1::new(
        definition.clone(),
        binding.clone(),
        request,
        receipt.clone(),
        None,
        0,
        UtcMicros(10),
    )
    .unwrap();
    let state = SourceAcquisitionQueueStateV1::new(
        definition,
        binding,
        Some(scheduled),
        None,
        BTreeMap::from([(receipt.event_key().clone(), receipt)]),
    )
    .unwrap();
    SourceAcquisitionQueueCasV1::new(identity, None, state).unwrap()
}

pub fn code_scope() -> serde_json::Value {
    serde_json::json!({
        "kind": "code",
        "project_id": "project.git-transaction.parity",
        "repository_id": "repository.git-transaction.parity",
        "scope": { "kind": "worktree", "worktree_id": "worktree.git-transaction.parity" }
    })
}

pub fn git_index_transaction() -> GitIndexTransactionRecordV1 {
    let oid = |byte: char| GitOidV1::new(byte.to_string().repeat(40)).unwrap();
    let snapshot = RepositoryStateSnapshotV1::new(
        id::<ProjectId>("project.git-transaction.parity"),
        id::<RepositoryId>("repository.git-transaction.parity"),
        Some(id::<WorktreeId>("worktree.git-transaction.parity")),
        1,
        GitObjectFormatV1::Sha1,
        GitHeadStateV1::Attached {
            branch: "refs/heads/main".to_owned(),
            commit: oid('a'),
        },
        RepositoryIndexSnapshotV1 {
            checksum: digest('b'),
            tree_id: Some(oid('c')),
            state: RepositoryIndexStateV1::Clean,
            unmerged_stage_digest: None,
        },
        RepositoryWorkingTreeSnapshotV1 {
            state: RepositoryWorkingTreeStateV1::Clean,
            tracked_digest: digest('d'),
            untracked_name_digest: None,
            ignored_collision_digest: None,
        },
        tracedecay_domain::GitOperationStateV1::None,
        Some(digest('0')),
        Some(digest('1')),
        Some(digest('2')),
        Some(digest('3')),
        Some(digest('4')),
        UtcMicros(1),
        GitCoverageV1::complete(),
    )
    .unwrap()
    .with_native_identity(
        "git version parity".to_owned(),
        "tracedecay.git-index-adapter.v1".to_owned(),
        digest('5'),
    )
    .unwrap();
    let snapshot_digest = GitIndexPreviewV1::repository_snapshot_digest(&snapshot).unwrap();
    let preview_id = GitIndexPreviewId::new("preview.git-transaction.parity").unwrap();
    let hunk = HunkRefV1 {
        repository: id::<RepositoryId>("repository.git-transaction.parity"),
        worktree: id::<WorktreeId>("worktree.git-transaction.parity"),
        direction: HunkDirectionV1::WorkingTreeToIndex,
        path: "packet.txt".to_owned(),
        original_path: None,
        expected_base_blob: GitBlobExpectationV1::AbsentFile,
        expected_index_entry: GitIndexEntryExpectationV1 {
            blob: GitBlobExpectationV1::AbsentFile,
            mode: None,
            unmerged_stage: None,
        },
        expected_worktree_blob: Some(GitBlobExpectationV1::AbsentFile),
        expected_worktree_mode: None,
        hunk_header: "@@ -1 +1 @@".to_owned(),
        context_digest: digest('b'),
        patch_digest: digest('c'),
        selected_line_bitmap: vec![1],
        attributes_digest: None,
        preview_id: preview_id.as_str().to_owned(),
        schema_version: "tracedecay.git-hunk-ref.v1".to_owned(),
        snapshot_digest: snapshot_digest.clone(),
    };
    let preview = GitIndexPreviewV1::new(
        preview_id,
        GitIndexTransactionOperationV1::StageHunks,
        snapshot.clone(),
        snapshot_digest,
        vec![hunk],
        snapshot.index.tree_id.clone(),
        GitIndexPreviewDispositionV1::Applicable,
        UtcMicros(10),
        UtcMicros(100),
    )
    .unwrap();
    GitIndexTransactionRecordV1 {
        idempotency_key: GitIndexIdempotencyKey::new("idempotency.git-index.parity".to_owned())
            .unwrap(),
        input_digest: digest('6'),
        journal: GitIndexTransactionJournalV1::prepared(
            GitIndexTransactionId::new("transaction.idempotency.git-index.parity".to_owned())
                .unwrap(),
            &preview,
            UtcMicros(11),
        )
        .unwrap(),
        preview,
        terminal_receipt: None,
    }
}

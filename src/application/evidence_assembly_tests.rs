use std::collections::BTreeSet;

use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext as ProductRequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::{
    AccessPolicyDigest, ActorId, AnchorDurabilityClass, AnchorLineageRefV3, AnchorOwnerBindingV1,
    AnchorProvenanceRelationV2, AnchorSourceGenerationV3, CoverageReportV1, EvidenceClass,
    ManifestDigest, PayloadAccessState, PrivacyDomainBoundLocatorDigest, PrivacyDomainId,
    ProjectId, ProjectionGenerationId, RepositoryId, ResolutionAuthorizationV1, RetentionClass,
    RetrievalAnchorId, RetrievalAnchorRecordV3, RetrievalAnchorRecordV3Parts,
    RetrievalAnchorTargetV3, ScopeResolutionId, SourceOccurrenceId, UserProfileId, UtcMicros,
    VectorWatermark, WorktreeId,
};
use tracedecay_store::{
    AnchorDerivativeKindV1, AnchorDispositionReasonClassV1, AnchorDispositionStateV1,
    EvidenceAssemblyOwnerV1, EvidenceAssemblyStoreError, RetrievalAnchorDerivativeV1,
    RetrievalAnchorDispositionRecordV1, RetrievalAnchorDispositionStore, RetrievalAnchorOwnerV1,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::*;
use crate::application::host_admission::HostAdmissionTestRuntimeV1;
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};

fn product_context() -> ProductRequestContext {
    let actor = ActorId::new("actor.evidence-test").unwrap();
    let scope = ResolvedScope::new(
        ProjectId::new("project.evidence-test").unwrap(),
        RepositoryId::new("repository.evidence-test").unwrap(),
        WorktreeId::new("worktree.evidence-test").unwrap(),
        None,
    )
    .unwrap();
    let capability = CapabilityId::new("capability.evidence-test").unwrap();
    let use_case = UseCaseId::new("use-case.evidence-test").unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.evidence-test").unwrap(),
        1,
        ManifestDigest::new(format!("sha256:{}", "11".repeat(32))).unwrap(),
        actor.clone(),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    ProductRequestContext::new(
        actor,
        scope,
        grant,
        RequestId::new("request.evidence-test").unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active("cancel.evidence-test").unwrap(),
    )
    .unwrap()
}

fn owner(context: &ProductRequestContext) -> EvidenceOwnerBinding {
    EvidenceOwnerBinding::for_feedback_context(context, "privacy.evidence-test", 7).unwrap()
}

fn runtime_owner(context: &ProductRequestContext) -> EvidenceAssemblyOwnerV1 {
    EvidenceAssemblyOwnerV1 {
        owner: AnchorOwnerBindingV1::for_project(
            UserProfileId::new("profile.evidence-test").unwrap(),
            context.scope().project_id.clone(),
            PrivacyDomainId::new("privacy.evidence-test").unwrap(),
        )
        .unwrap(),
        scope_digest: context.scope().scope_digest.clone(),
        key_epoch: 7,
    }
}

fn runtime_anchor(owner: &EvidenceAssemblyOwnerV1) -> RetrievalAnchorRecordV3 {
    let digest = format!("sha256:{}", "33".repeat(32));
    RetrievalAnchorRecordV3::new(RetrievalAnchorRecordV3Parts {
        target: RetrievalAnchorTargetV3::ExactSourceOccurrence(
            SourceOccurrenceId::new("occurrence.evidence-test").unwrap(),
        ),
        owner: owner.owner.clone(),
        aliases: vec![],
        occurred_at: None,
        ingested_at: UtcMicros(1),
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGenerationV3::Unknown,
        projection_generation: ProjectionGenerationId::new("projection.evidence-test").unwrap(),
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations: vec![],
        source_anchors: vec![
            AnchorLineageRefV3::new(
                0,
                AnchorProvenanceRelationV2::DerivedFrom,
                RetrievalAnchorId::new("anchor.source.evidence-test").unwrap(),
                owner.owner.clone(),
            )
            .unwrap(),
        ],
        authorization: ResolutionAuthorizationV1 {
            resolved_scope_id: ScopeResolutionId::new("scope.evidence-test").unwrap(),
            privacy_domain_id: PrivacyDomainId::new("privacy.evidence-test").unwrap(),
            access_policy_digest: AccessPolicyDigest::new(digest.clone()).unwrap(),
            capability_id: tracedecay_domain::CapabilityId::new("capability.evidence-test")
                .unwrap(),
            canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(digest).unwrap(),
        },
        payload_access: PayloadAccessState::Eligible,
        retention_class: RetentionClass::new("retention.evidence-test").unwrap(),
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .unwrap()
}

#[test]
fn runtime_anchor_resolution_rechecks_exact_current_scope() {
    let context = product_context();
    let owner = runtime_owner(&context);

    assert_eq!(
        authorize_runtime_anchor_resolution_at(&context, &owner, UtcMicros(100)),
        Ok(())
    );

    let mut wrong_scope = owner.clone();
    wrong_scope.scope_digest = ManifestDigest::new(format!("sha256:{}", "22".repeat(32))).unwrap();
    assert_eq!(
        authorize_runtime_anchor_resolution_at(&context, &wrong_scope, UtcMicros(100)),
        Err(EvidenceAssemblyStoreError::Unavailable)
    );

    assert_eq!(
        authorize_runtime_anchor_resolution_at(&context, &owner, UtcMicros(10_000)),
        Err(EvidenceAssemblyStoreError::Unavailable)
    );

    let mut wrong_owner = owner;
    wrong_owner.owner = AnchorOwnerBindingV1::for_project(
        UserProfileId::new("profile.evidence-test").unwrap(),
        ProjectId::new("project.other").unwrap(),
        PrivacyDomainId::new("privacy.evidence-test").unwrap(),
    )
    .unwrap();
    assert_eq!(
        authorize_runtime_anchor_resolution_at(&context, &wrong_owner, UtcMicros(100)),
        Err(EvidenceAssemblyStoreError::Unavailable)
    );
}

#[tokio::test]
async fn runtime_anchor_resolution_uses_one_snapshot_across_terminal_write() {
    let context = product_context();
    let owner = runtime_owner(&context);
    let anchor = runtime_anchor(&owner);
    let anchor_id = anchor.anchor_id().clone();
    let anchor_owner = RetrievalAnchorOwnerV1::V3(owner.owner.clone());
    let derivative = RetrievalAnchorDerivativeV1::new(
        anchor_id.clone(),
        anchor_owner.clone(),
        AnchorDerivativeKindV1::Span,
        "span.evidence-test",
        true,
    )
    .unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("project.db");
    let authority =
        DatabaseAuthority::acquire_test(&path, "anchor resolution snapshot test").unwrap();
    let (database, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .unwrap();
    let transaction = database
        .begin_write_transaction("seed anchor resolution snapshot test")
        .await
        .unwrap();
    transaction
        .execute(
            "INSERT INTO retrieval_anchors (
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                anchor_id.as_str(),
                serialize(&anchor).unwrap(),
                serialize(&anchor_owner).unwrap(),
                anchor.projection_generation().as_str(),
            ],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    RetrievalAnchorDispositionStore::publish_derivative(&database, derivative.clone())
        .await
        .unwrap();

    let snapshot = database
        .begin_engine_read_snapshot("anchor resolution snapshot test")
        .await
        .unwrap();
    let mut establish = snapshot
        .query(
            "SELECT anchor_id FROM retrieval_anchors WHERE anchor_id = ?1",
            params![anchor_id.as_str()],
        )
        .await
        .unwrap();
    assert!(establish.next().await.unwrap().is_some());
    drop(establish);

    RetrievalAnchorDispositionStore::append_disposition(
        &database,
        RetrievalAnchorDispositionRecordV1::new(
            "disposition.evidence-test",
            anchor_id.clone(),
            anchor_owner.clone(),
            AnchorDispositionStateV1::Deleted,
            None,
            AnchorDispositionReasonClassV1::UserRequest,
            UtcMicros(2),
        )
        .unwrap(),
    )
    .await
    .unwrap();

    assert_eq!(
        resolve_anchor_snapshot(&snapshot, &anchor_id, &anchor_owner)
            .await
            .unwrap(),
        EvidenceAssemblyAnchorResolutionV1::Resolved {
            record: anchor,
            derivatives: vec![derivative],
        }
    );
    drop(snapshot);

    let current = database
        .begin_engine_read_snapshot("current anchor resolution snapshot test")
        .await
        .unwrap();
    assert!(matches!(
        resolve_anchor_snapshot(&current, &anchor_id, &anchor_owner)
            .await
            .unwrap(),
        EvidenceAssemblyAnchorResolutionV1::Tombstone(tombstone)
            if tombstone.terminal_state() == AnchorDispositionStateV1::Deleted
    ));
}

fn occurrence(
    owner: &EvidenceOwnerBinding,
    source_order: u64,
    anchor_suffix: &str,
) -> SourceOccurrenceRecord {
    SourceOccurrenceRecord::new(
        owner.clone(),
        SourceTimelineKey::new(
            "cursor",
            "session.fixture",
            "session-generation:3",
            "observation_sequence_projection_output_ordinal",
        )
        .unwrap(),
        RetrievalAnchorId::new(format!("anchor.source.{anchor_suffix}")).unwrap(),
        Some(format!("observation.{anchor_suffix}")),
        SourceOccurrenceCoordinate::new(source_order + 10, 0, source_order),
        SourceOccurrenceKind::SessionOccurrence,
        "session-projector-v1",
    )
    .unwrap()
}

fn occurrence_in_generation(
    owner: &EvidenceOwnerBinding,
    source_order: u64,
    generation: &str,
) -> SourceOccurrenceRecord {
    SourceOccurrenceRecord::new(
        owner.clone(),
        SourceTimelineKey::new(
            "cursor",
            "session.fixture",
            generation,
            "observation_sequence_projection_output_ordinal",
        )
        .unwrap(),
        RetrievalAnchorId::new(format!("anchor.source.{generation}.{source_order}")).unwrap(),
        Some(format!("observation.{generation}.{source_order}")),
        SourceOccurrenceCoordinate::new(source_order + 10, 0, source_order),
        SourceOccurrenceKind::SessionOccurrence,
        "session-projector-v1",
    )
    .unwrap()
}

fn write_fixture(
    owner: &EvidenceOwnerBinding,
    idempotency_key: &str,
    source_orders: &[u64],
) -> EvidenceAssemblyWrite {
    let occurrences = source_orders
        .iter()
        .map(|order| occurrence(owner, *order, &order.to_string()))
        .collect::<Vec<_>>();
    build_write(
        owner.clone(),
        idempotency_key.to_owned(),
        occurrences.clone(),
        EvidenceSpanProducerKind::SessionBurst,
        "session-projector-v1",
        "session-adjacency-v1",
        "sha256:request-fixture".to_owned(),
        "session_temporal",
        TemporalModeV1::Current,
        RetrieverWatermarkBinding {
            source: "12".to_owned(),
            projection: "13".to_owned(),
            index: "14".to_owned(),
            summary: "15".to_owned(),
        },
        EvidenceAssemblyCoverage {
            eligible: occurrences.len() as u64,
            selected: occurrences.len() as u64,
            omitted: 0,
            hidden: 0,
            unknown: 0,
            redacted: 0,
            complete: true,
        },
        UtcMicros(100),
    )
    .unwrap()
}

#[test]
fn canonical_sets_ignore_input_permutation_but_spans_preserve_source_order() {
    let context = product_context();
    let owner = owner(&context);
    let first = occurrence(&owner, 0, "first");
    let second = occurrence(&owner, 1, "second");
    let left =
        CanonicalSourceOccurrenceSet::new(owner.clone(), vec![first.clone(), second.clone()])
            .unwrap();
    let right =
        CanonicalSourceOccurrenceSet::new(owner.clone(), vec![second.clone(), first.clone()])
            .unwrap();

    assert_eq!(left, right);
    let run = EvidenceSpanRun::verify(0, &[first.clone(), second.clone()], "session-adjacency-v1")
        .unwrap();
    assert_eq!(
        run.occurrence_ids,
        vec![first.occurrence_id.clone(), second.occurrence_id.clone()]
    );
    assert_eq!(
        EvidenceSpanRun::verify(
            0,
            &[first, occurrence(&owner, 3, "gap")],
            "session-adjacency-v1",
        ),
        Err(EvidenceAssemblyError::NonConsecutive)
    );
    assert_eq!(
        EvidenceSpanRun::verify(
            0,
            &[
                occurrence_in_generation(&owner, 0, "session-generation:3"),
                occurrence_in_generation(&owner, 1, "session-generation:4"),
            ],
            "session-adjacency-v1",
        ),
        Err(EvidenceAssemblyError::BoundaryMismatch)
    );
}

#[tokio::test]
async fn publication_is_atomic_replayable_and_conflict_detecting() {
    let context = product_context();
    let owner = owner(&context);
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("project.db");
    let authority = DatabaseAuthority::acquire_test(&path, "evidence assembly test").unwrap();
    let (database, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .unwrap();
    let write = write_fixture(&owner, "sha256:idempotency-fixture", &[0, 1]);
    let receipt = EvidenceAssemblyPublicationReceipt::from_write(&write).unwrap();

    let transaction = database
        .begin_write_transaction("test evidence publication")
        .await
        .unwrap();
    persist_write(&transaction, &write, &receipt).await.unwrap();
    transaction.commit().await.unwrap();

    let replay_transaction = database
        .begin_write_transaction("test evidence replay")
        .await
        .unwrap();
    assert_eq!(
        replay_receipt(&replay_transaction, &write).await.unwrap(),
        Some(receipt.clone())
    );
    replay_transaction.rollback().await.unwrap();

    let conflicting = write_fixture(&owner, "sha256:idempotency-fixture", &[0, 1, 2]);
    let conflict_transaction = database
        .begin_write_transaction("test evidence conflict")
        .await
        .unwrap();
    assert_eq!(
        replay_receipt(&conflict_transaction, &conflicting).await,
        Err(EvidenceAssemblyError::ReplayConflict)
    );
    conflict_transaction.rollback().await.unwrap();
}

#[tokio::test]
async fn authorized_drilldown_expands_contribution_span_set_and_exact_members() {
    let context = product_context();
    let owner = owner(&context);
    let temporary = tempfile::tempdir().unwrap();
    let project_path = temporary.path().join("project.db");
    let authority =
        DatabaseAuthority::acquire_test(&project_path, "evidence drilldown test").unwrap();
    let (database, _) = Database::publish_test_runtime(
        &project_path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .unwrap();
    let session_runtime = HostAdmissionTestRuntimeV1::profile(temporary.path().join(".tracedecay"))
        .await
        .unwrap();
    let write = write_fixture(&owner, "sha256:drilldown-fixture", &[4, 5, 6]);
    let contribution_id = write.contribution.contribution_id.clone();
    let span_id = write.span.span_id.clone();
    let ordered_occurrence_ids = write
        .ordered_occurrences
        .iter()
        .map(|occurrence| occurrence.occurrence_id.clone())
        .collect::<Vec<_>>();
    let receipt = EvidenceAssemblyPublicationReceipt::from_write(&write).unwrap();
    let transaction = database
        .begin_write_transaction("test evidence drilldown publication")
        .await
        .unwrap();
    persist_write(&transaction, &write, &receipt).await.unwrap();
    transaction.commit().await.unwrap();

    let service = session_runtime.evidence_assembly_service_for_test(database);
    drop(session_runtime);
    let first_page = service
        .drilldown_contribution(&context, &contribution_id, 0, 2)
        .await
        .unwrap();
    assert_eq!(first_page.contribution.contribution_id, contribution_id);
    assert_eq!(first_page.span.span_id, span_id);
    assert_eq!(
        first_page
            .exact_sources
            .iter()
            .map(|source| source.occurrence.occurrence_id.clone())
            .collect::<Vec<_>>(),
        ordered_occurrence_ids[..2]
    );
    assert_eq!(first_page.exact_sources.len(), 2);
    assert_eq!(first_page.next_ordinal, Some(2));
    assert!(first_page.exact_sources.iter().all(|source| {
        source.disposition == ExactSourceDisposition::Unavailable && source.anchor.is_none()
    }));

    let second_page = service
        .drilldown_contribution(&context, &contribution_id, 2, 2)
        .await
        .unwrap();
    assert_eq!(
        second_page.exact_sources[0].occurrence.occurrence_id,
        ordered_occurrence_ids[2]
    );
    assert_eq!(second_page.exact_sources.len(), 1);
    assert_eq!(second_page.next_ordinal, None);
    assert_eq!(second_page.occurrence_set_id, first_page.occurrence_set_id);
}

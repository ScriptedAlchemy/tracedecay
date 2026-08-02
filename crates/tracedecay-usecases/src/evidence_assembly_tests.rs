use std::collections::BTreeSet;

use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext as ProductRequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::{
    AccessPolicyDigest, ActorId, AnchorDurabilityClass, AnchorLineageRefV3, AnchorOwnerBindingV1,
    AnchorProvenanceRelationV2, AnchorSourceGenerationV2, AnchorSourceGenerationV3,
    CanonicalObservationIdV1, CoverageReportV1, EvidenceClass, FactOwnerV1, ManifestDigest,
    ObservationScopeV1, ObservationSourceGenerationV1, PayloadAccessState,
    PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectId, ProjectionGenerationId,
    RepositoryId, ResolutionAuthorizationV1, RetentionClass, RetrievalAnchorId,
    RetrievalAnchorRecordV2, RetrievalAnchorRecordV2Parts, RetrievalAnchorRecordV3,
    RetrievalAnchorRecordV3Parts, RetrievalAnchorTargetV2, RetrievalAnchorTargetV3,
    ScopeResolutionId, SourceOccurrenceId, UserProfileId, UtcMicros, VectorWatermark, WorktreeId,
};
use tracedecay_store::{
    AnchorDerivativeKindV1, AnchorDispositionReasonClassV1, AnchorDispositionStateV1,
    EvidenceAssemblyOwnerV1, EvidenceAssemblyStoreError, RetrievalAnchorDerivativeV1,
    RetrievalAnchorDispositionRecordV1, RetrievalAnchorDispositionStore, RetrievalAnchorOwnerV1,
    StoredRetrievalAnchorRecordV1,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::*;
use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};

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
    crate::register_test_schema_installer();
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
                serde_json::to_string(&anchor).unwrap(),
                serde_json::to_string(&anchor_owner).unwrap(),
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
            record: StoredRetrievalAnchorRecordV1::V3(anchor),
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

/// The observation and repository-provenance writers still commit V2 anchor
/// records. If evidence assembly only served V3, every anchor those writers
/// produced would resolve as `Unavailable` — an absence the store cannot
/// justify, because the record is right there. This pins that a persisted V2
/// anchor is served.
#[tokio::test]
async fn runtime_anchor_resolution_serves_persisted_v2_anchor_records() {
    let context = product_context();
    let anchor = legacy_anchor(&context);
    let anchor_id = anchor.anchor_id().clone();
    let anchor_owner = RetrievalAnchorOwnerV1::V2(FactOwnerV1::from(anchor.owner().clone()));

    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("project.db");
    crate::register_test_schema_installer();
    let authority =
        DatabaseAuthority::acquire_test(&path, "legacy anchor resolution test").unwrap();
    let (database, _) =
        Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
            .await
            .unwrap();
    let transaction = database
        .begin_write_transaction("seed legacy anchor resolution test")
        .await
        .unwrap();
    transaction
        .execute(
            "INSERT INTO retrieval_anchors (
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                anchor_id.as_str(),
                serde_json::to_string(&anchor).unwrap(),
                serde_json::to_string(&anchor_owner).unwrap(),
                anchor.projection_generation().as_str(),
            ],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let snapshot = database
        .begin_engine_read_snapshot("legacy anchor resolution test")
        .await
        .unwrap();
    assert_eq!(
        resolve_anchor_snapshot(&snapshot, &anchor_id, &anchor_owner)
            .await
            .unwrap(),
        EvidenceAssemblyAnchorResolutionV1::Resolved {
            record: StoredRetrievalAnchorRecordV1::V2(anchor),
            derivatives: vec![],
        }
    );
}

fn legacy_anchor(context: &ProductRequestContext) -> RetrievalAnchorRecordV2 {
    let digest = format!("sha256:{}", "44".repeat(32));
    let observation_id =
        CanonicalObservationIdV1::new(format!("sha256:{}", "55".repeat(32))).unwrap();
    RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target: RetrievalAnchorTargetV2::ExactObservation(observation_id.clone()),
        owner: ObservationScopeV1::Project {
            project_id: context.scope().project_id.clone(),
        },
        aliases: vec![],
        occurred_at: None,
        ingested_at: UtcMicros(1),
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGenerationV2::Observation(
            ObservationSourceGenerationV1::new(1).unwrap(),
        ),
        projection_generation: ProjectionGenerationId::new("projection.evidence-test").unwrap(),
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations: vec![observation_id],
        source_anchors: vec![],
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

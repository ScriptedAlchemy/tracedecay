use serde_json::json;

use super::*;
use crate::configuration::UserProfileId;
use crate::research::{
    AccessPolicyDigest, ComponentVersion, EntityId, EntityKind, PrivacyDomainId, ProjectId,
    RetrieverContributionIdV1, SanitizationReceiptId, ScopeResolutionId,
};
use crate::retrieval::SourceOccurrenceId;
use crate::session_derived::EvidenceSpanIdV1;

const DIGEST_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DIGEST_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn observation(seed: char) -> CanonicalObservationIdV1 {
    CanonicalObservationIdV1::new(format!(
        "sha256:{}",
        std::iter::repeat_n(seed, 64).collect::<String>()
    ))
    .unwrap()
}

fn owner(project: &str) -> ObservationScopeV1 {
    ObservationScopeV1::Project {
        project_id: ProjectId::new(project).unwrap(),
    }
}

fn v3_owner(project: &str, privacy: &str) -> AnchorOwnerBindingV1 {
    AnchorOwnerBindingV1::for_project(
        UserProfileId::new("profile.fixture").unwrap(),
        ProjectId::new(project).unwrap(),
        PrivacyDomainId::new(privacy).unwrap(),
    )
    .unwrap()
}

fn authorization() -> ResolutionAuthorizationV1 {
    ResolutionAuthorizationV1 {
        resolved_scope_id: ScopeResolutionId::new("scope.fixture").unwrap(),
        privacy_domain_id: PrivacyDomainId::new("privacy.fixture").unwrap(),
        access_policy_digest: AccessPolicyDigest::new(DIGEST_A).unwrap(),
        capability_id: crate::research::CapabilityId::new("capability.fixture").unwrap(),
        canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(DIGEST_B).unwrap(),
    }
}

fn record_parts(
    target: RetrievalAnchorTargetV2,
    owner: ObservationScopeV1,
) -> RetrievalAnchorRecordV2Parts {
    let source_observations = match &target {
        RetrievalAnchorTargetV2::ExactObservation(id) => vec![id.clone()],
        _ => vec![observation('c')],
    };
    RetrievalAnchorRecordV2Parts {
        target,
        owner,
        aliases: vec![],
        occurred_at: Some(TimeInterval {
            start: UtcMicros(1),
            end: UtcMicros(2),
        }),
        ingested_at: UtcMicros(3),
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGenerationV2::Observation(
            ObservationSourceGenerationV1::new(7).unwrap(),
        ),
        projection_generation: ProjectionGenerationId::new("projection.fixture").unwrap(),
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations,
        source_anchors: vec![],
        authorization: authorization(),
        payload_access: PayloadAccessState::Eligible,
        retention_class: RetentionClass::new("retention.fixture").unwrap(),
        durability: AnchorDurabilityClass::DurableEvidence,
    }
}

fn entity_target(id: &str) -> RetrievalAnchorTargetV2 {
    RetrievalAnchorTargetV2::Entity(EntityRef {
        id: EntityId::new(id).unwrap(),
        kind: EntityKind::Document,
    })
}

#[test]
fn assertion_provenance_relations_have_stable_snake_case_wire_values() {
    for (relation, expected) in [
        (AnchorProvenanceRelationV2::Corrects, "corrects"),
        (AnchorProvenanceRelationV2::Contradicts, "contradicts"),
        (AnchorProvenanceRelationV2::Supersedes, "supersedes"),
        (AnchorProvenanceRelationV2::Supports, "supports"),
    ] {
        assert_eq!(serde_json::to_value(relation).unwrap(), json!(expected));
        assert_eq!(
            serde_json::from_value::<AnchorProvenanceRelationV2>(json!(expected)).unwrap(),
            relation
        );
    }
}

#[test]
fn replay_derives_the_same_anchor_identity() {
    let first = RetrievalAnchorRecordV2::new(record_parts(
        entity_target("document.fixture"),
        owner("project.fixture"),
    ))
    .unwrap();
    let mut replay_parts =
        record_parts(entity_target("document.fixture"), owner("project.fixture"));
    replay_parts.ingested_at = UtcMicros(999);
    replay_parts.aliases = vec![
        NativeAliasV2::new(
            NativeAliasKindV2::Path,
            PrivacyDomainBoundLocatorDigest::new(DIGEST_A).unwrap(),
        )
        .unwrap(),
    ];
    let replay = RetrievalAnchorRecordV2::new(replay_parts).unwrap();

    assert_eq!(first.anchor_id(), replay.anchor_id());
}

#[test]
fn exact_observation_anchor_identity_ignores_projection_generation() {
    let observation_id = observation('a');
    let owner = owner("project.fixture");
    let expected = derive_exact_observation_anchor_id(&owner, &observation_id).unwrap();
    let mut parts = record_parts(
        RetrievalAnchorTargetV2::ExactObservation(observation_id.clone()),
        owner.clone(),
    );
    parts.source_observations = vec![observation_id];
    let first = RetrievalAnchorRecordV2::new(parts.clone()).unwrap();
    parts.projection_generation = ProjectionGenerationId::new("projection.rebuilt").unwrap();
    let rebuilt = RetrievalAnchorRecordV2::new(parts).unwrap();

    assert_eq!(first.anchor_id(), &expected);
    assert_eq!(rebuilt.anchor_id(), &expected);
}

#[test]
fn owner_is_part_of_anchor_identity() {
    let first = RetrievalAnchorRecordV2::new(record_parts(
        entity_target("document.fixture"),
        owner("project.one"),
    ))
    .unwrap();
    let second = RetrievalAnchorRecordV2::new(record_parts(
        entity_target("document.fixture"),
        owner("project.two"),
    ))
    .unwrap();

    assert_ne!(first.anchor_id(), second.anchor_id());
}

#[test]
fn v3_targets_exact_occurrences_spans_and_contributions() {
    let occurrence = SourceOccurrenceId::new("occurrence.fixture").unwrap();
    let span = EvidenceSpanIdV1::new(format!("sha256:{}", "12".repeat(32))).unwrap();
    let contribution = RetrieverContributionIdV1::new("contribution.fixture").unwrap();

    for (target, expected_kind) in [
        (
            RetrievalAnchorTargetV3::ExactSourceOccurrence(occurrence),
            "exact_source_occurrence",
        ),
        (
            RetrievalAnchorTargetV3::ExactEvidenceSpan(span),
            "exact_evidence_span",
        ),
        (
            RetrievalAnchorTargetV3::RetrieverContribution(contribution),
            "retriever_contribution",
        ),
    ] {
        target.validate().unwrap();
        let wire = serde_json::to_value(&target).unwrap();
        assert_eq!(wire["kind"], json!(expected_kind));
        assert_eq!(
            serde_json::from_value::<RetrievalAnchorTargetV3>(wire).unwrap(),
            target
        );
    }
}

#[test]
fn v3_target_decodes_existing_v2_wire_unchanged() {
    let v2 = entity_target("document.fixture");
    let v2_wire = serde_json::to_value(&v2).unwrap();
    let v3 = serde_json::from_value::<RetrievalAnchorTargetV3>(v2_wire.clone()).unwrap();

    assert_eq!(serde_json::to_value(&v3).unwrap(), v2_wire);
}

#[test]
fn v3_exact_evidence_anchor_identity_is_owner_bound() {
    let occurrence = SourceOccurrenceId::new("occurrence.fixture").unwrap();
    let first = derive_exact_source_occurrence_anchor_id(
        &v3_owner("project.one", "privacy.one"),
        &occurrence,
    )
    .unwrap();
    let replay = derive_exact_source_occurrence_anchor_id(
        &v3_owner("project.one", "privacy.one"),
        &occurrence,
    )
    .unwrap();
    let other_owner = derive_exact_source_occurrence_anchor_id(
        &v3_owner("project.two", "privacy.one"),
        &occurrence,
    )
    .unwrap();
    let other_privacy = derive_exact_source_occurrence_anchor_id(
        &v3_owner("project.one", "privacy.two"),
        &occurrence,
    )
    .unwrap();

    assert_eq!(first, replay);
    assert_ne!(first, other_owner);
    assert_ne!(first, other_privacy);
    assert!(first.as_str().starts_with("retrieval.v3."));
}

#[test]
fn v3_lineage_preserves_source_order_and_privacy_binding() {
    let owner = v3_owner("project.fixture", "privacy.fixture");
    let first = AnchorLineageRefV3::new(
        0,
        AnchorProvenanceRelationV2::DerivedFrom,
        RetrievalAnchorId::new("retrieval.source.first").unwrap(),
        owner.clone(),
    )
    .unwrap();
    let second = AnchorLineageRefV3::new(
        1,
        AnchorProvenanceRelationV2::DerivedFrom,
        RetrievalAnchorId::new("retrieval.source.second").unwrap(),
        owner,
    )
    .unwrap();

    validate_anchor_lineage_v3(&[first.clone(), second.clone()]).unwrap();
    assert_eq!(first.source_ordinal(), 0);
    assert_eq!(second.source_ordinal(), 1);
    assert_eq!(
        validate_anchor_lineage_v3(&[second, first]).unwrap_err(),
        DomainError::NonCanonical {
            field: "retrieval anchor V3 source lineage order"
        }
    );
}

#[test]
fn v3_record_binds_exact_target_owner_and_ordered_lineage() {
    let owner = v3_owner("project.fixture", "privacy.fixture");
    let source = AnchorLineageRefV3::new(
        0,
        AnchorProvenanceRelationV2::DerivedFrom,
        RetrievalAnchorId::new("retrieval.source.fixture").unwrap(),
        owner.clone(),
    )
    .unwrap();
    let parts = RetrievalAnchorRecordV3Parts {
        target: RetrievalAnchorTargetV3::ExactSourceOccurrence(
            SourceOccurrenceId::new("occurrence.fixture").unwrap(),
        ),
        owner: owner.clone(),
        aliases: vec![],
        occurred_at: Some(TimeInterval {
            start: UtcMicros(1),
            end: UtcMicros(2),
        }),
        ingested_at: UtcMicros(3),
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGenerationV3::Unknown,
        projection_generation: ProjectionGenerationId::new("projection.fixture").unwrap(),
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations: vec![],
        source_anchors: vec![source],
        authorization: authorization(),
        payload_access: PayloadAccessState::Eligible,
        retention_class: RetentionClass::new("retention.fixture").unwrap(),
        durability: AnchorDurabilityClass::DurableEvidence,
    };
    let record = RetrievalAnchorRecordV3::new(parts).unwrap();
    let wire = serde_json::to_value(&record).unwrap();

    assert_eq!(record.owner(), &owner);
    assert!(matches!(
        record.target(),
        RetrievalAnchorTargetV3::ExactSourceOccurrence(_)
    ));
    assert_eq!(
        serde_json::from_value::<RetrievalAnchorRecordV3>(wire).unwrap(),
        record
    );
}

#[test]
fn v3_record_rejects_cross_privacy_authorization() {
    let owner = v3_owner("project.fixture", "privacy.other");
    let source = AnchorLineageRefV3::new(
        0,
        AnchorProvenanceRelationV2::DerivedFrom,
        RetrievalAnchorId::new("retrieval.source.fixture").unwrap(),
        owner.clone(),
    )
    .unwrap();
    let parts = RetrievalAnchorRecordV3Parts {
        target: RetrievalAnchorTargetV3::ExactSourceOccurrence(
            SourceOccurrenceId::new("occurrence.fixture").unwrap(),
        ),
        owner,
        aliases: vec![],
        occurred_at: None,
        ingested_at: UtcMicros(1),
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGenerationV3::Unknown,
        projection_generation: ProjectionGenerationId::new("projection.fixture").unwrap(),
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations: vec![],
        source_anchors: vec![source],
        authorization: authorization(),
        payload_access: PayloadAccessState::Eligible,
        retention_class: RetentionClass::new("retention.fixture").unwrap(),
        durability: AnchorDurabilityClass::DurableEvidence,
    };

    assert_eq!(
        RetrievalAnchorRecordV3::new(parts).unwrap_err(),
        DomainError::UnknownReference {
            field: "retrieval anchor V3 authorization owner"
        }
    );
}

#[test]
fn rejects_alias_digest_collisions_across_alias_kinds() {
    let mut parts = record_parts(entity_target("document.fixture"), owner("project.fixture"));
    parts.aliases = vec![
        NativeAliasV2::new(
            NativeAliasKindV2::Path,
            PrivacyDomainBoundLocatorDigest::new(DIGEST_A).unwrap(),
        )
        .unwrap(),
        NativeAliasV2::new(
            NativeAliasKindV2::Ref,
            PrivacyDomainBoundLocatorDigest::new(DIGEST_A).unwrap(),
        )
        .unwrap(),
    ];

    assert_eq!(
        RetrievalAnchorRecordV2::new(parts).unwrap_err(),
        DomainError::DuplicateId {
            field: "retrieval anchor aliases"
        }
    );
}

#[test]
fn copied_lineage_does_not_reuse_source_anchor_identity() {
    let source = RetrievalAnchorRecordV2::new(record_parts(
        entity_target("document.source"),
        owner("project.fixture"),
    ))
    .unwrap();
    let mut copied_parts = record_parts(entity_target("document.copy"), owner("project.fixture"));
    copied_parts.source_anchors = vec![
        AnchorLineageRefV2::new(
            AnchorProvenanceRelationV2::CopiedFrom,
            source.anchor_id().clone(),
            owner("project.fixture"),
        )
        .unwrap(),
    ];
    let copied = RetrievalAnchorRecordV2::new(copied_parts).unwrap();

    assert_ne!(source.anchor_id(), copied.anchor_id());
    assert_eq!(
        copied.source_anchors()[0].relation(),
        AnchorProvenanceRelationV2::CopiedFrom
    );
}

#[test]
fn copied_prompt_attribution_survives_replay() {
    let source = RetrievalAnchorRecordV2::new(record_parts(
        entity_target("document.source"),
        owner("project.fixture"),
    ))
    .unwrap();
    let mut copied_parts = record_parts(entity_target("document.copy"), owner("project.fixture"));
    copied_parts.source_anchors = vec![
        AnchorLineageRefV2::new(
            AnchorProvenanceRelationV2::CopiedFrom,
            source.anchor_id().clone(),
            owner("project.fixture"),
        )
        .unwrap(),
    ];

    // Replaying the derivation from identical inputs is idempotent: the
    // copied-prompt identity is stable across re-derivation.
    let copied = RetrievalAnchorRecordV2::new(copied_parts.clone()).unwrap();
    let replayed = RetrievalAnchorRecordV2::new(copied_parts).unwrap();
    assert_eq!(copied.anchor_id(), replayed.anchor_id());

    // The copied identity stays distinct from the source it was copied
    // from, yet the replayed record retains the source in its lineage.
    assert_ne!(replayed.anchor_id(), source.anchor_id());
    assert_eq!(
        replayed.source_anchors()[0].relation(),
        AnchorProvenanceRelationV2::CopiedFrom
    );
    assert_eq!(replayed.source_anchors()[0].anchor_id(), source.anchor_id());
}

#[test]
fn repository_capture_requires_a_project_owner() {
    let capture_id = RepositoryCaptureId::new("capture.fixture").unwrap();
    let target = RetrievalAnchorTargetV2::RepositoryCapture {
        repository_id: RepositoryId::new("repository.fixture").unwrap(),
        capture_id: capture_id.clone(),
        receipt: SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.fixture").unwrap(),
            ComponentVersion::new("sanitizer.fixture").unwrap(),
        )
        .unwrap(),
    };
    let mut parts = record_parts(target, ObservationScopeV1::Profile);
    parts.source_generation = AnchorSourceGenerationV2::RepositoryCapture(capture_id);

    assert!(RetrievalAnchorRecordV2::new(parts).is_err());
}

#[test]
fn exact_git_targets_require_canonical_object_ids() {
    let mut parts = record_parts(
        RetrievalAnchorTargetV2::ExactRepositoryCommit {
            repository_id: RepositoryId::new("repository.fixture").unwrap(),
            commit_id: CommitId::new("main").unwrap(),
        },
        owner("project.fixture"),
    );
    parts.source_generation = AnchorSourceGenerationV2::Unknown;

    assert_eq!(
        RetrievalAnchorRecordV2::new(parts).unwrap_err(),
        DomainError::NonCanonical {
            field: "retrieval anchor commit"
        }
    );
}

#[test]
fn standalone_target_deserialization_enforces_git_identity() {
    let wire = json!({
        "kind": "exact_repository_commit",
        "target": {
            "repository_id": "repository.fixture",
            "commit_id": "not-a-git-object"
        }
    });

    assert!(serde_json::from_value::<RetrievalAnchorTargetV2>(wire).is_err());
}

#[test]
fn record_canonicalizes_and_bounds_source_collections() {
    let owner = owner("project.fixture");
    let alias_a = NativeAliasV2::new(
        NativeAliasKindV2::Path,
        PrivacyDomainBoundLocatorDigest::new(DIGEST_A).unwrap(),
    )
    .unwrap();
    let alias_b = NativeAliasV2::new(
        NativeAliasKindV2::Ref,
        PrivacyDomainBoundLocatorDigest::new(DIGEST_B).unwrap(),
    )
    .unwrap();
    let source_a = AnchorLineageRefV2::new(
        AnchorProvenanceRelationV2::Observed,
        RetrievalAnchorId::new("retrieval.a").unwrap(),
        owner.clone(),
    )
    .unwrap();
    let source_b = AnchorLineageRefV2::new(
        AnchorProvenanceRelationV2::Observed,
        RetrievalAnchorId::new("retrieval.b").unwrap(),
        owner.clone(),
    )
    .unwrap();
    let mut parts = record_parts(entity_target("document.fixture"), owner.clone());
    parts.aliases = vec![alias_b.clone(), alias_a.clone()];
    parts.source_observations = vec![observation('b'), observation('a')];
    parts.source_anchors = vec![source_b.clone(), source_a.clone()];
    let record = RetrievalAnchorRecordV2::new(parts).unwrap();

    assert_eq!(record.aliases(), &[alias_a.clone(), alias_b]);
    assert_eq!(
        record.source_observations(),
        &[observation('a'), observation('b')]
    );
    assert_eq!(record.source_anchors(), &[source_a, source_b.clone()]);

    let mut aliases = record_parts(entity_target("document.aliases"), owner.clone());
    aliases.aliases = vec![alias_a; MAX_ANCHOR_ALIASES + 1];
    assert!(matches!(
        RetrievalAnchorRecordV2::new(aliases),
        Err(DomainError::NonCanonical {
            field: "retrieval anchor aliases"
        })
    ));

    let mut observations = record_parts(entity_target("document.observations"), owner.clone());
    observations.source_observations = vec![observation('a'); MAX_ANCHOR_SOURCE_OBSERVATIONS + 1];
    assert!(matches!(
        RetrievalAnchorRecordV2::new(observations),
        Err(DomainError::NonCanonical {
            field: "retrieval anchor source observations"
        })
    ));

    let mut lineage = record_parts(entity_target("document.lineage"), owner);
    lineage.source_anchors = vec![source_b; MAX_ANCHOR_SOURCE_ANCHORS + 1];
    assert!(matches!(
        RetrievalAnchorRecordV2::new(lineage),
        Err(DomainError::NonCanonical {
            field: "retrieval anchor source lineage"
        })
    ));
}

#[test]
fn repository_capture_requires_the_matching_source_generation() {
    let target = RetrievalAnchorTargetV2::RepositoryCapture {
        repository_id: RepositoryId::new("repository.fixture").unwrap(),
        capture_id: RepositoryCaptureId::new("capture.target").unwrap(),
        receipt: SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.fixture").unwrap(),
            ComponentVersion::new("sanitizer.fixture").unwrap(),
        )
        .unwrap(),
    };
    let mut parts = record_parts(target, owner("project.fixture"));
    parts.source_generation = AnchorSourceGenerationV2::RepositoryCapture(
        RepositoryCaptureId::new("capture.other").unwrap(),
    );

    assert_eq!(
        RetrievalAnchorRecordV2::new(parts).unwrap_err(),
        DomainError::UnknownReference {
            field: "retrieval anchor source generation"
        }
    );
}

#[test]
fn deserialization_rejects_a_tampered_anchor_identity() {
    let record = RetrievalAnchorRecordV2::new(record_parts(
        entity_target("document.fixture"),
        owner("project.fixture"),
    ))
    .unwrap();
    let mut wire = serde_json::to_value(record).unwrap();
    wire["anchor_id"] = json!("retrieval.v2.tampered");

    assert!(serde_json::from_value::<RetrievalAnchorRecordV2>(wire).is_err());
}

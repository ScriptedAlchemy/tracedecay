use serde_json::json;

use super::*;
use crate::research::{
    AccessPolicyDigest, ComponentVersion, EntityId, EntityKind, PrivacyDomainId, ProjectId,
    SanitizationReceiptId, ScopeResolutionId,
};

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
    target: RetrievalAnchorTarget,
    owner: ObservationScopeV1,
) -> RetrievalAnchorRecordParts {
    let source_observations = match &target {
        RetrievalAnchorTarget::ExactObservation(id) => vec![id.clone()],
        _ => vec![observation('c')],
    };
    RetrievalAnchorRecordParts {
        target,
        owner,
        aliases: vec![],
        occurred_at: Some(TimeInterval {
            start: UtcMicros(1),
            end: UtcMicros(2),
        }),
        ingested_at: UtcMicros(3),
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGeneration::Observation(
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

fn entity_target(id: &str) -> RetrievalAnchorTarget {
    RetrievalAnchorTarget::Entity(EntityRef {
        id: EntityId::new(id).unwrap(),
        kind: EntityKind::Document,
    })
}

#[test]
fn assertion_provenance_relations_have_stable_snake_case_wire_values() {
    for (relation, expected) in [
        (AnchorProvenanceRelation::Corrects, "corrects"),
        (AnchorProvenanceRelation::Contradicts, "contradicts"),
        (AnchorProvenanceRelation::Supersedes, "supersedes"),
        (AnchorProvenanceRelation::Supports, "supports"),
    ] {
        assert_eq!(serde_json::to_value(relation).unwrap(), json!(expected));
        assert_eq!(
            serde_json::from_value::<AnchorProvenanceRelation>(json!(expected)).unwrap(),
            relation
        );
    }
}

#[test]
fn replay_derives_the_same_anchor_identity() {
    let first = RetrievalAnchorRecord::new(record_parts(
        entity_target("document.fixture"),
        owner("project.fixture"),
    ))
    .unwrap();
    let mut replay_parts =
        record_parts(entity_target("document.fixture"), owner("project.fixture"));
    replay_parts.ingested_at = UtcMicros(999);
    replay_parts.aliases = vec![
        NativeAlias::new(
            NativeAliasKind::Path,
            PrivacyDomainBoundLocatorDigest::new(DIGEST_A).unwrap(),
        )
        .unwrap(),
    ];
    let replay = RetrievalAnchorRecord::new(replay_parts).unwrap();

    assert_eq!(first.anchor_id(), replay.anchor_id());
}

#[test]
fn exact_observation_anchor_identity_ignores_projection_generation() {
    let observation_id = observation('a');
    let owner = owner("project.fixture");
    let expected = derive_exact_observation_anchor_id(&owner, &observation_id).unwrap();
    let mut parts = record_parts(
        RetrievalAnchorTarget::ExactObservation(observation_id.clone()),
        owner.clone(),
    );
    parts.source_observations = vec![observation_id];
    let first = RetrievalAnchorRecord::new(parts.clone()).unwrap();
    parts.projection_generation = ProjectionGenerationId::new("projection.rebuilt").unwrap();
    let rebuilt = RetrievalAnchorRecord::new(parts).unwrap();

    assert_eq!(first.anchor_id(), &expected);
    assert_eq!(rebuilt.anchor_id(), &expected);
}

#[test]
fn owner_is_part_of_anchor_identity() {
    let first = RetrievalAnchorRecord::new(record_parts(
        entity_target("document.fixture"),
        owner("project.one"),
    ))
    .unwrap();
    let second = RetrievalAnchorRecord::new(record_parts(
        entity_target("document.fixture"),
        owner("project.two"),
    ))
    .unwrap();

    assert_ne!(first.anchor_id(), second.anchor_id());
}

#[test]
fn rejects_alias_digest_collisions_across_alias_kinds() {
    let mut parts = record_parts(entity_target("document.fixture"), owner("project.fixture"));
    parts.aliases = vec![
        NativeAlias::new(
            NativeAliasKind::Path,
            PrivacyDomainBoundLocatorDigest::new(DIGEST_A).unwrap(),
        )
        .unwrap(),
        NativeAlias::new(
            NativeAliasKind::Ref,
            PrivacyDomainBoundLocatorDigest::new(DIGEST_A).unwrap(),
        )
        .unwrap(),
    ];

    assert_eq!(
        RetrievalAnchorRecord::new(parts).unwrap_err(),
        DomainError::DuplicateId {
            field: "retrieval anchor aliases"
        }
    );
}

#[test]
fn copied_lineage_does_not_reuse_source_anchor_identity() {
    let source = RetrievalAnchorRecord::new(record_parts(
        entity_target("document.source"),
        owner("project.fixture"),
    ))
    .unwrap();
    let mut copied_parts = record_parts(entity_target("document.copy"), owner("project.fixture"));
    copied_parts.source_anchors = vec![
        AnchorLineageRef::new(
            AnchorProvenanceRelation::CopiedFrom,
            source.anchor_id().clone(),
            owner("project.fixture"),
        )
        .unwrap(),
    ];
    let copied = RetrievalAnchorRecord::new(copied_parts).unwrap();

    assert_ne!(source.anchor_id(), copied.anchor_id());
    assert_eq!(
        copied.source_anchors()[0].relation(),
        AnchorProvenanceRelation::CopiedFrom
    );
}

#[test]
fn repository_capture_requires_a_project_owner() {
    let capture_id = RepositoryCaptureId::new("capture.fixture").unwrap();
    let target = RetrievalAnchorTarget::RepositoryCapture {
        repository_id: RepositoryId::new("repository.fixture").unwrap(),
        capture_id: capture_id.clone(),
        receipt: SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.fixture").unwrap(),
            ComponentVersion::new("sanitizer.fixture").unwrap(),
        )
        .unwrap(),
    };
    let mut parts = record_parts(target, ObservationScopeV1::Profile);
    parts.source_generation = AnchorSourceGeneration::RepositoryCapture(capture_id);

    assert!(RetrievalAnchorRecord::new(parts).is_err());
}

#[test]
fn exact_git_targets_require_canonical_object_ids() {
    let mut parts = record_parts(
        RetrievalAnchorTarget::ExactRepositoryCommit {
            repository_id: RepositoryId::new("repository.fixture").unwrap(),
            commit_id: CommitId::new("main").unwrap(),
        },
        owner("project.fixture"),
    );
    parts.source_generation = AnchorSourceGeneration::Unknown;

    assert_eq!(
        RetrievalAnchorRecord::new(parts).unwrap_err(),
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

    assert!(serde_json::from_value::<RetrievalAnchorTarget>(wire).is_err());
}

#[test]
fn record_canonicalizes_and_bounds_source_collections() {
    let owner = owner("project.fixture");
    let alias_a = NativeAlias::new(
        NativeAliasKind::Path,
        PrivacyDomainBoundLocatorDigest::new(DIGEST_A).unwrap(),
    )
    .unwrap();
    let alias_b = NativeAlias::new(
        NativeAliasKind::Ref,
        PrivacyDomainBoundLocatorDigest::new(DIGEST_B).unwrap(),
    )
    .unwrap();
    let source_a = AnchorLineageRef::new(
        AnchorProvenanceRelation::Observed,
        RetrievalAnchorId::new("retrieval.a").unwrap(),
        owner.clone(),
    )
    .unwrap();
    let source_b = AnchorLineageRef::new(
        AnchorProvenanceRelation::Observed,
        RetrievalAnchorId::new("retrieval.b").unwrap(),
        owner.clone(),
    )
    .unwrap();
    let mut parts = record_parts(entity_target("document.fixture"), owner.clone());
    parts.aliases = vec![alias_b.clone(), alias_a.clone()];
    parts.source_observations = vec![observation('b'), observation('a')];
    parts.source_anchors = vec![source_b.clone(), source_a.clone()];
    let record = RetrievalAnchorRecord::new(parts).unwrap();

    assert_eq!(record.aliases(), &[alias_a.clone(), alias_b]);
    assert_eq!(
        record.source_observations(),
        &[observation('a'), observation('b')]
    );
    assert_eq!(record.source_anchors(), &[source_a, source_b.clone()]);

    let mut aliases = record_parts(entity_target("document.aliases"), owner.clone());
    aliases.aliases = vec![alias_a; MAX_ANCHOR_ALIASES + 1];
    assert!(matches!(
        RetrievalAnchorRecord::new(aliases),
        Err(DomainError::NonCanonical {
            field: "retrieval anchor aliases"
        })
    ));

    let mut observations = record_parts(entity_target("document.observations"), owner.clone());
    observations.source_observations = vec![observation('a'); MAX_ANCHOR_SOURCE_OBSERVATIONS + 1];
    assert!(matches!(
        RetrievalAnchorRecord::new(observations),
        Err(DomainError::NonCanonical {
            field: "retrieval anchor source observations"
        })
    ));

    let mut lineage = record_parts(entity_target("document.lineage"), owner);
    lineage.source_anchors = vec![source_b; MAX_ANCHOR_SOURCE_ANCHORS + 1];
    assert!(matches!(
        RetrievalAnchorRecord::new(lineage),
        Err(DomainError::NonCanonical {
            field: "retrieval anchor source lineage"
        })
    ));
}

#[test]
fn repository_capture_requires_the_matching_source_generation() {
    let target = RetrievalAnchorTarget::RepositoryCapture {
        repository_id: RepositoryId::new("repository.fixture").unwrap(),
        capture_id: RepositoryCaptureId::new("capture.target").unwrap(),
        receipt: SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.fixture").unwrap(),
            ComponentVersion::new("sanitizer.fixture").unwrap(),
        )
        .unwrap(),
    };
    let mut parts = record_parts(target, owner("project.fixture"));
    parts.source_generation = AnchorSourceGeneration::RepositoryCapture(
        RepositoryCaptureId::new("capture.other").unwrap(),
    );

    assert_eq!(
        RetrievalAnchorRecord::new(parts).unwrap_err(),
        DomainError::UnknownReference {
            field: "retrieval anchor source generation"
        }
    );
}

#[test]
fn deserialization_rejects_a_tampered_anchor_identity() {
    let record = RetrievalAnchorRecord::new(record_parts(
        entity_target("document.fixture"),
        owner("project.fixture"),
    ))
    .unwrap();
    let mut wire = serde_json::to_value(record).unwrap();
    wire["anchor_id"] = json!("retrieval.v2.tampered");

    assert!(serde_json::from_value::<RetrievalAnchorRecord>(wire).is_err());
}

#[test]
fn stored_anchor_omits_derivable_authorization_and_default_coverage() {
    let mut parts = record_parts(
        RetrievalAnchorTarget::ExactObservation(observation('a')),
        owner("project.fixture"),
    );
    parts.authorization = ResolutionAuthorizationV1::for_authority(
        "observation-capture.v1",
        PrivacyDomainBoundLocatorDigest::new(DIGEST_B).unwrap(),
    )
    .unwrap();
    let derived = RetrievalAnchorRecord::new(parts).unwrap();
    let encoded = serde_json::to_value(&derived).unwrap();
    for omitted in [
        "coverage",
        "aliases",
        "projection_watermark",
        "source_anchors",
    ] {
        assert!(encoded.get(omitted).is_none(), "{omitted}: {encoded}");
    }
    assert_eq!(
        encoded["authorization"],
        json!({"authority": "observation-capture.v1", "canonical_request_digest": DIGEST_B})
    );
    assert_eq!(
        serde_json::from_value::<RetrievalAnchorRecord>(encoded).unwrap(),
        derived
    );

    // A fixture authorization is not its namespace's derivation, so it keeps
    // every field.
    let explicit = RetrievalAnchorRecord::new(record_parts(
        RetrievalAnchorTarget::ExactObservation(observation('a')),
        owner("project.fixture"),
    ))
    .unwrap();
    let encoded = serde_json::to_value(&explicit).unwrap();
    assert_eq!(
        encoded["authorization"],
        serde_json::to_value(authorization()).unwrap()
    );
    assert_eq!(
        serde_json::from_value::<RetrievalAnchorRecord>(encoded).unwrap(),
        explicit
    );
}

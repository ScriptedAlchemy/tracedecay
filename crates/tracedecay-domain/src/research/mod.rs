//! Immutable research-provenance and retrieval-anchor contracts.
//!
//! This module is a compatibility facade. Ownership-aligned implementation
//! modules remain directly addressable while all existing
//! `tracedecay_domain::research::Type` imports continue to resolve.

pub mod anchor;
pub mod branch_stack;
pub mod canonical;
mod canonical_serializer;
mod canonical_sink;
mod canonical_value;
pub mod coverage;
pub mod error;
pub mod evidence;
pub mod git_topology;
pub mod id;
pub mod manifest;
pub mod resolution;
pub mod retrieval;
pub mod subjects;
pub mod time;
pub mod watermark;

pub use anchor::*;
pub use branch_stack::*;
pub use canonical::*;
pub use coverage::*;
pub use error::*;
pub use evidence::*;
pub use git_topology::*;
pub use id::*;
pub use manifest::*;
pub use resolution::*;
pub use retrieval::*;
pub use subjects::*;
pub use time::*;
pub use watermark::*;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;

    const SHA256_FIXTURE: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String, Error = DomainError>,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn snapshot() -> CatalogSnapshotRefV1 {
        CatalogSnapshotRefV1 {
            generation: id("catalog.fixture.v1"),
            digest: id(SHA256_FIXTURE),
        }
    }

    fn watermark() -> VectorWatermark {
        VectorWatermark {
            components: BTreeMap::from([(id("shard.fixture"), 7)]),
        }
    }

    fn seal_catalog(catalog: &mut RetrievalAnchorCatalogV1) {
        catalog.snapshot.digest = catalog.compute_digest().unwrap();
        for record in catalog.records.values_mut() {
            record.capability_catalog = catalog.snapshot.clone();
        }
    }

    fn valid_retrieval_anchor_record() -> RetrievalAnchorRecordV1 {
        let document = EntityRef {
            id: id("document.fixture"),
            kind: EntityKind::Document,
        };
        RetrievalAnchorRecordV1 {
            anchor_id: id("retrieval.fixture"),
            target: RetrievalAnchorTargetV1::Entity(document.clone()),
            target_kind: EntityKind::Document,
            resolved_scope_id: id("scope.fixture"),
            privacy_domain_id: id("privacy.fixture"),
            access_policy_digest: id(SHA256_FIXTURE),
            source_identity_class: SourceIdentityClass::ProjectEvidence,
            immutable_source_refs: vec![document],
            source_observations: vec![id("observation.fixture")],
            snapshot: watermark(),
            schema_registry_digest: id(SHA256_FIXTURE),
            capability_catalog: CatalogSnapshotRefV1 {
                generation: id("catalog.fixture.v1"),
                digest: id(SHA256_FIXTURE),
            },
            data_version_digest: id(SHA256_FIXTURE),
            projection_version: id("projection.fixture.v1"),
            view_algorithm_version: None,
            view: RetrievalViewV1::EntityVersion,
            expansion_recipe: RetrievalExpansionRecipeV1 {
                capability_id: id("capability.research.exact"),
                expansion: RetrievalExpansionMode::ExactTarget,
                bounded_arguments_digest: id(SHA256_FIXTURE),
            },
            canonical_request_digest: id(SHA256_FIXTURE),
            provenance: vec![id("provenance.fixture")],
            payload_access: PayloadAccessState::Eligible,
            retention_class: RetentionClass::new("retention.fixture").unwrap(),
            created_at: UtcMicros(1),
            durability: AnchorDurabilityClass::DurableEvidence,
        }
    }

    fn valid_envelope() -> ResearchBundleEnvelopeV1 {
        let anchor_id: RetrievalAnchorId = id("retrieval.fixture");
        let entry_id: ResearchAnchorId = id("entry.fixture");
        let recipe_id: RetrievalRecipeId = id("recipe.fixture");
        let catalog_snapshot = snapshot();
        let snapshot_watermark = watermark();
        let document = EntityRef {
            id: id("document.fixture"),
            kind: EntityKind::Document,
        };
        let record = RetrievalAnchorRecordV1 {
            anchor_id: anchor_id.clone(),
            target: RetrievalAnchorTargetV1::Entity(document.clone()),
            target_kind: EntityKind::Document,
            resolved_scope_id: id("scope.fixture"),
            privacy_domain_id: id("privacy.fixture"),
            access_policy_digest: id(SHA256_FIXTURE),
            source_identity_class: SourceIdentityClass::ProjectEvidence,
            immutable_source_refs: vec![document.clone()],
            source_observations: vec![id("observation.fixture")],
            snapshot: snapshot_watermark.clone(),
            schema_registry_digest: id(SHA256_FIXTURE),
            capability_catalog: catalog_snapshot.clone(),
            data_version_digest: id(SHA256_FIXTURE),
            projection_version: id("projection.fixture.v1"),
            view_algorithm_version: None,
            view: RetrievalViewV1::EntityVersion,
            expansion_recipe: RetrievalExpansionRecipeV1 {
                capability_id: id("capability.research.exact"),
                expansion: RetrievalExpansionMode::ExactTarget,
                bounded_arguments_digest: id(SHA256_FIXTURE),
            },
            canonical_request_digest: id(SHA256_FIXTURE),
            provenance: vec![id("provenance.fixture")],
            payload_access: PayloadAccessState::Eligible,
            retention_class: RetentionClass::new("retention.fixture").unwrap(),
            created_at: UtcMicros(1),
            durability: AnchorDurabilityClass::DurableEvidence,
        };
        let mut catalog = RetrievalAnchorCatalogV1 {
            snapshot: catalog_snapshot,
            records: BTreeMap::from([(anchor_id.clone(), record)]),
        };
        seal_catalog(&mut catalog);
        let catalog_snapshot = catalog.snapshot.clone();
        let anchors = NonEmptyUniqueVec::new(vec![anchor_id.clone()], "fixture anchors").unwrap();
        let recipe = RetrievalRecipeV1 {
            recipe_id: recipe_id.clone(),
            use_case: id("usecase.research.fixture"),
            anchors: anchors.clone(),
            purpose: evidence::test_fixtures::log_safe_text("Resolve fixture evidence."),
            snapshot: snapshot_watermark.clone(),
        };
        let anchor = ResearchContextAnchorV1 {
            entry_id,
            retrieval_anchors: anchors,
            purpose: evidence::test_fixtures::log_safe_text("Bind fixture evidence."),
            subject: ResearchAnchorSubjectV1::Document(DocumentResearchSubjectV1 {
                document: document.clone(),
                version: None,
            }),
            related_activity: None,
            occurred_window: None,
            source_observation_ids: vec![id("observation.fixture")],
            evidence_class: EvidenceClass::Observed,
            confidence: Confidence::new(1.0).unwrap(),
            expected_subject: evidence::test_fixtures::log_safe_text("Fixture document."),
            retrieval_recipe_id: recipe_id,
            snapshot: snapshot_watermark.clone(),
            coverage: CoverageReportV1::default(),
        };
        let mut manifest = ResearchBundleManifestV1 {
            manifest_id: id("manifest.fixture"),
            schema_version: id("research.v1"),
            supersedes: None,
            created_at: UtcMicros(1),
            created_by: ActorRef {
                actor_id: id("actor.fixture"),
                version: None,
            },
            parent_plan: EntityRef {
                id: id("plan.fixture"),
                kind: EntityKind::Plan,
            },
            repository: id("repository.fixture"),
            base_commit: id("commit.fixture"),
            plan_commit: None,
            catalog_snapshot,
            store_watermarks: snapshot_watermark,
            private_corpus: None,
            git_snapshot: GitTruthManifest {
                repository: id("repository.fixture"),
                head_commit: id("commit.fixture"),
                merge_base: None,
                refs: vec![],
                dirty: false,
                captured_at: UtcMicros(1),
            },
            anchors: vec![anchor],
            agent_contributions: vec![],
            unresolved_attribution: vec![],
            retrieval_recipes: vec![recipe],
            redaction_report: RedactionReport {
                sanitizer_version: id("fixture.sanitizer.v1"),
                scanned: 1,
                redacted: 0,
                rejected: 0,
                receipts: vec![id("fixture.sanitization-receipt")],
            },
            digest: id(SHA256_FIXTURE),
        };
        manifest.digest = manifest.compute_digest().unwrap();
        ResearchBundleEnvelopeV1 {
            manifest,
            retrieval_catalog: catalog,
        }
    }

    #[test]
    fn ids_reject_invalid_deserialized_values() {
        assert!(serde_json::from_str::<ShardId>("\"\"").is_err());
        assert!(serde_json::from_str::<ShardId>("\" shard.fixture\"").is_err());
        assert!(serde_json::from_value::<ShardId>(json!("shard\nfixture")).is_err());
        assert!(serde_json::from_value::<ShardId>(json!("x".repeat(513))).is_err());
        assert_eq!(
            serde_json::from_str::<ShardId>("\"shard.fixture\"")
                .unwrap()
                .as_str(),
            "shard.fixture"
        );
    }

    #[test]
    fn source_subject_rejects_inverted_byte_range() {
        let subject = ResearchAnchorSubjectV1::Source(SourceResearchSubjectV1 {
            source_store_id: id(
                "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            ),
            source_entity: EntityRef {
                id: id("source-record.fixture"),
                kind: EntityKind::SourceRecord,
            },
            source_position: Some(SourcePosition::ByteOffset {
                start: 100,
                end: 10,
            }),
        });

        assert_eq!(
            subject.validate(),
            Err(DomainError::UnknownReference {
                field: "source position byte range",
            })
        );
    }

    #[test]
    fn owner_modules_and_compatibility_facades_resolve_the_same_ids() {
        let owned: crate::research::id::ShardId =
            crate::research::id::ShardId::new("shard.fixture").unwrap();
        let research_facade: crate::research::ShardId = owned.clone();
        let crate_facade: crate::ShardId = research_facade.clone();

        assert_eq!(owned, research_facade);
        assert_eq!(research_facade, crate_facade);
    }

    #[test]
    fn constrained_anchor_collections_reject_empty_and_duplicates() {
        type Anchors = NonEmptyUniqueVec<RetrievalAnchorId>;

        assert!(serde_json::from_value::<Anchors>(json!([])).is_err());
        assert!(serde_json::from_value::<Anchors>(json!(["retrieval.a", "retrieval.a"])).is_err());

        let anchors =
            serde_json::from_value::<Anchors>(json!(["retrieval.a", "retrieval.b"])).unwrap();
        assert_eq!(anchors.len(), 2);
        assert_eq!(anchors[0].as_str(), "retrieval.a");
    }

    #[test]
    fn sanitization_safety_requires_an_explicit_receipt_proof_boundary() {
        assert!(serde_json::from_value::<LogSafeText>(json!("raw text")).is_err());
        assert!(serde_json::from_value::<SanitizedTextV1>(json!("raw text")).is_err());
        assert!(serde_json::from_value::<SanitizationProofV1>(json!("raw proof")).is_err());
        assert!(
            serde_json::from_value::<SanitizationProofV1>(json!({
                "receipt_id": "fixture.sanitization-receipt",
                "sanitizer_version": "fixture.sanitizer.v1"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<LogSafeText>(json!({ "value": "missing receipt" })).is_err()
        );

        let value = evidence::test_fixtures::log_safe_text("receipt-bound text");
        let serialized = serde_json::to_value(&value).unwrap();
        assert_eq!(
            serialized["receipt"]["receipt_id"],
            json!("fixture.sanitization-receipt")
        );
        assert!(serde_json::from_value::<LogSafeText>(serialized).is_err());
    }

    #[test]
    fn grouped_coverage_wire_deserializes_into_one_disposition_map() {
        let coverage: CoverageReportV1 = serde_json::from_value(json!({
            "searched": ["shard.a"],
            "skipped": [],
            "stale": [],
            "unavailable": [],
            "incompatible": [],
            "locked": [],
            "redacted": [],
            "truncated": [],
            "freshness": {},
            "unknown_coverage": false
        }))
        .unwrap();
        assert_eq!(
            coverage.disposition(&id("shard.a")),
            Some(ShardDispositionV1::Searched)
        );
        assert!(coverage.is_complete());
        let serialized = serde_json::to_string(&coverage).unwrap();
        assert_eq!(
            serialized,
            r#"{"searched":["shard.a"],"skipped":[],"stale":[],"unavailable":[],"incompatible":[],"locked":[],"redacted":[],"truncated":[],"freshness":{},"unknown_coverage":false}"#
        );
        assert_eq!(
            serde_json::from_str::<CoverageReportV1>(&serialized).unwrap(),
            coverage
        );

        assert!(
            serde_json::from_value::<CoverageReportV1>(json!({
                "searched": ["shard.a"],
                "stale": ["shard.a"]
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<CoverageReportV1>(json!({
                "searched": ["shard.a"],
                "future_coverage_field": true
            }))
            .is_err()
        );
    }

    #[test]
    fn coverage_rejects_retired_remote_authority_metadata() {
        let error = serde_json::from_value::<CoverageReportV1>(json!({
            "searched": ["shard.a"],
            "remote": {}
        }))
        .expect_err("retired remote metadata must not be accepted");

        assert!(error.to_string().contains("unknown field `remote`"));
    }

    #[test]
    fn coverage_completeness_requires_an_explicit_nonempty_searched_universe() {
        let default_report = CoverageReportV1::default();
        assert_eq!(
            default_report.universe,
            CoverageUniverseKnowledgeV1::Unknown
        );
        assert!(!default_report.is_complete());

        let omitted_universe: CoverageReportV1 = serde_json::from_value(json!({
            "searched": ["shard.a"]
        }))
        .unwrap();
        assert_eq!(
            omitted_universe.universe,
            CoverageUniverseKnowledgeV1::Unknown
        );
        assert!(!omitted_universe.is_complete());

        let empty_known_universe: CoverageReportV1 = serde_json::from_value(json!({
            "unknown_coverage": false
        }))
        .unwrap();
        assert!(!empty_known_universe.is_complete());

        let skipped_only: CoverageReportV1 = serde_json::from_value(json!({
            "skipped": ["shard.a"],
            "unknown_coverage": false
        }))
        .unwrap();
        assert!(!skipped_only.is_complete());
    }

    #[test]
    fn manifest_requires_external_snapshot_pinned_catalog() {
        let envelope = valid_envelope();
        envelope.validate().unwrap();

        let mut missing = envelope.clone();
        missing.retrieval_catalog.records.clear();
        seal_catalog(&mut missing.retrieval_catalog);
        missing.manifest.catalog_snapshot = missing.retrieval_catalog.snapshot.clone();
        assert!(matches!(
            missing.manifest.validate(&missing.retrieval_catalog),
            Err(DomainError::UnknownReference { .. })
        ));

        let mut wrong_snapshot = envelope;
        wrong_snapshot.retrieval_catalog.snapshot.generation = id("catalog.other");
        seal_catalog(&mut wrong_snapshot.retrieval_catalog);
        assert!(matches!(
            wrong_snapshot
                .manifest
                .validate(&wrong_snapshot.retrieval_catalog),
            Err(DomainError::SnapshotMismatch { .. })
        ));
    }

    #[test]
    fn manifest_parent_plan_requires_plan_kind() {
        let mut envelope = valid_envelope();
        envelope.manifest.parent_plan.kind = EntityKind::Artifact;

        assert_eq!(
            envelope.manifest.validate_structure(),
            Err(DomainError::UnknownReference {
                field: "parent_plan.kind",
            })
        );
    }

    #[test]
    fn retrieval_anchor_rejects_incoherent_query_target_kind() {
        let mut record = valid_retrieval_anchor_record();
        record.target = RetrievalAnchorTargetV1::Query(id("query.fixture"));

        assert_eq!(
            record.validate(),
            Err(DomainError::UnknownReference {
                field: "retrieval anchor query target_kind",
            })
        );
    }

    #[test]
    fn retrieval_anchor_rejects_incoherent_source_position_target_kind() {
        let mut record = valid_retrieval_anchor_record();
        record.target = RetrievalAnchorTargetV1::SourcePosition {
            source: id("source.fixture"),
            position_digest: id(
                "sha256:1111111111111111111111111111111111111111111111111111111111111111",
            ),
        };

        assert_eq!(
            record.validate(),
            Err(DomainError::UnknownReference {
                field: "retrieval anchor source position target_kind",
            })
        );
    }

    #[test]
    fn retrieval_catalog_digest_rejects_record_mutation() {
        let mut envelope = valid_envelope();
        let anchor_id = envelope.manifest.retrieval_recipes[0].anchors[0].clone();
        envelope
            .retrieval_catalog
            .records
            .get_mut(&anchor_id)
            .unwrap()
            .canonical_request_digest =
            id("sha256:1111111111111111111111111111111111111111111111111111111111111111");

        assert!(matches!(
            envelope.validate(),
            Err(DomainError::SnapshotMismatch {
                field: "retrieval catalog snapshot digest"
            })
        ));
    }

    #[test]
    fn retrieval_recipe_rejects_catalog_record_at_a_different_snapshot() {
        let mut envelope = valid_envelope();
        let anchor_id = envelope.manifest.retrieval_recipes[0].anchors[0].clone();
        envelope.manifest.anchors.clear();
        envelope
            .retrieval_catalog
            .records
            .get_mut(&anchor_id)
            .unwrap()
            .snapshot = VectorWatermark {
            components: BTreeMap::from([(id("shard.fixture"), 6)]),
        };
        seal_catalog(&mut envelope.retrieval_catalog);
        envelope.manifest.catalog_snapshot = envelope.retrieval_catalog.snapshot.clone();

        assert!(matches!(
            envelope.manifest.validate(&envelope.retrieval_catalog),
            Err(DomainError::SnapshotMismatch {
                field: "recipe retrieval record snapshot"
            })
        ));
    }

    #[test]
    fn manifest_digest_is_domain_separated_excludes_itself_and_detects_mutation() {
        let mut envelope = valid_envelope();
        envelope.manifest.verify_digest().unwrap();
        let first = envelope.manifest.compute_digest().unwrap();
        assert_eq!(first, envelope.manifest.compute_digest().unwrap());

        envelope.manifest.digest =
            id("sha256:1111111111111111111111111111111111111111111111111111111111111111");
        assert_eq!(first, envelope.manifest.compute_digest().unwrap());
        assert_eq!(
            envelope.manifest.verify_digest(),
            Err(DomainError::DigestMismatch)
        );

        envelope.manifest.digest = first;
        envelope.manifest.created_at = UtcMicros(2);
        assert_eq!(
            envelope.manifest.verify_digest(),
            Err(DomainError::DigestMismatch)
        );
    }

    #[test]
    fn canonical_json_sorts_object_keys_recursively() {
        assert_eq!(
            canonical_json_value(&json!({"z": {"b": 1, "a": 2}, "a": 0})).unwrap(),
            r#"{"a":0,"z":{"a":2,"b":1}}"#
        );
    }
}

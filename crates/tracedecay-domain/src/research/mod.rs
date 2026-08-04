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

    fn remote_coverage_json(shard_count: usize) -> String {
        let shards = (0..shard_count)
            .map(|index| {
                json!({
                    "shard_id": format!("shard.{index}"),
                    "authority_id": "authority.fixture",
                    "authority_epoch": 1,
                    "served_by_node": "node.fixture",
                    "served_by_role": "authority",
                    "captured_watermark": null,
                    "cache_generation": null,
                    "cache_not_after": null,
                    "cache_age_micros": null,
                    "cache_grant_snapshot": null,
                    "sync_lag_micros": null,
                    "pending_local_observations": 0,
                    "pending_tombstone_acks": 0
                })
            })
            .collect::<Vec<_>>();
        serde_json::to_string(&json!({
            "brain_id": "brain.fixture",
            "placement_version": "placement.fixture.v1",
            "evaluated_at": 1,
            "requested_consistency": "authoritative",
            "shards": shards
        }))
        .unwrap()
    }

    #[test]
    fn coverage_wire_objects_reject_unknown_fields() {
        assert!(
            serde_json::from_value::<ReadConsistencyV1>(json!({
                "bounded_stale": {
                    "max_lag_micros": 10,
                    "future_consistency_field": true
                }
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<EvidenceRetentionWatermark>(json!({
                "evaluated_at": 1,
                "cutoffs": {},
                "future_retention_field": true
            }))
            .is_err()
        );

        let mut remote =
            serde_json::from_str::<serde_json::Value>(&remote_coverage_json(1)).unwrap();
        remote["future_remote_field"] = json!(true);
        assert!(serde_json::from_value::<RemoteCoverageV1>(remote).is_err());

        let mut shard =
            serde_json::from_str::<serde_json::Value>(&remote_coverage_json(1)).unwrap();
        shard["shards"][0]["future_shard_field"] = json!(true);
        assert!(serde_json::from_value::<RemoteCoverageV1>(shard).is_err());

        let report = offline_cache_report(99, 100);
        let mut serialized = serde_json::to_value(&report).unwrap();
        serialized["remote"]["shards"][0]["cache_grant_snapshot"]["future_grant_field"] =
            json!(true);
        assert!(serde_json::from_value::<CoverageReportV1>(serialized).is_err());
    }

    #[test]
    fn remote_coverage_accepts_exact_shard_bound() {
        let remote: RemoteCoverageV1 = serde_json::from_str(&remote_coverage_json(1_024)).unwrap();
        assert_eq!(remote.shards.len(), 1_024);
    }

    #[test]
    fn remote_coverage_rejects_shard_bound_plus_one() {
        let error = serde_json::from_str::<RemoteCoverageV1>(&remote_coverage_json(1_025))
            .expect_err("remote shard coverage above the bound must be rejected");
        assert!(
            error
                .to_string()
                .contains("a sequence with at most 1024 elements"),
            "unexpected error: {error}"
        );
    }

    fn offline_cache_report(evaluated_at: i64, cache_not_after: i64) -> CoverageReportV1 {
        const SHA256_FIXTURE: &str =
            "sha256:0000000000000000000000000000000000000000000000000000000000000000";

        CoverageReportV1 {
            dispositions: BTreeMap::from([(id("shard.a"), ShardDispositionV1::Searched)]),
            universe: CoverageUniverseKnowledgeV1::Known,
            remote: Some(RemoteCoverageV1 {
                brain_id: id("brain.fixture"),
                placement_version: id("placement.fixture.v1"),
                evaluated_at: UtcMicros(evaluated_at),
                requested_consistency: ReadConsistencyV1::OfflineCache,
                shards: BoundedVec::try_from(vec![RemoteShardCoverageV1 {
                    shard_id: id("shard.a"),
                    authority_id: id("authority.fixture"),
                    authority_epoch: AuthorityEpoch(1),
                    served_by_node: id("node.fixture"),
                    served_by_role: BrainNodeRoleV1::RemoteClient,
                    captured_watermark: None,
                    cache_generation: Some(id(SHA256_FIXTURE)),
                    cache_not_after: Some(UtcMicros(cache_not_after)),
                    cache_age_micros: Some(10),
                    cache_grant_snapshot: Some(VerifiedCacheGrantSnapshotV1 {
                        grant_digest: id(SHA256_FIXTURE),
                        issued_at: UtcMicros(1),
                        not_after: UtcMicros(cache_not_after),
                        grant_revocation_generation: 7,
                        purge_frontier: VectorWatermark {
                            components: BTreeMap::from([(id("shard.a"), 7)]),
                        },
                        verified_placement_version: Some(id("placement.fixture.v1")),
                        verified_authority_id: Some(id("authority.fixture")),
                        verified_authority_epoch: Some(AuthorityEpoch(1)),
                        verified_revocation_generation: Some(7),
                        verified_purge_frontier: Some(VectorWatermark {
                            components: BTreeMap::from([(id("shard.a"), 7)]),
                        }),
                    }),
                    sync_lag_micros: None,
                    pending_local_observations: 0,
                    pending_tombstone_acks: 0,
                }])
                .expect("single remote shard is within the coverage bound"),
            }),
            ..CoverageReportV1::default()
        }
    }

    #[test]
    fn offline_cache_coverage_rejects_an_expired_grant() {
        let report = offline_cache_report(101, 100);
        report.validate().unwrap();
        assert!(!report.is_complete());
    }

    #[test]
    fn offline_cache_coverage_uses_an_exclusive_clock_boundary() {
        let mut report = offline_cache_report(99, 100);
        report.validate().unwrap();
        assert_eq!(
            serde_json::to_value(&report).unwrap()["remote"]["evaluated_at"],
            json!(99)
        );
        assert!(report.is_complete());

        report.remote.as_mut().unwrap().evaluated_at = UtcMicros(100);
        assert!(!report.is_complete());

        let remote = report.remote.as_mut().unwrap();
        remote.evaluated_at = UtcMicros(99);
        remote.shards[0].cache_not_after = Some(UtcMicros(101));
        assert!(!report.is_complete());
    }

    #[test]
    fn offline_cache_coverage_rejects_a_revoked_grant() {
        let mut report = offline_cache_report(99, 100);
        assert!(report.is_complete());
        report.remote.as_mut().unwrap().shards[0]
            .cache_grant_snapshot
            .as_mut()
            .unwrap()
            .verified_revocation_generation = Some(8);
        assert!(!report.is_complete());
    }

    #[test]
    fn offline_cache_coverage_requires_current_placement_and_authority_evidence() {
        let mut report = offline_cache_report(99, 100);
        assert!(report.is_complete());
        let snapshot = report.remote.as_mut().unwrap().shards[0]
            .cache_grant_snapshot
            .as_mut()
            .unwrap();
        snapshot.verified_placement_version = None;
        assert!(!report.is_complete());

        let snapshot = report.remote.as_mut().unwrap().shards[0]
            .cache_grant_snapshot
            .as_mut()
            .unwrap();
        snapshot.verified_placement_version = Some(id("placement.fixture.v1"));
        snapshot.verified_authority_id = None;
        snapshot.verified_authority_epoch = None;
        assert!(!report.is_complete());
    }

    #[test]
    fn offline_cache_coverage_rejects_pending_purge() {
        let mut report = offline_cache_report(99, 100);
        assert!(report.is_complete());
        let snapshot = report.remote.as_mut().unwrap().shards[0]
            .cache_grant_snapshot
            .as_mut()
            .unwrap();
        snapshot.verified_purge_frontier = Some(VectorWatermark {
            components: BTreeMap::from([(id("shard.a"), 6)]),
        });
        assert!(!report.is_complete());
    }

    #[test]
    fn coverage_rejects_detail_shards_without_a_canonical_disposition() {
        let freshness_without_disposition = serde_json::from_value::<CoverageReportV1>(json!({
            "searched": ["shard.a"],
            "freshness": {
                "shard.b": {
                    "shard_id": "shard.b",
                    "outbox_sequence": 7
                }
            }
        }));
        assert!(freshness_without_disposition.is_err());

        let report = CoverageReportV1 {
            dispositions: BTreeMap::from([(id("shard.a"), ShardDispositionV1::Searched)]),
            remote: Some(RemoteCoverageV1 {
                brain_id: id("brain.fixture"),
                placement_version: id("placement.fixture.v1"),
                evaluated_at: UtcMicros(1),
                requested_consistency: ReadConsistencyV1::Authoritative,
                shards: BoundedVec::try_from(vec![RemoteShardCoverageV1 {
                    shard_id: id("shard.b"),
                    authority_id: id("authority.fixture"),
                    authority_epoch: AuthorityEpoch(1),
                    served_by_node: id("node.fixture"),
                    served_by_role: BrainNodeRoleV1::Authority,
                    captured_watermark: Some(ShardWatermark {
                        shard_id: id("shard.b"),
                        outbox_sequence: 7,
                    }),
                    cache_generation: None,
                    cache_not_after: None,
                    cache_age_micros: None,
                    cache_grant_snapshot: None,
                    sync_lag_micros: None,
                    pending_local_observations: 0,
                    pending_tombstone_acks: 0,
                }])
                .expect("single remote shard is within the coverage bound"),
            }),
            ..CoverageReportV1::default()
        };
        assert!(matches!(
            report.validate(),
            Err(DomainError::UnknownReference {
                field: "remote coverage disposition shard"
            })
        ));
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

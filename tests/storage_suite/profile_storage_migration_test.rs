use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
#[cfg(feature = "test-transport")]
use tracedecay::application::memory::{
    MemoryApplication, MemoryOperationContext, automation_fact_proposal_add_command,
};
#[cfg(feature = "test-transport")]
use tracedecay::automation::fact_proposals::{
    FactProposalRecord, FactProposalState, FactProposalStore, fact_proposals_path,
    import_legacy_fact_proposals,
};
use tracedecay::branch::BranchAddOutcome;
use tracedecay::branch_meta::{self, BranchMeta};
use tracedecay::config::{TraceDecayConfig, USER_DATA_DIR_ENV};
use tracedecay::global_db::{GraphScopeUpsert, StoreArtifactUpsert, StoreInstanceUpsert};
#[cfg(feature = "test-transport")]
use tracedecay::memory::types::{
    AddFactRequest, FactRelationKind, FeedbackAction, FeedbackRequest, MemoryCategory,
    MemoryGroomingOperation, UpdateFactRequest,
};
use tracedecay::migrate::inventory::{
    MigrationInventory, RegistryStatus, StoreArtifact, StoreBrand, StoreInventory, StoreRole,
    StoreStatus,
};
use tracedecay::migrate::manifest::{
    MigrationPlanOptions, apply_migration_manifest, build_plan_manifest, finalize_migration_apply,
    verify_migration_manifest,
};
#[cfg(feature = "test-transport")]
use tracedecay::migrate::memory_cutover::MemoryCutoverOptions;
use tracedecay::migrate::registry::{
    RegistryReconstructionReport, RegistryReconstructionStatus,
    reconstruct_registry_from_store_manifest, scan_profile_store_manifests,
};
use tracedecay::serve;
use tracedecay::storage::{
    EnrollmentMarker, STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode,
    StoreKind, StoreManifest, read_enrollment_marker, write_enrollment_marker,
    write_repository_identity_marker,
};
#[cfg(feature = "test-transport")]
use tracedecay::store::memory::DatabaseFactStore;
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_domain::ProjectId;
#[cfg(feature = "test-transport")]
use tracedecay_domain::{
    AccessPolicyDigest, AnchorDurabilityClass, AnchorSourceGenerationV2, CapabilityId,
    ComponentVersion, Confidence, CoverageReportV1, DomainError, EntityId, EntityKind, EntityRef,
    EvidenceClass, FactAssertionKindV1, FactAssertionV1, FactCategoryV1, FactEvidenceRefV1,
    FactEvidenceRelationV1, FactId, FactIdentityMaterialV1, FactIdentitySourceV1,
    FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1, NativeAliasKindV2,
    NativeAliasV2, PayloadAccessState, PayloadReferenceV1, PrivacyDomainBoundLocatorDigest,
    PrivacyDomainId, ProjectionGenerationId, ProvenanceId, ResolutionAuthorizationV1,
    RetentionClass, RetrievalAnchorId, RetrievalAnchorRecordV2, RetrievalAnchorRecordV2Parts,
    RetrievalAnchorTargetV2, SanitizationReceiptId, SanitizationReceiptRefV1,
    SanitizationReceiptV1, SanitizerDispositionV1, ScopeResolutionId, SensitivityV1, UtcMicros,
    VectorWatermark,
};
#[cfg(feature = "test-transport")]
use tracedecay_store::{
    AnchorDispositionReasonClassV1, AnchorDispositionStateV1,
    CompatibilityFactFeedbackHistoryQueryV1, CompatibilityFactIdV1, CompatibilityFactTargetV1,
    FactCommitOutcome, FactCurrentQuery, FactLineageQuery, FactWriteBatch,
    RetrievalAnchorDispositionRecordV1, RetrievalAnchorDispositionStore, RetrievalAnchorOwnerV1,
};

use crate::common::EnvVarGuard;
use crate::support::{HOME_ENV_LOCK, ephemeral_safe_fixture_base};

#[cfg(feature = "test-transport")]
const ARCHIVE_DIGEST_A: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
#[cfg(feature = "test-transport")]
const ARCHIVE_DIGEST_B: &str =
    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[cfg(feature = "test-transport")]
fn archive_id<T>(value: &str) -> T
where
    T: TryFrom<String, Error = DomainError>,
{
    T::try_from(value.to_owned()).unwrap()
}

#[cfg(feature = "test-transport")]
async fn seed_public_archive_fact(
    branch: &Path,
    owner: FactOwnerV1,
) -> (FactId, String, FactId, i64, RetrievalAnchorId) {
    let (database, _) = crate::common::open_test_database(branch).await.unwrap();
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&database)).unwrap();
    let operation = "project-memory-cutover-public";
    let identity = FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Application {
            operation_id: archive_id("operation.project-memory-cutover-public"),
        },
    )
    .unwrap();
    let fact_id = FactId::derive(&identity).unwrap();
    let anchor = RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target: RetrievalAnchorTargetV2::Entity(EntityRef {
            id: EntityId::new("entity.project-memory-cutover-public".to_owned()).unwrap(),
            kind: EntityKind::Document,
        }),
        owner: owner.clone().into(),
        aliases: vec![
            NativeAliasV2::new(
                NativeAliasKindV2::Path,
                PrivacyDomainBoundLocatorDigest::new(ARCHIVE_DIGEST_A).unwrap(),
            )
            .unwrap(),
        ],
        occurred_at: None,
        ingested_at: UtcMicros(10),
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGenerationV2::Unknown,
        projection_generation: ProjectionGenerationId::new(
            "projection.project-memory-cutover-public".to_owned(),
        )
        .unwrap(),
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations: vec![],
        source_anchors: vec![],
        authorization: ResolutionAuthorizationV1 {
            resolved_scope_id: ScopeResolutionId::new(
                "scope.project-memory-cutover-public".to_owned(),
            )
            .unwrap(),
            privacy_domain_id: PrivacyDomainId::new(
                "privacy.project-memory-cutover-public".to_owned(),
            )
            .unwrap(),
            access_policy_digest: AccessPolicyDigest::new(ARCHIVE_DIGEST_A).unwrap(),
            capability_id: CapabilityId::new("capability.project-memory-cutover-public".to_owned())
                .unwrap(),
            canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(ARCHIVE_DIGEST_B)
                .unwrap(),
        },
        payload_access: PayloadAccessState::Eligible,
        retention_class: RetentionClass::new("retention.project-memory-cutover-public".to_owned())
            .unwrap(),
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .unwrap();
    let content = "public writer branch fact survives retirement".to_owned();
    let material = serde_json::json!({
        "content": content,
        "category": "project",
        "tags": ["cutover"],
        "entities": ["TraceDecay"],
        "metadata": {"source": operation},
    });
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            archive_id::<SanitizationReceiptId>("receipt.project-memory-cutover-public"),
            archive_id::<ComponentVersion>("sanitizer.project-memory-cutover.v1"),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&material).unwrap()),
    )
    .unwrap();
    let payload = FactPayloadV1::new(
        content.clone(),
        FactCategoryV1::Project,
        vec!["cutover".to_owned()],
        vec!["TraceDecay".to_owned()],
        serde_json::json!({"source": operation}),
        receipt,
        RetentionClass::new("retention.project-memory-cutover-public".to_owned()).unwrap(),
    )
    .unwrap();
    let evidence = FactEvidenceRefV1::new(
        fact_id.clone(),
        anchor.anchor_id().clone(),
        FactEvidenceRelationV1::Supports,
        EvidenceClass::Observed,
        Confidence::new(1.0).unwrap(),
    )
    .unwrap();
    let assertion = FactAssertionV1::new(
        fact_id.clone(),
        owner.clone(),
        FactAssertionKindV1::Initial,
        payload,
        vec![evidence],
        UtcMicros(10),
        None,
    )
    .unwrap();
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        UtcMicros(10),
        None,
    )
    .unwrap();
    let source_anchor_id = anchor.anchor_id().clone();
    let batch = FactWriteBatch::new(
        fact_id.clone(),
        owner.clone(),
        Some(assertion),
        vec![event],
        vec![anchor],
        vec![],
        None,
        None,
    )
    .unwrap()
    .with_identity_material(identity)
    .unwrap();
    assert!(matches!(
        memory.commit_fact(batch).await.unwrap(),
        FactCommitOutcome::Committed(_)
    ));
    let legacy_fact = memory
        .add_fact_v1(
            AddFactRequest {
                content: "authorized purge V2-only closure".to_owned(),
                category: MemoryCategory::Project,
                source: Some("project-memory-cutover-test".to_owned()),
                tags: vec!["cutover".to_owned()],
                entities: vec!["TraceDecay".to_owned()],
                trust: Some(0.8),
                metadata: serde_json::json!({"fixture": "authorized-purge"}),
            },
            MemoryOperationContext::generated(&owner, "cutover-add", None).unwrap(),
        )
        .await
        .unwrap()
        .fact
        .expect("compatibility add returns its legacy mirror");
    memory
        .update_fact_v1(
            UpdateFactRequest {
                fact_id: legacy_fact.fact_id,
                content: Some("updated authoritative cutover closure".to_owned()),
                category: None,
                tags: Some(vec!["cutover".to_owned(), "superseded".to_owned()]),
                entities: None,
                trust: Some(0.85),
                source: None,
                metadata: None,
            },
            MemoryOperationContext::generated(&owner, "cutover-update", None).unwrap(),
        )
        .await
        .unwrap();
    memory
        .record_fact_feedback_v1(
            FeedbackRequest {
                fact_id: legacy_fact.fact_id,
                action: FeedbackAction::Helpful,
                source: Some("project-memory-cutover-test".to_owned()),
                note: Some("preserve feedback history".to_owned()),
            },
            MemoryOperationContext::generated(&owner, "cutover-feedback", None).unwrap(),
        )
        .await
        .unwrap();
    let related_fact = memory
        .add_fact_v1(
            AddFactRequest {
                content: "related authoritative cutover closure".to_owned(),
                category: MemoryCategory::Project,
                source: Some("project-memory-cutover-test".to_owned()),
                tags: vec!["relation".to_owned()],
                entities: vec!["TraceDecay".to_owned()],
                trust: Some(0.8),
                metadata: serde_json::json!({"fixture": "relation-target"}),
            },
            MemoryOperationContext::generated(&owner, "cutover-related-add", None).unwrap(),
        )
        .await
        .unwrap()
        .fact
        .unwrap();
    memory
        .dashboard_apply_grooming_v1(
            vec![MemoryGroomingOperation::LinkFacts {
                source_fact_id: legacy_fact.fact_id,
                target_fact_id: related_fact.fact_id,
                relation: FactRelationKind::Supports,
                evidence_fact_ids: vec![related_fact.fact_id],
                confidence: 0.9,
                source: "project-memory-cutover-test".to_owned(),
                metadata: serde_json::json!({"fixture": "relation"}),
            }],
            0.5,
            MemoryOperationContext::generated(&owner, "cutover-curation", None).unwrap(),
        )
        .await
        .unwrap();
    let proposal_id = ProvenanceId::new("proposal.project-memory-cutover".to_owned()).unwrap();
    let proposal_command = automation_fact_proposal_add_command(
        owner.clone(),
        AddFactRequest {
            content: "proposed authoritative cutover closure".to_owned(),
            category: MemoryCategory::Project,
            source: Some("project-memory-cutover-test".to_owned()),
            tags: vec!["proposal".to_owned()],
            entities: vec!["TraceDecay".to_owned()],
            trust: Some(0.8),
            metadata: serde_json::json!({"fixture": "proposal"}),
        },
        "run.project-memory-cutover",
        proposal_id.as_str(),
        None,
    )
    .unwrap();
    memory
        .submit_compatibility_fact_proposal(proposal_id, proposal_command, None)
        .await
        .unwrap();
    let legacy_proposal_root = branch.parent().unwrap().join("legacy-proposals");
    let legacy_proposals = FactProposalStore {
        schema_version: 1,
        proposals: vec![FactProposalRecord {
            schema_version: 1,
            proposal_id: "legacy-proposal-project-memory-cutover".to_owned(),
            run_id: "legacy-run-project-memory-cutover".to_owned(),
            evidence_hash: None,
            state: FactProposalState::PendingApproval,
            add_fact_request: Some(AddFactRequest {
                content: "imported legacy proposal closure".to_owned(),
                category: MemoryCategory::Project,
                source: Some("project-memory-cutover-test".to_owned()),
                tags: vec!["legacy-proposal".to_owned()],
                entities: vec!["TraceDecay".to_owned()],
                trust: Some(0.8),
                metadata: serde_json::json!({"fixture": "legacy-proposal"}),
            }),
            proposal: None,
            validation_reason: None,
            validation: None,
            reviewer: None,
            applied_canonical_fact_id: None,
            applied_fact_id: None,
            apply_outcome: None,
            created_at: 1,
            updated_at: 1,
            duplicate_count: 0,
            last_duplicate_run_id: None,
            folded_contents: Vec::new(),
        }],
    };
    tokio::fs::create_dir_all(&legacy_proposal_root)
        .await
        .unwrap();
    tokio::fs::write(
        fact_proposals_path(&legacy_proposal_root),
        serde_json::to_vec(&legacy_proposals).unwrap(),
    )
    .await
    .unwrap();
    import_legacy_fact_proposals(&memory, &legacy_proposal_root)
        .await
        .unwrap();
    let before_purge = DatabaseFactStore::new(&database)
        .inspect_owner_archive_for_test(&owner)
        .await
        .unwrap();
    let purged_fact_id = before_purge
        .records()
        .iter()
        .find_map(|record| {
            if record.family() != tracedecay_store::MemoryV2ArchiveFamilyV1::LegacyFactMap
                || !matches!(
                    record.key().get("legacy_fact_id"),
                    Some(tracedecay_store::MemoryV2ArchiveScalarV1::Integer(value))
                        if *value == legacy_fact.fact_id
                )
            {
                return None;
            }
            match record.fields().get("fact_id") {
                Some(tracedecay_store::MemoryV2ArchiveScalarV1::Text(value)) => {
                    Some(FactId::new(value.clone()).unwrap())
                }
                _ => None,
            }
        })
        .expect("production compatibility writer publishes its stable V2 mapping");
    memory
        .remove_fact_v1(
            legacy_fact.fact_id,
            MemoryOperationContext::generated(&owner, "cutover-remove", None).unwrap(),
        )
        .await
        .unwrap();
    database.close();
    for entry in fs::read_dir(branch.parent().unwrap()).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".tracedecay-test-profile-"))
        {
            fs::remove_file(path).unwrap();
        }
    }
    (
        fact_id,
        content,
        purged_fact_id,
        legacy_fact.fact_id,
        source_anchor_id,
    )
}

#[cfg(feature = "test-transport")]
async fn seed_production_evidence_assembly(
    branch: &Path,
    owner: &FactOwnerV1,
    project_id: ProjectId,
) {
    let (source_anchor, write) = tracedecay_rusqlite_runtime::repository::write_fixture_for_project(
        "retriever.project-memory-cutover.v1",
        project_id,
    );
    let (database, _) = crate::common::open_test_database(branch).await.unwrap();
    let existing = DatabaseFactStore::new(&database)
        .inspect_owner_archive_for_test(owner)
        .await
        .unwrap();
    let anchor_id = tracedecay_store::MemoryV2ArchiveScalarV1::Text(
        source_anchor.anchor_id().as_str().to_owned(),
    );
    let mut key = std::collections::BTreeMap::new();
    key.insert("anchor_id".to_owned(), anchor_id);
    let mut fields = std::collections::BTreeMap::new();
    fields.insert(
        "anchor_json".to_owned(),
        tracedecay_store::MemoryV2ArchiveScalarV1::Text(
            serde_json::to_string(&source_anchor).unwrap(),
        ),
    );
    fields.insert(
        "owner_json".to_owned(),
        tracedecay_store::MemoryV2ArchiveScalarV1::Text(
            serde_json::to_string(source_anchor.owner()).unwrap(),
        ),
    );
    fields.insert(
        "projection_generation".to_owned(),
        tracedecay_store::MemoryV2ArchiveScalarV1::Text(
            source_anchor.projection_generation().as_str().to_owned(),
        ),
    );
    let mut records = existing.records().to_vec();
    records.push(
        tracedecay_store::MemoryV2ArchiveRecordV1::new(
            tracedecay_store::MemoryV2ArchiveFamilyV1::RetrievalAnchor,
            key,
            fields,
            Vec::new(),
        )
        .unwrap(),
    );
    let payload = records
        .iter()
        .find(|record| {
            record.family() == tracedecay_store::MemoryV2ArchiveFamilyV1::AssertionPayload
        })
        .unwrap();
    let mut vector_fields = std::collections::BTreeMap::new();
    vector_fields.insert(
        "vector".to_owned(),
        tracedecay_store::MemoryV2ArchiveScalarV1::Blob(vec![0, 0, 128, 63]),
    );
    vector_fields.insert(
        "algebra".to_owned(),
        tracedecay_store::MemoryV2ArchiveScalarV1::Text(
            "project-memory-cutover-fixture".to_owned(),
        ),
    );
    vector_fields.insert(
        "dimensions".to_owned(),
        tracedecay_store::MemoryV2ArchiveScalarV1::Integer(1),
    );
    vector_fields.insert(
        "precision".to_owned(),
        tracedecay_store::MemoryV2ArchiveScalarV1::Text("f32".to_owned()),
    );
    records.push(
        tracedecay_store::MemoryV2ArchiveRecordV1::new(
            tracedecay_store::MemoryV2ArchiveFamilyV1::AssertionVector,
            payload.key().clone(),
            vector_fields,
            vec![
                tracedecay_store::MemoryV2ArchiveReferenceV1::new(
                    tracedecay_store::MemoryV2ArchiveFamilyV1::AssertionPayload,
                    payload.key().clone(),
                )
                .unwrap(),
            ],
        )
        .unwrap(),
    );
    let (owner_kind, owner_project_id) = match owner {
        FactOwnerV1::Profile => ("profile", String::new()),
        FactOwnerV1::Project { project_id } => ("project", project_id.as_str().to_owned()),
    };
    let mut quarantine_key = std::collections::BTreeMap::new();
    quarantine_key.insert(
        "owner_kind".to_owned(),
        tracedecay_store::MemoryV2ArchiveScalarV1::Text(owner_kind.to_owned()),
    );
    quarantine_key.insert(
        "project_id".to_owned(),
        tracedecay_store::MemoryV2ArchiveScalarV1::Text(owner_project_id),
    );
    quarantine_key.insert(
        "source_store_id".to_owned(),
        tracedecay_store::MemoryV2ArchiveScalarV1::Text("source.project-memory-cutover".to_owned()),
    );
    quarantine_key.insert(
        "source_table".to_owned(),
        tracedecay_store::MemoryV2ArchiveScalarV1::Text("memory_oplog".to_owned()),
    );
    quarantine_key.insert(
        "source_row_id".to_owned(),
        tracedecay_store::MemoryV2ArchiveScalarV1::Integer(9001),
    );
    let mut quarantine_fields = std::collections::BTreeMap::new();
    quarantine_fields.insert(
        "reason_code".to_owned(),
        tracedecay_store::MemoryV2ArchiveScalarV1::Text("fixture-invalid-row".to_owned()),
    );
    quarantine_fields.insert(
        "recorded_at".to_owned(),
        tracedecay_store::MemoryV2ArchiveScalarV1::Integer(1),
    );
    records.push(
        tracedecay_store::MemoryV2ArchiveRecordV1::new(
            tracedecay_store::MemoryV2ArchiveFamilyV1::LegacyQuarantine,
            quarantine_key,
            quarantine_fields,
            Vec::new(),
        )
        .unwrap(),
    );
    let archive = tracedecay_store::MemoryV2OwnerArchiveV1::new(
        owner.clone(),
        tracedecay_store::authoritative_memory_v2_archive_families(),
        records,
    )
    .unwrap();
    DatabaseFactStore::new(&database)
        .import_owner_archive_for_test(&archive)
        .await
        .unwrap();
    database.close();
    for entry in fs::read_dir(branch.parent().unwrap()).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".tracedecay-test-profile-"))
        {
            fs::remove_file(path).unwrap();
        }
    }

    let mut connection = rusqlite::Connection::open(branch).unwrap();
    let mut transaction = connection.transaction().unwrap();
    let savepoint = transaction.savepoint().unwrap();
    tracedecay_rusqlite_runtime::repository::ProjectExecutor::default()
        .execute_evidence_assembly_write(&savepoint, &write)
        .unwrap();
    savepoint.commit().unwrap();
    transaction.commit().unwrap();
    drop(connection);

    let (database, _) = crate::common::open_test_database(branch).await.unwrap();
    RetrievalAnchorDispositionStore::append_disposition(
        &database,
        RetrievalAnchorDispositionRecordV1::new(
            "disposition.project-memory-cutover",
            source_anchor.anchor_id().clone(),
            RetrievalAnchorOwnerV1::V3(write.owner.owner.clone()),
            AnchorDispositionStateV1::Deleted,
            None,
            AnchorDispositionReasonClassV1::UserRequest,
            UtcMicros(3),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    database.close();
    for entry in fs::read_dir(branch.parent().unwrap()).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".tracedecay-test-profile-"))
        {
            fs::remove_file(path).unwrap();
        }
    }
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn project_memory_cutover_unions_all_branches_and_accepts_v18() {
    let _lock = HOME_ENV_LOCK.lock().await;
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn cutover_fixture() {}\n").unwrap();
    run_git(&project, &["init", "-b", "main"]);
    run_git(&project, &["config", "user.email", "test@example.com"]);
    run_git(&project, &["config", "user.name", "TraceDecay Test"]);
    run_git(&project, &["add", "."]);
    run_git(&project, &["commit", "-m", "initial"]);
    let options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(profile_root.join("global.db")),
    };
    let initialized = init_with_maintenance(&project, &profile_root, options.clone())
        .await
        .unwrap();
    let data_root = initialized.store_layout().data_root.clone();
    let project_graph = initialized.store_layout().graph_db_path.clone();
    let lifecycle = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "checkpoint project memory cutover fixture",
    )
    .unwrap();
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "checkpoint project memory cutover fixture",
    )
    .unwrap();
    initialized
        .db()
        .truncate_wal_for_test_artifact()
        .await
        .unwrap();
    initialized.close();
    drop(_database_scope);
    drop(lifecycle);

    let branches = data_root.join("branches");
    fs::create_dir_all(&branches).unwrap();
    let stale = branches.join("stale-v18.db");
    let current = branches.join("current.db");
    fs::copy(&project_graph, &stale).unwrap();
    fs::copy(&project_graph, &current).unwrap();
    let stale_db = rusqlite::Connection::open(&stale).unwrap();
    stale_db
        .execute_batch(
            "PRAGMA user_version = 18;
             INSERT INTO memory_facts(
                 fact_id, content, category, tags, trust_score, access_count,
                 created_at, updated_at, source, metadata
             ) VALUES
                 (1, 'shared cutover fact', 'project', '[\"stale\"]', 0.5, 1,
                  10, 10, 'stale-v18', '{}'),
                 (2, 'stale branch exclusive', 'decision', '[]', 0.8, 2,
                  10, 10, 'stale-v18', '{}');
             INSERT INTO memory_feedback_events(
                 event_id, fact_id, action, trust_delta, old_trust, new_trust,
                 created_at, source, note
             ) VALUES(1, 1, 'helpful', 0.1, 0.4, 0.5, 10, 'fixture', 'kept');",
        )
        .unwrap();
    drop(stale_db);
    let current_db = rusqlite::Connection::open(&current).unwrap();
    current_db
        .execute_batch(
            "INSERT INTO memory_facts(
                 fact_id, content, category, tags, trust_score, access_count,
                 created_at, updated_at, source, metadata, hrr_precision
             ) VALUES
                 (1, 'shared cutover fact', 'project', '[\"current\"]', 0.9, 7,
                  5, 20, 'current', '{\"winner\":\"current\"}', 'f32'),
                 (3, 'current branch exclusive', 'tool', '[]', 0.7, 3,
                  20, 20, 'current', '{}', 'f32');
             INSERT INTO memory_oplog(id, ts, op, fact_id, detail_json)
             VALUES(1, 20, 'add', 3, '{}');",
        )
        .unwrap();
    drop(current_db);
    let mut meta = branch_meta::load_branch_meta(&data_root).unwrap();
    meta.add_branch("stale-v18", "branches/stale-v18.db", "main");
    meta.add_branch("current", "branches/current.db", "main");
    branch_meta::save_branch_meta(&data_root, &meta).unwrap();

    let cutover = MemoryCutoverOptions {
        project_root: project.clone(),
        profile_root: profile_root.clone(),
    };
    let planned = tracedecay::migrate::memory_cutover::plan(&cutover)
        .await
        .unwrap();
    assert_eq!(planned.sources.len(), 2);
    assert!(
        planned
            .sources
            .iter()
            .any(|source| source.user_version == 18)
    );
    let applied = tracedecay::migrate::memory_cutover::apply(&cutover, &planned.confirmation_token)
        .await
        .unwrap();
    assert!(applied.applied);
    assert!(data_root.join("memory-branch-cutover.json").is_file());

    let target = rusqlite::Connection::open_with_flags(
        &project_graph,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let shared: (f64, i64, String, String) = target
        .query_row(
            "SELECT trust_score, access_count, tags, metadata
             FROM memory_facts WHERE content = 'shared cutover fact'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(shared.0, 0.9);
    assert_eq!(shared.1, 7);
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&shared.2).unwrap(),
        vec!["current", "stale"]
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&shared.3).unwrap()["winner"],
        "current"
    );
    assert_eq!(
        target
            .query_row("SELECT COUNT(*) FROM memory_facts", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        3
    );
    assert_eq!(
        target
            .query_row("SELECT COUNT(*) FROM memory_feedback_events", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    assert_eq!(
        target
            .query_row("SELECT COUNT(*) FROM memory_oplog", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    assert_eq!(
        target
            .query_row(
                "SELECT phase FROM memory_v2_backfill_progress
                 WHERE owner_kind='project' AND source_store_id='legacy-memory-v1'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "cutover_complete"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn project_memory_cutover_preserves_v2_authority_after_legacy_reclamation() {
    let _lock = HOME_ENV_LOCK.lock().await;
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub fn v2_cutover_fixture() {}\n",
    )
    .unwrap();
    run_git(&project, &["init", "-b", "main"]);
    run_git(&project, &["config", "user.email", "test@example.com"]);
    run_git(&project, &["config", "user.name", "TraceDecay Test"]);
    run_git(&project, &["add", "."]);
    run_git(&project, &["commit", "-m", "initial"]);
    let options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(profile_root.join("global.db")),
    };
    let initialized = init_with_maintenance(&project, &profile_root, options)
        .await
        .unwrap();
    let data_root = initialized.store_layout().data_root.clone();
    let project_graph = initialized.store_layout().graph_db_path.clone();
    let project_id = read_enrollment_marker(&project)
        .unwrap()
        .unwrap()
        .project_id;
    let owner = FactOwnerV1::Project {
        project_id: ProjectId::new(project_id.clone()).unwrap(),
    };
    let lifecycle = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "checkpoint V2 project memory cutover fixture",
    )
    .unwrap();
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "checkpoint V2 project memory cutover fixture",
    )
    .unwrap();
    initialized
        .db()
        .truncate_wal_for_test_artifact()
        .await
        .unwrap();
    initialized.close();
    drop(_database_scope);
    drop(lifecycle);

    let branches = data_root.join("branches");
    fs::create_dir_all(&branches).unwrap();
    let branch = branches.join("v2-authority.db");
    fs::copy(&project_graph, &branch).unwrap();
    let (public_fact_id, public_fact_content, purged_fact_id, purged_legacy_id, _) =
        seed_public_archive_fact(&branch, owner.clone()).await;
    seed_production_evidence_assembly(&branch, &owner, ProjectId::new(project_id.clone()).unwrap())
        .await;
    let (source_database, _) = crate::common::open_test_database_read_only(&branch)
        .await
        .unwrap();
    let source_archive = DatabaseFactStore::new(&source_database)
        .inspect_owner_archive_for_test(&owner)
        .await
        .unwrap();
    source_database.close();
    for entry in fs::read_dir(branch.parent().unwrap()).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".tracedecay-test-profile-"))
        {
            fs::remove_file(path).unwrap();
        }
    }
    let source_records = source_archive.records().to_vec();
    let mut meta = branch_meta::load_branch_meta(&data_root).unwrap();
    meta.add_branch("v2-authority", "branches/v2-authority.db", "main");
    branch_meta::save_branch_meta(&data_root, &meta).unwrap();

    let cutover = MemoryCutoverOptions {
        project_root: project,
        profile_root,
    };
    let planned = tracedecay::migrate::memory_cutover::plan(&cutover)
        .await
        .unwrap();
    let applied = tracedecay::migrate::memory_cutover::apply(&cutover, &planned.confirmation_token)
        .await
        .unwrap();
    assert!(applied.applied);
    let receipt_path = data_root.join("memory-branch-cutover.json");
    let receipt_bytes = fs::read(&receipt_path).unwrap();
    let mut wrong_project_receipt: serde_json::Value =
        serde_json::from_slice(&receipt_bytes).unwrap();
    wrong_project_receipt["project_id"] = serde_json::json!("proj_other");
    fs::write(
        &receipt_path,
        serde_json::to_vec_pretty(&wrong_project_receipt).unwrap(),
    )
    .unwrap();
    assert!(
        tracedecay::migrate::memory_cutover::verify_branch_removal_receipts(
            &data_root,
            std::slice::from_ref(&branch),
            &[],
        )
        .is_err(),
        "a receipt copied from another project must fail closed"
    );
    fs::write(&receipt_path, receipt_bytes).unwrap();
    tracedecay::migrate::memory_cutover::verify_branch_removal_receipts(
        &data_root,
        std::slice::from_ref(&branch),
        &[],
    )
    .unwrap();
    fs::remove_file(&branch).unwrap();

    let (project_database, _) = crate::common::open_test_database_read_only(&project_graph)
        .await
        .unwrap();
    let project_memory =
        MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&project_database)).unwrap();
    let public_fact = project_memory
        .query_fact_current(FactCurrentQuery::new(owner.clone(), public_fact_id.clone()).unwrap())
        .await
        .unwrap()
        .expect("public writer fact remains readable after branch source deletion");
    assert_eq!(public_fact.fact_id(), &public_fact_id);
    assert_eq!(
        public_fact.payload().map(FactPayloadV1::content),
        Some(public_fact_content.as_str())
    );
    let public_lineage = project_memory
        .query_fact_lineage(
            FactLineageQuery::new(owner.clone(), public_fact_id.clone(), None, 100).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(public_lineage.len(), 1);
    assert_eq!(public_lineage[0].fact_id(), &public_fact_id);
    let tombstone_lineage = project_memory
        .query_fact_lineage(
            FactLineageQuery::new(owner.clone(), purged_fact_id.clone(), None, 100).unwrap(),
        )
        .await
        .unwrap();
    assert!(
        tombstone_lineage.len() >= 3,
        "public lineage must preserve assertion supersession and terminal history"
    );
    let feedback = project_memory
        .get_compatibility_feedback_history(
            CompatibilityFactFeedbackHistoryQueryV1::new(
                CompatibilityFactTargetV1::Canonical(
                    CompatibilityFactIdV1::new(owner.clone(), purged_fact_id.clone()).unwrap(),
                ),
                None,
                100,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(feedback.events().len(), 1);
    let proposals = project_memory
        .list_compatibility_fact_proposals(None, None, 100)
        .await
        .unwrap();
    assert!(
        proposals.proposals().iter().any(|proposal| {
            proposal.proposal_id().as_str() == "proposal.project-memory-cutover"
        })
    );
    assert!(proposals.proposals().iter().any(|proposal| {
        proposal
            .automation_run_id()
            .is_some_and(|run_id| run_id == "legacy-run-project-memory-cutover")
    }));
    // Public MemoryApplication reads cover stable identity/current payload and
    // lineage, feedback history, and proposal state. Evidence assembly,
    // retrieval disposition, relation, vector, quarantine, and compatibility
    // mapping families do not expose one project-wide aggregate read, so the
    // typed archive inspection below is the narrow adapter contract for them.
    let archive = DatabaseFactStore::new(&project_database)
        .inspect_owner_archive_for_test(&owner)
        .await
        .unwrap();
    let populated_families = archive
        .records()
        .iter()
        .map(tracedecay_store::MemoryV2ArchiveRecordV1::family)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        populated_families,
        tracedecay_store::authoritative_memory_v2_archive_families(),
        "the production-writer fixture must populate every authoritative archive family"
    );
    for source in &source_records {
        if source.family() == tracedecay_store::MemoryV2ArchiveFamilyV1::CurrentFact {
            continue;
        }
        assert!(
            archive.records().contains(source),
            "stable owner-scoped archive record was not preserved: {:?} {:?}",
            source.family(),
            source.key()
        );
    }
    project_database.close();

    let has_text =
        |record: &tracedecay_store::MemoryV2ArchiveRecordV1, column: &str, expected: &str| {
            matches!(
                record
                    .key()
                    .get(column)
                    .or_else(|| record.fields().get(column)),
                Some(tracedecay_store::MemoryV2ArchiveScalarV1::Text(value))
                    if value == expected
            )
        };
    let records_for_fact = |family: tracedecay_store::MemoryV2ArchiveFamilyV1, fact_id: &str| {
        archive
            .records()
            .iter()
            .filter(|record| record.family() == family && has_text(record, "fact_id", fact_id))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        records_for_fact(
            tracedecay_store::MemoryV2ArchiveFamilyV1::Fact,
            purged_fact_id.as_str()
        )
        .len(),
        1,
        "authorized legacy purge must retain the original V2 identity"
    );
    assert_eq!(
        records_for_fact(
            tracedecay_store::MemoryV2ArchiveFamilyV1::LegacyFactMap,
            purged_fact_id.as_str()
        )
        .iter()
        .filter(|record| {
            matches!(
                record.key().get("legacy_fact_id"),
                Some(tracedecay_store::MemoryV2ArchiveScalarV1::Integer(value))
                    if *value == purged_legacy_id
            )
        })
        .count(),
        1,
        "authorized legacy purge must not erase its stable V2 mapping"
    );
    for (family, expected) in [
        (tracedecay_store::MemoryV2ArchiveFamilyV1::Fact, 1),
        (tracedecay_store::MemoryV2ArchiveFamilyV1::Assertion, 1),
        (
            tracedecay_store::MemoryV2ArchiveFamilyV1::AssertionPayload,
            1,
        ),
        (tracedecay_store::MemoryV2ArchiveFamilyV1::LineageEvent, 1),
        (tracedecay_store::MemoryV2ArchiveFamilyV1::FactEvidence, 1),
        (
            tracedecay_store::MemoryV2ArchiveFamilyV1::AssertionEvidence,
            1,
        ),
        (tracedecay_store::MemoryV2ArchiveFamilyV1::CurrentFact, 1),
    ] {
        assert_eq!(
            records_for_fact(family, public_fact_id.as_str()).len(),
            expected,
            "{family:?} production-writer closure must survive branch retirement"
        );
    }
    assert_eq!(
        records_for_fact(
            tracedecay_store::MemoryV2ArchiveFamilyV1::CurrentFact,
            purged_fact_id.as_str()
        )
        .iter()
        .filter(|record| has_text(record, "payload_access", "deleted"))
        .count(),
        1
    );
    assert_eq!(
        archive
            .records()
            .iter()
            .filter(|record| {
                record.family() == tracedecay_store::MemoryV2ArchiveFamilyV1::LegacyFactMap
                    && has_text(record, "source_store_id", "legacy-memory-v1")
                    && has_text(record, "fact_id", purged_fact_id.as_str())
                    && matches!(
                        record.key().get("legacy_fact_id"),
                        Some(tracedecay_store::MemoryV2ArchiveScalarV1::Integer(value))
                            if *value == purged_legacy_id
                    )
            })
            .count(),
        1,
        "stale legacy map identity must remain bound to its stable V2 FactId"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn project_memory_cutover_rejects_incompatible_v2_identity_without_receipt() {
    let _lock = HOME_ENV_LOCK.lock().await;
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("project");
    let profile_root = temp.path().join("profile");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub fn v2_conflict_fixture() {}\n",
    )
    .unwrap();
    run_git(&project, &["init", "-b", "main"]);
    run_git(&project, &["config", "user.email", "test@example.com"]);
    run_git(&project, &["config", "user.name", "TraceDecay Test"]);
    run_git(&project, &["add", "."]);
    run_git(&project, &["commit", "-m", "initial"]);
    let initialized = init_with_maintenance(
        &project,
        &profile_root,
        TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        },
    )
    .await
    .unwrap();
    let data_root = initialized.store_layout().data_root.clone();
    let project_graph = initialized.store_layout().graph_db_path.clone();
    let project_id = read_enrollment_marker(&project)
        .unwrap()
        .unwrap()
        .project_id;
    let owner_json = serde_json::to_string(&tracedecay_domain::FactOwnerV1::Project {
        project_id: ProjectId::new(project_id.clone()).unwrap(),
    })
    .unwrap();
    let lifecycle = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        &profile_root,
        "checkpoint V2 conflict fixture",
    )
    .unwrap();
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        &profile_root,
        "checkpoint V2 conflict fixture",
    )
    .unwrap();
    initialized
        .db()
        .truncate_wal_for_test_artifact()
        .await
        .unwrap();
    initialized.close();
    drop(_database_scope);
    drop(lifecycle);

    let branches = data_root.join("branches");
    fs::create_dir_all(&branches).unwrap();
    let branch = branches.join("v2-conflict.db");
    fs::copy(&project_graph, &branch).unwrap();
    for (path, identity) in [
        (&project_graph, r#"{"content":"target"}"#),
        (&branch, r#"{"content":"source"}"#),
    ] {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
            .execute(
                "INSERT INTO memory_v2_facts(
                    fact_id, owner_kind, project_id, owner_json, identity_json, created_at
                 ) VALUES('fact.conflict', 'project', ?1, ?2, ?3, 1)",
                rusqlite::params![project_id, owner_json, identity],
            )
            .unwrap();
    }
    let mut meta = branch_meta::load_branch_meta(&data_root).unwrap();
    meta.add_branch("v2-conflict", "branches/v2-conflict.db", "main");
    branch_meta::save_branch_meta(&data_root, &meta).unwrap();

    let cutover = MemoryCutoverOptions {
        project_root: project,
        profile_root,
    };
    let planned = tracedecay::migrate::memory_cutover::plan(&cutover)
        .await
        .unwrap();
    let error = tracedecay::migrate::memory_cutover::apply(&cutover, &planned.confirmation_token)
        .await
        .expect_err("incompatible stable V2 identity must fail closed");
    assert!(
        error.to_string().contains("memory_v2") && error.to_string().contains("conflict"),
        "unexpected conflict error: {error}"
    );
    assert!(branch.is_file());
    assert!(!data_root.join("memory-branch-cutover.json").exists());
    assert!(
        tracedecay::migrate::memory_cutover::verify_branch_removal_receipts(
            &data_root,
            std::slice::from_ref(&branch),
            &[],
        )
        .is_err(),
        "a failed merge must not authorize source deletion"
    );
}

struct HomeEnvGuard {
    previous_home: Option<OsString>,
    previous_userprofile: Option<OsString>,
    previous_data_dir: Option<OsString>,
}

#[cfg(unix)]
fn colliding_non_unicode_project_paths(root: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::ffi::OsStringExt as _;

    (
        root.join(OsString::from_vec(vec![b'p', 0x80])),
        root.join(OsString::from_vec(vec![b'p', 0x81])),
    )
}

#[cfg(windows)]
fn colliding_non_unicode_project_paths(root: &Path) -> (PathBuf, PathBuf) {
    use std::os::windows::ffi::OsStringExt as _;

    (
        root.join(OsString::from_wide(&[u16::from(b'p'), 0xd800])),
        root.join(OsString::from_wide(&[u16::from(b'p'), 0xd801])),
    )
}

#[tokio::test]
async fn hermes_home_env_cannot_redirect_legacy_migration() {
    let _lock = HOME_ENV_LOCK.lock().await;
    let temp = TempDir::new().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("profile");
    let redirected = temp.path().join("redirected-hermes");
    let redirected_db = redirected.join(".tracedecay/sessions.db");
    fs::create_dir_all(redirected_db.parent().unwrap()).unwrap();
    fs::write(&redirected_db, b"must remain untouched").unwrap();
    let _hermes_home = EnvVarGuard::set("HERMES_HOME", &redirected);

    let report =
        tracedecay::migrate::hermes::migrate_legacy_hermes_stores_to(&user_home, &profile_root)
            .await;

    assert_eq!(report, Default::default());
    assert_eq!(fs::read(&redirected_db).unwrap(), b"must remain untouched");
    assert!(!profile_root.join("global.db").exists());
    assert!(!profile_root.join("projects").exists());
}

impl HomeEnvGuard {
    fn set(home: &Path) -> Self {
        let previous_home = std::env::var_os("HOME");
        let previous_userprofile = std::env::var_os("USERPROFILE");
        let previous_data_dir = std::env::var_os(USER_DATA_DIR_ENV);
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("USERPROFILE", home);
            std::env::set_var(USER_DATA_DIR_ENV, home.join(".tracedecay"));
        }
        Self {
            previous_home,
            previous_userprofile,
            previous_data_dir,
        }
    }
}

impl Drop for HomeEnvGuard {
    fn drop(&mut self) {
        unsafe {
            match self.previous_home.take() {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match self.previous_userprofile.take() {
                Some(value) => std::env::set_var("USERPROFILE", value),
                None => std::env::remove_var("USERPROFILE"),
            }
            match self.previous_data_dir.take() {
                Some(value) => std::env::set_var(USER_DATA_DIR_ENV, value),
                None => std::env::remove_var(USER_DATA_DIR_ENV),
            }
        }
    }
}

fn canonical_temp_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn normalize_test_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("//?/")
        .to_string()
}

fn assert_path_eq(actual: impl AsRef<Path>, expected: impl AsRef<Path>) {
    assert_eq!(
        normalize_test_path(actual.as_ref()),
        normalize_test_path(expected.as_ref())
    );
}

fn prepare_maintenance_profile(profile_root: &Path) {
    fs::create_dir_all(profile_root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(profile_root, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

async fn init_with_maintenance(
    project_root: &Path,
    profile_root: &Path,
    open_options: TraceDecayOpenOptions,
) -> tracedecay::errors::Result<TraceDecay> {
    prepare_maintenance_profile(profile_root);
    let lifecycle = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        profile_root,
        "profile storage migration fixture initialization",
    )
    .unwrap();
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        profile_root,
        "profile storage migration fixture initialization",
    )
    .unwrap();
    TraceDecay::init_with_exclusive_maintenance(project_root, open_options, &lifecycle).await
}

async fn open_with_maintenance(
    project_root: &Path,
    profile_root: &Path,
    open_options: TraceDecayOpenOptions,
) -> tracedecay::errors::Result<TraceDecay> {
    prepare_maintenance_profile(profile_root);
    let lifecycle = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        profile_root,
        "profile storage migration fixture open",
    )
    .unwrap();
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        profile_root,
        "profile storage migration fixture open",
    )
    .unwrap();
    TraceDecay::open_with_exclusive_maintenance(project_root, open_options, &lifecycle).await
}

async fn open_branch_with_maintenance(
    project_root: &Path,
    branch_name: &str,
    profile_root: &Path,
    open_options: TraceDecayOpenOptions,
) -> tracedecay::errors::Result<TraceDecay> {
    prepare_maintenance_profile(profile_root);
    let lifecycle = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        profile_root,
        "profile storage migration fixture branch open",
    )
    .unwrap();
    let _database_scope = tracedecay::db::enter_maintenance_database_scope(
        &lifecycle,
        profile_root,
        "profile storage migration fixture branch open",
    )
    .unwrap();
    TraceDecay::open_branch_with_exclusive_maintenance(
        project_root,
        branch_name,
        open_options,
        &lifecycle,
    )
    .await
}

fn portable_relpath(path: &str) -> String {
    path.replace('\\', "/")
}

fn run_git(project: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .unwrap_or_else(|err| panic!("failed to run git {args:?}: {err}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn table_exists(db_path: &std::path::Path, table: &str) -> bool {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
         )",
        rusqlite::params![table],
        |row| row.get::<_, bool>(0),
    )
    .unwrap()
}

fn write_profile_store_manifest(profile_root: &Path, project_root: &Path) -> std::path::PathBuf {
    write_profile_store_manifest_for_id(profile_root, project_root, "proj_123")
}

fn write_profile_store_manifest_for_id(
    profile_root: &Path,
    project_root: &Path,
    project_id: &str,
) -> std::path::PathBuf {
    let data_root = profile_root.join("projects").join(project_id);
    fs::create_dir_all(&data_root).unwrap();
    fs::create_dir_all(project_root).unwrap();
    fs::write(data_root.join("tracedecay.db"), b"graph").unwrap();
    fs::write(data_root.join("sessions.db"), b"sessions").unwrap();
    let branch_meta = BranchMeta::new_for_dir(&data_root, "main");
    branch_meta::save_branch_meta(&data_root, &branch_meta).unwrap();
    let manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some(project_id.to_string()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root: project_root.to_path_buf(),
        data_root: data_root.clone(),
        graph_db_relpath: "tracedecay.db".into(),
        sessions_db_relpath: "sessions.db".into(),
        branch_meta_relpath: "branch-meta.json".into(),
    };
    let manifest_path = data_root.join(STORE_MANIFEST_FILENAME);
    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    manifest_path
}

#[tokio::test]
async fn global_db_creates_profile_storage_registry_tables() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let runtime = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    runtime.checkpoint_profile_database_for_test().await;
    drop(runtime);

    for table in [
        "code_projects",
        "project_aliases",
        "store_instances",
        "graph_scopes",
        "store_artifacts",
    ] {
        assert!(table_exists(&db_path, table), "{table} missing");
    }
}

#[test]
fn reconstructs_registry_records_from_profile_store_manifest() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let manifest_path = write_profile_store_manifest(&profile_root, &project_root);

    let report =
        reconstruct_registry_from_store_manifest(&manifest_path, &profile_root, 1_800_000_001);

    assert!(report.issues.is_empty(), "{:?}", report.issues);
    assert_eq!(report.plans.len(), 1);
    let plan = &report.plans[0];
    let canonical_project_root = project_root.canonicalize().unwrap();
    assert_eq!(plan.status, RegistryReconstructionStatus::Eligible);
    assert_eq!(plan.project.project_id, "proj_123");
    assert_eq!(plan.project.project_root, canonical_project_root);
    assert_eq!(plan.project.aliases, vec![canonical_project_root]);
    assert_eq!(plan.store.project_id, "proj_123");
    assert_eq!(plan.store.store_kind, "code_project");
    assert_eq!(plan.store.storage_mode, "profile_sharded");
    assert_eq!(plan.store.store_relpath, "projects/proj_123");
    assert_eq!(
        plan.store.manifest_relpath.as_deref().map(portable_relpath),
        Some("projects/proj_123/store_manifest.json".to_string())
    );
    assert_eq!(plan.store.last_verified_at, Some(1_800_000_001));
    assert!(
        plan.artifacts
            .iter()
            .any(|artifact| artifact.artifact_kind == "graph_db"
                && portable_relpath(&artifact.relpath) == "projects/proj_123/tracedecay.db")
    );
    assert!(
        plan.artifacts
            .iter()
            .any(|artifact| artifact.artifact_kind == "store_manifest"
                && portable_relpath(&artifact.relpath) == "projects/proj_123/store_manifest.json")
    );
    assert_eq!(plan.graph_scopes.len(), 1);
    assert_eq!(plan.graph_scopes[0].branch_name, "main");
    assert_eq!(
        portable_relpath(&plan.graph_scopes[0].db_relpath),
        "projects/proj_123/tracedecay.db"
    );
}

#[test]
fn scan_profile_store_manifests_rejects_unsafe_manifest_relpaths() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let data_root = profile_root.join("projects/proj_bad");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&data_root).unwrap();
    fs::create_dir_all(&project_root).unwrap();
    let manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some("proj_bad".to_string()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root,
        data_root,
        graph_db_relpath: "../outside.db".into(),
        sessions_db_relpath: "sessions.db".into(),
        branch_meta_relpath: "branch-meta.json".into(),
    };
    fs::write(
        profile_root.join("projects/proj_bad/store_manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let report = scan_profile_store_manifests(&profile_root, 1_800_000_001);

    assert!(report.plans.is_empty());
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.contains("unsafe graph_db_relpath"))
    );
}

#[cfg(unix)]
#[test]
fn reconstruction_accepts_equivalent_profile_symlink_but_rejects_symlink_escape() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let profile_alias = dir.path().join("profile-alias");
    let project_root = dir.path().join("repo");
    let manifest = write_profile_store_manifest(&profile_root, &project_root);
    std::os::unix::fs::symlink(&profile_root, &profile_alias).unwrap();

    let equivalent = reconstruct_registry_from_store_manifest(&manifest, &profile_alias, 1);
    assert!(equivalent.issues.is_empty(), "{:?}", equivalent.issues);
    assert_eq!(equivalent.plans.len(), 1);

    let outside = dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let escaped_root = profile_root.join("projects/proj_escape");
    std::os::unix::fs::symlink(&outside, &escaped_root).unwrap();
    let escaped_manifest = StoreManifest {
        schema_version: STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some("proj_escape".to_string()),
        store_kind: StoreKind::CodeProject,
        storage_mode: StorageMode::ProfileSharded,
        project_root,
        data_root: escaped_root.clone(),
        graph_db_relpath: "tracedecay.db".into(),
        sessions_db_relpath: "sessions.db".into(),
        branch_meta_relpath: "branch-meta.json".into(),
    };
    let escaped_manifest_path = escaped_root.join(STORE_MANIFEST_FILENAME);
    fs::write(
        &escaped_manifest_path,
        serde_json::to_string_pretty(&escaped_manifest).unwrap(),
    )
    .unwrap();

    let escaped =
        reconstruct_registry_from_store_manifest(&escaped_manifest_path, &profile_root, 1);
    assert!(escaped.plans.is_empty());
    assert!(
        escaped
            .issues
            .iter()
            .any(|issue| issue.contains("outside profile root"))
    );
}

#[test]
fn unsafe_branch_database_path_blocks_reconstruction() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let manifest = write_profile_store_manifest(&profile_root, &project_root);
    let branch_meta_path = manifest.parent().unwrap().join("branch-meta.json");
    let mut branch_meta: serde_json::Value =
        serde_json::from_slice(&fs::read(&branch_meta_path).unwrap()).unwrap();
    branch_meta["branches"]["main"]["db_file"] = serde_json::json!("../escape.db");
    fs::write(
        &branch_meta_path,
        serde_json::to_vec_pretty(&branch_meta).unwrap(),
    )
    .unwrap();

    let report = reconstruct_registry_from_store_manifest(&manifest, &profile_root, 1);

    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.contains("must reference canonical main database")),
        "unexpected reconstruction issues: {:?}",
        report.issues
    );
}

#[tokio::test]
async fn registry_resolves_project_store_by_canonical_alias() {
    let dir = TempDir::new().unwrap();
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();

    let project = db
        .upsert_code_project(
            "proj_123",
            &project_root,
            None,
            Some("https://example.test/repo.git"),
            Some("main"),
        )
        .await
        .unwrap();
    db.upsert_project_alias(&project_root.join("."), &project.project_id)
        .await
        .unwrap();
    let store = db
        .upsert_store_instance(StoreInstanceUpsert {
            store_id: "store_123".to_string(),
            project_id: project.project_id.clone(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: "projects/proj_123".to_string(),
            manifest_relpath: Some("projects/proj_123/store_manifest.json".to_string()),
            last_verified_at: Some(42),
            last_write_at: Some(43),
        })
        .await
        .unwrap();
    db.upsert_graph_scope(GraphScopeUpsert {
        graph_scope_id: "scope_123".to_string(),
        project_id: project.project_id.clone(),
        store_id: store.store_id.clone(),
        branch_name: "main".to_string(),
        db_relpath: "tracedecay.db".to_string(),
        parent_scope_id: None,
        last_synced_at: Some(44),
        writable: true,
    })
    .await
    .unwrap();
    db.upsert_store_artifact(StoreArtifactUpsert {
        store_id: store.store_id.clone(),
        artifact_kind: "graph_db".to_string(),
        relpath: "tracedecay.db".to_string(),
        size_bytes: Some(128),
        schema_version: Some("1".to_string()),
        updated_at: Some(45),
    })
    .await
    .unwrap();

    let resolved = db
        .resolve_project_store_by_alias(&project_root)
        .await
        .unwrap();

    assert_eq!(resolved.project.project_id, "proj_123");
    assert_eq!(resolved.store.store_id, "store_123");
    assert_eq!(resolved.graph_scopes.len(), 1);
    assert_eq!(resolved.graph_scopes[0].branch_name, "main");
    assert_eq!(resolved.artifacts.len(), 1);
    assert_eq!(resolved.artifacts[0].artifact_kind, "graph_db");
    assert_eq!(
        resolved.project.canonical_root,
        project_root.canonicalize().unwrap().to_string_lossy()
    );
}

#[tokio::test]
async fn delete_project_uses_same_canonical_key_as_upsert() {
    let dir = TempDir::new().unwrap();
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();

    db.upsert(&project_root, 99).await;
    assert_eq!(db.get_project_tokens(&project_root).await, 99);

    db.delete_project(&project_root.join(".")).await;

    assert_eq!(db.get_project_tokens(&project_root).await, 0);
}

#[tokio::test]
async fn staged_migration_resumes_cutover_after_registry_and_marker() {
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let manifest_path = root.join("manifest.json");
    let project = root.join("repo");
    let data_dir = project.join(".tracedecay");
    let graph_db = data_dir.join("tracedecay.db");
    let profile_root = root.join("profile");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(&graph_db, b"graph").unwrap();
    fs::write(
        data_dir.join("branch-meta.json"),
        r#"{"default_branch":"main","branches":{}}"#,
    )
    .unwrap();
    let graph_db_path = graph_db.clone();
    let mut manifest = build_plan_manifest(
        MigrationInventory {
            stores: vec![StoreInventory {
                project_root: project.clone(),
                data_dir,
                db_path: graph_db,
                brand: StoreBrand::TraceDecay,
                role: StoreRole::CodeProjectStore,
                registry_status: RegistryStatus::Unregistered,
                size_bytes: 128,
                statuses: vec![StoreStatus::Ok],
                artifacts: vec![StoreArtifact {
                    kind: "graph_db".to_string(),
                    path: graph_db_path,
                    size_bytes: 5,
                }],
            }],
            skipped: Vec::new(),
            global_db: None,
        },
        MigrationPlanOptions {
            manifest_path,
            migration_id: "mig_123".to_string(),
            tracedecay_version: "0.0.2".to_string(),
            created_at_unix: 1_800_000_000,
            confirmation_token: "confirm-mig_123".to_string(),
            target_profile_root: profile_root,
            project_id: "proj_123".to_string(),
        },
    )
    .unwrap();

    apply_migration_manifest(&mut manifest).await.unwrap();
    let staged = verify_migration_manifest(&manifest);
    assert!(staged.cutover_ready);
    assert!(!staged.apply_supported);
    assert!(read_enrollment_marker(&project).unwrap().is_none());

    let db = HostAdmissionTestRuntimeV1::profile(root).await.unwrap();
    db.apply_registry_reconstruction_report(&staged.registry_reconstruction)
        .await
        .unwrap();
    write_enrollment_marker(
        &project,
        &EnrollmentMarker {
            project_id: "proj_123".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    finalize_migration_apply(&mut manifest).unwrap();

    assert!(verify_migration_manifest(&manifest).apply_supported);
}

#[tokio::test]
async fn applies_registry_reconstruction_records_from_manifest() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let manifest_path = write_profile_store_manifest(&profile_root, &project_root);
    let report =
        reconstruct_registry_from_store_manifest(&manifest_path, &profile_root, 1_800_000_001);
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();

    let applied = db
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap();

    assert_eq!(applied.projects, 1);
    assert_eq!(applied.aliases, 1);
    assert_eq!(applied.stores, 1);
    assert_eq!(applied.graph_scopes, 1);
    assert_eq!(applied.artifacts, 4);
    let resolved = db
        .resolve_project_store_by_alias(&project_root.join("."))
        .await
        .unwrap();
    assert_eq!(resolved.project.project_id, "proj_123");
    assert_eq!(resolved.store.storage_mode, "profile_sharded");
    assert_eq!(
        resolved
            .store
            .manifest_relpath
            .as_deref()
            .map(portable_relpath),
        Some("projects/proj_123/store_manifest.json".to_string())
    );
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn registry_reconstruction_preserves_distinct_native_path_aliases() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let first_manifest = write_profile_store_manifest_for_id(
        &profile_root,
        &dir.path().join("first-source"),
        "proj_first_native",
    );
    let second_manifest = write_profile_store_manifest_for_id(
        &profile_root,
        &dir.path().join("second-source"),
        "proj_second_native",
    );
    let mut first = reconstruct_registry_from_store_manifest(&first_manifest, &profile_root, 1);
    let mut second = reconstruct_registry_from_store_manifest(&second_manifest, &profile_root, 1);
    let (first_path, second_path) = colliding_non_unicode_project_paths(dir.path());
    assert_eq!(first_path.to_string_lossy(), second_path.to_string_lossy());
    first.plans[0].project.project_root = first_path.clone();
    first.plans[0].project.aliases = vec![first_path.clone()];
    second.plans[0].project.project_root = second_path.clone();
    second.plans[0].project.aliases = vec![second_path.clone()];
    let report = RegistryReconstructionReport {
        plans: first.plans.into_iter().chain(second.plans).collect(),
        issues: Vec::new(),
    };
    let db = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();

    let applied = db
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap();
    assert_eq!(applied.projects, 2);
    assert_eq!(applied.aliases, 2);
    assert_eq!(
        db.project_registry_context_by_alias(&first_path)
            .await
            .unwrap()
            .unwrap()
            .project
            .project_id,
        "proj_first_native"
    );
    assert_eq!(
        db.project_registry_context_by_alias(&second_path)
            .await
            .unwrap()
            .unwrap()
            .project
            .project_id,
        "proj_second_native"
    );

    let resumed = db
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap();
    assert_eq!(resumed.projects, 0);
    assert_eq!(resumed.aliases, 0);
}

#[tokio::test]
async fn single_plan_reconstruction_rejects_noneligible_and_accepts_matching_existing_rows() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let manifest_path = write_profile_store_manifest(&profile_root, &project_root);
    let report =
        reconstruct_registry_from_store_manifest(&manifest_path, &profile_root, 1_800_000_001);
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();

    let mut retired = report.clone();
    retired.plans[0].status = RegistryReconstructionStatus::Retired;
    retired.plans[0].status_reason = Some("superseded project".to_string());
    let error = db
        .apply_single_registry_reconstruction_report(&retired)
        .await
        .unwrap_err();
    assert!(error.iter().any(|issue| issue.contains("Retired")));
    assert!(db.get_code_project("proj_123").await.is_none());

    let inserted = db
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap();
    assert_eq!(inserted.projects, 1);
    assert_eq!(inserted.stores, 1);

    let resumed = db
        .apply_single_registry_reconstruction_report(&report)
        .await
        .unwrap();
    assert_eq!(resumed.projects, 0);
    assert_eq!(resumed.stores, 0);
    assert_eq!(
        db.resolve_project_store_by_identity(&project_root, None)
            .await
            .unwrap()
            .unwrap()
            .project
            .project_id,
        "proj_123"
    );
}

#[tokio::test]
async fn conflicting_alias_is_rejected_without_stealing_or_partial_writes() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let manifest = write_profile_store_manifest(&profile_root, &project_root);
    let report = reconstruct_registry_from_store_manifest(&manifest, &profile_root, 1);
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    db.upsert_code_project("proj_owner", &project_root, None, None, None)
        .await
        .unwrap();

    let error = db
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap_err();

    assert!(error.iter().any(|issue| issue.contains("already owned")));
    assert!(db.get_code_project("proj_123").await.is_none());
    assert_eq!(
        db.project_registry_context_by_alias(&project_root)
            .await
            .unwrap()
            .unwrap()
            .project
            .project_id,
        "proj_owner"
    );

    let second_project_root = dir.path().join("repo-2");
    let second_manifest = write_profile_store_manifest(&profile_root, &second_project_root);
    let second_report =
        reconstruct_registry_from_store_manifest(&second_manifest, &profile_root, 2);
    let applied = db
        .apply_registry_reconstruction_report(&second_report)
        .await
        .unwrap();
    assert_eq!(applied.projects, 1);
    assert_eq!(
        db.project_registry_context_by_alias(&second_project_root)
            .await
            .unwrap()
            .unwrap()
            .project
            .project_id,
        "proj_123"
    );
}

#[tokio::test]
async fn conflicting_later_plan_rolls_back_the_entire_reconstruction_batch() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let first_root = dir.path().join("first-repo");
    let second_root = dir.path().join("second-repo");
    let first = write_profile_store_manifest_for_id(&profile_root, &first_root, "proj_first");
    let second = write_profile_store_manifest_for_id(&profile_root, &second_root, "proj_second");
    let first = reconstruct_registry_from_store_manifest(&first, &profile_root, 1);
    let second = reconstruct_registry_from_store_manifest(&second, &profile_root, 1);
    let report = RegistryReconstructionReport {
        plans: first.plans.into_iter().chain(second.plans).collect(),
        issues: Vec::new(),
    };
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    db.upsert_code_project("proj_owner", &second_root, None, None, None)
        .await
        .unwrap();

    db.apply_registry_reconstruction_report(&report)
        .await
        .unwrap_err();

    assert!(db.get_code_project("proj_first").await.is_none());
    assert!(db.get_code_project("proj_second").await.is_none());
    assert!(db.get_code_project("proj_owner").await.is_some());
}

#[tokio::test]
async fn physical_store_path_conflict_rolls_back_without_creating_project() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let manifest = write_profile_store_manifest(&profile_root, &project_root);
    let report = reconstruct_registry_from_store_manifest(&manifest, &profile_root, 1);
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    let owner_root = dir.path().join("owner");
    fs::create_dir_all(&owner_root).unwrap();
    db.upsert_code_project("proj_owner", &owner_root, None, None, None)
        .await
        .unwrap();
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: "store:owner".to_string(),
        project_id: "proj_owner".to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: report.plans[0].store.store_relpath.clone(),
        manifest_relpath: None,
        last_verified_at: None,
        last_write_at: None,
    })
    .await
    .unwrap();

    let error = db
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap_err();

    assert!(
        error
            .iter()
            .any(|issue| issue.contains("physical store path"))
    );
    assert!(db.get_code_project("proj_123").await.is_none());
}

#[tokio::test]
async fn physical_graph_scope_conflict_rolls_back_without_creating_project() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    let manifest = write_profile_store_manifest(&profile_root, &project_root);
    let report = reconstruct_registry_from_store_manifest(&manifest, &profile_root, 1);
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();
    let owner_root = dir.path().join("owner");
    fs::create_dir_all(&owner_root).unwrap();
    db.upsert_code_project("proj_owner", &owner_root, None, None, None)
        .await
        .unwrap();
    db.upsert_store_instance(StoreInstanceUpsert {
        store_id: "store:owner".to_string(),
        project_id: "proj_owner".to_string(),
        store_kind: "code_project".to_string(),
        storage_mode: "profile_sharded".to_string(),
        store_relpath: "projects/owner".to_string(),
        manifest_relpath: None,
        last_verified_at: None,
        last_write_at: None,
    })
    .await
    .unwrap();
    db.upsert_graph_scope(GraphScopeUpsert {
        graph_scope_id: "scope:owner".to_string(),
        project_id: "proj_owner".to_string(),
        store_id: "store:owner".to_string(),
        branch_name: "owner".to_string(),
        db_relpath: report.plans[0].graph_scopes[0].db_relpath.clone(),
        parent_scope_id: None,
        last_synced_at: None,
        writable: true,
    })
    .await
    .unwrap();

    let error = db
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap_err();

    assert!(
        error
            .iter()
            .any(|issue| issue.contains("physical graph database path"))
    );
    assert!(db.get_code_project("proj_123").await.is_none());
}

#[test]
fn missing_and_temporary_project_roots_are_classified_stale() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("temporary-repo");
    let manifest = write_profile_store_manifest(&profile_root, &project_root);
    write_enrollment_marker(
        &project_root,
        &EnrollmentMarker {
            project_id: "proj_123".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();

    let scanned = scan_profile_store_manifests(&profile_root, 1);
    assert_eq!(scanned.plans[0].status, RegistryReconstructionStatus::Stale);
    assert!(
        scanned.plans[0]
            .status_reason
            .as_deref()
            .unwrap()
            .contains("temporary directory")
    );

    fs::remove_dir_all(&project_root).unwrap();
    let missing = reconstruct_registry_from_store_manifest(&manifest, &profile_root, 1);
    assert_eq!(missing.plans[0].status, RegistryReconstructionStatus::Stale);
    assert!(
        missing.plans[0]
            .status_reason
            .as_deref()
            .unwrap()
            .contains("unavailable")
    );
}

#[test]
fn strict_scan_classifies_unmarked_temporary_project_root_stale() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("unmarked-repo");
    write_profile_store_manifest(&profile_root, &project_root);

    let scanned = scan_profile_store_manifests(&profile_root, 1);

    assert_eq!(scanned.plans[0].status, RegistryReconstructionStatus::Stale);
    assert!(
        scanned.plans[0]
            .status_reason
            .as_deref()
            .unwrap()
            .contains("temporary directory")
    );
}

#[test]
fn strict_scan_requires_matching_repository_identity_or_enrollment() {
    let dir = tempfile::Builder::new()
        .prefix("reconstruct-identity-")
        .tempdir_in(ephemeral_safe_fixture_base())
        .unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    write_profile_store_manifest(&profile_root, &project_root);
    run_git(&project_root, &["init"]);

    let unowned = scan_profile_store_manifests(&profile_root, 1);
    assert_eq!(
        unowned.plans[0].status,
        RegistryReconstructionStatus::Blocked
    );
    assert!(
        unowned.plans[0]
            .status_reason
            .as_deref()
            .unwrap()
            .contains("no repository identity or enrollment marker")
    );

    write_enrollment_marker(
        &project_root,
        &EnrollmentMarker {
            project_id: "proj_123".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let enrolled = scan_profile_store_manifests(&profile_root, 1);
    assert_eq!(
        enrolled.plans[0].status,
        RegistryReconstructionStatus::Eligible
    );
}

#[tokio::test]
async fn consolidation_source_is_skipped_while_destination_applies() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    run_git(&project_root, &["init"]);
    write_repository_identity_marker(&project_root, "proj_destination").unwrap();
    let source = write_profile_store_manifest_for_id(
        &profile_root,
        &project_root,
        "proj_consolidated_source",
    );
    fs::write(
        source.parent().unwrap().join("branch-meta.json"),
        r#"{"default_branch":"main","branches":{"main":{"db_file":"../outside.db","created_at":"1","last_synced_at":"1"}}}"#,
    )
    .unwrap();
    let destination =
        write_profile_store_manifest_for_id(&profile_root, &project_root, "proj_destination");

    let source = reconstruct_registry_from_store_manifest(&source, &profile_root, 1);
    let destination = reconstruct_registry_from_store_manifest(&destination, &profile_root, 1);

    assert_eq!(
        source.plans[0].status,
        RegistryReconstructionStatus::Retired
    );
    assert!(source.issues.is_empty(), "{:?}", source.issues);
    assert_eq!(
        destination.plans[0].status,
        RegistryReconstructionStatus::Eligible
    );
    let issues = source
        .issues
        .into_iter()
        .chain(destination.issues)
        .collect();
    let report = RegistryReconstructionReport {
        plans: source.plans.into_iter().chain(destination.plans).collect(),
        issues,
    };
    let db = HostAdmissionTestRuntimeV1::profile(dir.path())
        .await
        .unwrap();

    let applied = db
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap();

    assert_eq!(applied.projects, 1);
    assert!(
        db.get_code_project("proj_consolidated_source")
            .await
            .is_none()
    );
    assert!(db.get_code_project("proj_destination").await.is_some());
}

#[test]
fn disagreeing_dual_markers_block_and_matching_retired_markers_retire() {
    let dir = TempDir::new().unwrap();
    let profile_root = dir.path().join("profile");
    let project_root = dir.path().join("repo");
    fs::create_dir_all(&project_root).unwrap();
    run_git(&project_root, &["init"]);
    write_repository_identity_marker(&project_root, "proj_repository").unwrap();
    write_enrollment_marker(
        &project_root,
        &EnrollmentMarker {
            project_id: "proj_enrollment".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let manifest =
        write_profile_store_manifest_for_id(&profile_root, &project_root, "proj_repository");

    let disagreement = reconstruct_registry_from_store_manifest(&manifest, &profile_root, 1);
    assert_eq!(
        disagreement.plans[0].status,
        RegistryReconstructionStatus::Blocked
    );

    write_enrollment_marker(
        &project_root,
        &EnrollmentMarker {
            project_id: "proj_repository".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let retired_manifest =
        write_profile_store_manifest_for_id(&profile_root, &project_root, "proj_retired_manifest");
    let retired = reconstruct_registry_from_store_manifest(&retired_manifest, &profile_root, 1);
    assert_eq!(
        retired.plans[0].status,
        RegistryReconstructionStatus::Retired
    );
}

#[tokio::test]
async fn cursor_session_db_uses_registry_profile_shard_without_marker() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("home");
    let profile_root = home.join(".tracedecay");
    let project_root = dir.path().join("repo");
    let manifest_path = write_profile_store_manifest(&profile_root, &project_root);
    let session_db = profile_root.join("projects/proj_123/sessions.db");
    fs::remove_file(&session_db).unwrap();
    let global = HostAdmissionTestRuntimeV1::project(
        &profile_root,
        &project_root,
        ProjectId::new("proj_123").unwrap(),
    )
    .await
    .unwrap();
    let _home_guard = HomeEnvGuard::set(&home);
    let report =
        reconstruct_registry_from_store_manifest(&manifest_path, &profile_root, 1_800_000_001);
    global
        .apply_registry_reconstruction_report(&report)
        .await
        .unwrap();

    assert_eq!(
        global.database_path(HostAdmissionScope::Project),
        Some(session_db.as_path()),
        "session ingest should retain the registry-backed profile session DB"
    );
    assert!(session_db.is_file());
    assert!(
        !project_root.join(".tracedecay/sessions.db").exists(),
        "session ingest must not create a repo-local sessions DB for registry-backed profile stores"
    );
}

#[tokio::test]
async fn trace_decay_init_uses_profile_shard_when_enrolled() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let home = root.join("home");
    let profile_root = home.join(".tracedecay");
    let project = root.join("repo");
    let shard_root = profile_root.join("projects/proj_init");
    fs::create_dir_all(&project).unwrap();
    let _home_guard = HomeEnvGuard::set(&home);
    write_enrollment_marker(
        &project,
        &EnrollmentMarker {
            project_id: "proj_init".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();

    let cg = init_with_maintenance(&project, &profile_root, TraceDecayOpenOptions::default())
        .await
        .unwrap();

    assert_path_eq(&cg.store_layout().data_root, &shard_root);
    assert_path_eq(cg.db_path(), shard_root.join("tracedecay.db"));
    assert!(cg.db_path().is_file());
    assert!(
        !shard_root.join("config.json").exists(),
        "profile-sharded init persists configuration in the store, not a legacy config.json"
    );
    assert!(shard_root.join(STORE_MANIFEST_FILENAME).is_file());
    assert!(
        !project.join(".tracedecay/tracedecay.db").exists(),
        "profile-sharded init must not create a repo-local graph DB"
    );
}

#[tokio::test]
async fn trace_decay_init_with_options_uses_explicit_profile_identity() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let daemon_home = root.join("daemon-home");
    let client_profile = root.join("client-profile");
    let project = root.join("repo");
    fs::create_dir_all(&project).unwrap();
    let _home_guard = HomeEnvGuard::set(&daemon_home);
    write_enrollment_marker(
        &project,
        &EnrollmentMarker {
            project_id: "proj_explicit".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(client_profile.clone()),
        global_db_path: Some(client_profile.join("global.db")),
    };

    assert!(
        !TraceDecay::is_initialized_with_options(&project, &open_options),
        "a marker alone must not initialize an explicit client profile"
    );

    let cg = init_with_maintenance(&project, &client_profile, open_options.clone())
        .await
        .unwrap();

    assert_eq!(
        cg.store_layout().data_root,
        client_profile.join("projects/proj_explicit")
    );
    assert!(
        !cg.store_layout().config_path.exists(),
        "init persists configuration in the store, not a legacy config.json"
    );
    assert!(cg.db_path().is_file());
    assert!(TraceDecay::is_initialized_with_options(
        &project,
        &open_options
    ));
    assert!(
        !daemon_home.join(".tracedecay").exists(),
        "explicit client profile init must not create a store in the daemon/default profile"
    );
}

#[tokio::test]
async fn trace_decay_options_global_db_path_implies_profile_root() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let daemon_home = root.join("daemon-home");
    let client_profile = root.join("client-profile");
    let project = root.join("repo");
    fs::create_dir_all(&project).unwrap();
    let _home_guard = HomeEnvGuard::set(&daemon_home);
    write_enrollment_marker(
        &project,
        &EnrollmentMarker {
            project_id: "proj_db_only".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let open_options = TraceDecayOpenOptions {
        profile_root: None,
        global_db_path: Some(client_profile.join("global.db")),
    };

    let cg = init_with_maintenance(&project, &client_profile, open_options.clone())
        .await
        .unwrap();

    assert_eq!(
        cg.store_layout().data_root,
        client_profile.join("projects/proj_db_only")
    );
    assert!(
        !cg.store_layout().config_path.exists(),
        "init persists configuration in the store, not a legacy config.json"
    );
    assert!(cg.db_path().is_file());
    assert!(TraceDecay::is_initialized_with_options(
        &project,
        &open_options
    ));
    assert!(
        !daemon_home.join(".tracedecay").exists(),
        "global_db_path-only options must not fall back to the daemon/default profile"
    );
}

#[tokio::test]
async fn trace_decay_add_branch_tracking_returns_not_indexed_for_uninitialized_profile_store() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let home = root.join("home");
    let project = root.join("repo");
    fs::create_dir_all(&project).unwrap();
    let _home_guard = HomeEnvGuard::set(&home);

    let outcome = TraceDecay::add_branch_tracking(&project, "feature/unindexed")
        .await
        .unwrap();

    assert_eq!(outcome, BranchAddOutcome::NotIndexed);
    assert!(
        !home
            .join(".tracedecay/projects")
            .join(tracedecay::storage::default_profile_project_id(&project))
            .exists(),
        "branch add must not create project profile storage before tracedecay init"
    );
}

#[tokio::test]
async fn trace_decay_open_matches_renamed_git_checkout_by_registered_remote() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let home = root.join("home");
    let project = root.join("repo-before-rename");
    let renamed = root.join("repo-after-rename");
    fs::create_dir_all(&project).unwrap();
    run_git(&project, &["init"]);
    run_git(
        &project,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:ScriptedAlchemy/tracedecay.git",
        ],
    );
    let _home_guard = HomeEnvGuard::set(&home);

    let profile_root = home.join(".tracedecay");
    let initialized =
        init_with_maintenance(&project, &profile_root, TraceDecayOpenOptions::default())
            .await
            .unwrap();
    let original_project_id = initialized
        .store_layout()
        .identity
        .project_id
        .clone()
        .unwrap();
    let original_data_root = initialized.store_layout().data_root.clone();
    drop(initialized);
    fs::rename(&project, &renamed).unwrap();

    let reopened = open_with_maintenance(&renamed, &profile_root, TraceDecayOpenOptions::default())
        .await
        .unwrap();

    assert_eq!(
        reopened.store_layout().identity.project_id.as_deref(),
        Some(original_project_id.as_str())
    );
    assert_eq!(reopened.store_layout().data_root, original_data_root);
    assert!(
        !home
            .join(".tracedecay/projects")
            .join(tracedecay::storage::default_profile_project_id(&renamed))
            .join("tracedecay.db")
            .exists(),
        "renamed checkout must not create a second path-hash profile shard"
    );
}

#[tokio::test]
async fn persisted_repository_identity_survives_rename_while_serve_open_fails_closed() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let daemon_home = root.join("daemon-home");
    let client_profile = root.join("client-profile");
    let project = root.join("repo-before-rename");
    let renamed = root.join("repo-after-rename");
    fs::create_dir_all(&project).unwrap();
    run_git(&project, &["init"]);
    run_git(
        &project,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:ScriptedAlchemy/tracedecay.git",
        ],
    );
    let _home_guard = HomeEnvGuard::set(&daemon_home);
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(client_profile.clone()),
        global_db_path: Some(client_profile.join("global.db")),
    };

    let initialized = init_with_maintenance(&project, &client_profile, open_options.clone())
        .await
        .unwrap();
    let original_data_root = initialized.store_layout().data_root.clone();
    drop(initialized);
    fs::rename(&project, &renamed).unwrap();

    assert!(
        TraceDecay::is_initialized_with_options(&renamed, &open_options),
        "the durable git marker should resolve the moved profile store synchronously"
    );
    let serve_error =
        match serve::ensure_initialized_with_options(&renamed, open_options.clone()).await {
            Ok(_) => panic!("serve compatibility API must not open the project database locally"),
            Err(error) => error,
        };
    assert!(
        serve_error
            .to_string()
            .contains("managed TraceDecay daemon"),
        "serve should direct callers to the sole database owner: {serve_error}"
    );

    let reopened = open_with_maintenance(&renamed, &client_profile, open_options)
        .await
        .unwrap();

    assert_eq!(reopened.store_layout().data_root, original_data_root);
    assert!(
        !client_profile
            .join("projects")
            .join(tracedecay::storage::default_profile_project_id(&renamed))
            .join("tracedecay.db")
            .exists(),
        "serve must not create or require a second path-hash profile shard"
    );
}

#[tokio::test]
async fn branch_open_rejects_a_mismatched_maintenance_profile() {
    let dir = TempDir::new().unwrap();
    let project = dir.path().join("repo");
    let requested_profile = dir.path().join("requested-profile");
    let leased_profile = dir.path().join("leased-profile");
    fs::create_dir_all(&project).unwrap();
    prepare_maintenance_profile(&requested_profile);
    prepare_maintenance_profile(&leased_profile);
    let lifecycle = tracedecay::lifecycle_lease::acquire_exclusive_for_profile(
        &leased_profile,
        "mismatched branch fixture",
    )
    .unwrap();

    let error = match TraceDecay::open_branch_with_exclusive_maintenance(
        &project,
        "main",
        TraceDecayOpenOptions {
            profile_root: Some(requested_profile.clone()),
            global_db_path: Some(requested_profile.join("global.db")),
        },
        &lifecycle,
    )
    .await
    {
        Ok(_) => panic!("mismatched profile lease must be rejected"),
        Err(error) => error,
    };

    assert_eq!(
        error.to_string(),
        "config error: branch open requires the exact profile's exclusive lifecycle lease"
    );
}

#[tokio::test]
async fn trace_decay_open_branch_uses_profile_shard_branch_db() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let home = root.join("home");
    let profile_root = home.join(".tracedecay");
    let project = root.join("repo");
    let shard_root = profile_root.join("projects/proj_branch");
    let branch_db = shard_root.join("branches/feature_profile.db");
    fs::create_dir_all(branch_db.parent().unwrap()).unwrap();
    fs::create_dir_all(project.join(".tracedecay")).unwrap();
    run_git(&project, &["init"]);
    let _home_guard = HomeEnvGuard::set(&home);
    write_enrollment_marker(
        &project,
        &EnrollmentMarker {
            project_id: "proj_branch".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let config = TraceDecayConfig {
        root_dir: project.to_string_lossy().to_string(),
        ..TraceDecayConfig::default()
    };
    fs::write(
        shard_root.join("config.json"),
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
    crate::common::initialize_test_database(&shard_root.join("tracedecay.db"))
        .await
        .unwrap();
    crate::common::initialize_test_database(&branch_db)
        .await
        .unwrap();
    let mut meta = BranchMeta::new_for_dir(&shard_root, "main");
    meta.add_branch("feature/profile", "branches/feature_profile.db", "main");
    branch_meta::save_branch_meta(&shard_root, &meta).unwrap();

    let cg = open_branch_with_maintenance(
        &project,
        "feature/profile",
        &profile_root,
        TraceDecayOpenOptions::default(),
    )
    .await
    .unwrap();

    assert_path_eq(&cg.store_layout().data_root, &shard_root);
    assert_path_eq(cg.db_path(), &branch_db);
    assert_eq!(cg.serving_branch(), Some("feature/profile"));
}

#[tokio::test]
async fn trace_decay_open_with_options_auto_tracks_branch_in_explicit_profile() {
    let _guard = HOME_ENV_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let root = canonical_temp_path(dir.path());
    let daemon_home = root.join("daemon-home");
    let client_profile = root.join("client-profile");
    let project = root.join("repo");
    fs::create_dir_all(&project).unwrap();
    run_git(&project, &["init"]);
    run_git(&project, &["config", "user.email", "test@example.com"]);
    run_git(&project, &["config", "user.name", "TraceDecay Test"]);
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
    run_git(&project, &["add", "."]);
    run_git(&project, &["commit", "-m", "initial"]);
    run_git(&project, &["checkout", "-b", "feature/client-profile"]);
    fs::write(
        project.join("src/main.rs"),
        "fn main() { println!(\"feature\"); }\n",
    )
    .unwrap();
    run_git(&project, &["add", "."]);
    run_git(&project, &["commit", "-m", "feature"]);
    run_git(&project, &["checkout", "-"]);

    let _home_guard = HomeEnvGuard::set(&daemon_home);
    write_enrollment_marker(
        &project,
        &EnrollmentMarker {
            project_id: "proj_auto_branch".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(client_profile.clone()),
        global_db_path: Some(client_profile.join("global.db")),
    };
    let main = init_with_maintenance(&project, &client_profile, open_options.clone())
        .await
        .unwrap();
    let shard_root = main.store_layout().data_root.clone();
    assert_eq!(shard_root, client_profile.join("projects/proj_auto_branch"));
    drop(main);

    run_git(&project, &["checkout", "feature/client-profile"]);
    let cg = open_with_maintenance(&project, &client_profile, open_options)
        .await
        .unwrap();

    assert_eq!(cg.store_layout().data_root, shard_root);
    assert_eq!(cg.serving_branch(), Some("feature/client-profile"));
    assert!(cg.db_path().starts_with(shard_root.join("branches")));
    assert!(cg.db_path().is_file());
    assert!(
        !daemon_home.join(".tracedecay").exists(),
        "auto-tracking with explicit options must not create branch storage in the daemon/default profile"
    );
}

//! PR7 acceptance: fact/anchor writes and resolution hold the writer-authority
//! contract. A revoked or missing authority fails closed with a typed error and
//! no partial commit, ambiguous project scope never falls back to another
//! store, linked worktrees resolve through canonical project identity, and
//! concurrent clients commit exactly one fact/anchor — losers get the
//! idempotent replay or a typed conflict, never a second writer.

use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::TempDir;
use tracedecay::application::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::db::{Database, DatabaseAuthority};
#[cfg(feature = "test-transport")]
use tracedecay::db::{
    DatabaseAuthorityRole, TestDatabaseRuntimeMode, enter_maintenance_database_scope,
};
use tracedecay::global_db::StoreInstanceUpsert;
#[cfg(feature = "test-transport")]
use tracedecay::lifecycle_lease::acquire_exclusive_for_profile;
use tracedecay::store::DatabaseFactStore;
use tracedecay_domain::{
    AccessPolicyDigest, AnchorDurabilityClass, AnchorSourceGenerationV2, CapabilityId,
    ComponentVersion, Confidence, CoverageReportV1, EntityId, EntityKind, EntityRef, EvidenceClass,
    FactAssertionKindV1, FactAssertionV1, FactCategoryV1, FactEventId, FactEvidenceRefV1,
    FactEvidenceRelationV1, FactId, FactIdentityMaterialV1, FactIdentitySourceV1,
    FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1, ObservationScopeV1,
    PayloadAccessState, PayloadReferenceV1, PrivacyDomainBoundLocatorDigest, PrivacyDomainId,
    ProjectId, ProjectionGenerationId, ProvenanceId, ResolutionAuthorizationV1, RetentionClass,
    RetrievalAnchorRecordV2, RetrievalAnchorRecordV2Parts, RetrievalAnchorTargetV2,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    ScopeResolutionId, SensitivityV1, UtcMicros, VectorWatermark,
};
#[cfg(feature = "test-transport")]
use tracedecay_store::FactStoreError;
use tracedecay_store::{
    CurrentFactsQuery, FactCommitConflict, FactCommitOutcome, FactCurrentQuery, FactLineageQuery,
    FactStore, FactWriteBatch, RetrievalAnchorQuery, StoredFactV1,
};

fn profile_owner() -> FactOwnerV1 {
    FactOwnerV1::Profile
}

fn identity(owner: &FactOwnerV1, operation: &str) -> (FactId, FactIdentityMaterialV1) {
    let material = FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Application {
            operation_id: ProvenanceId::new(operation.to_owned()).unwrap(),
        },
    )
    .unwrap();
    let fact_id = FactId::derive(&material).unwrap();
    (fact_id, material)
}

fn payload(content: &str, receipt_id: &str) -> FactPayloadV1 {
    let material = json!({
        "content": content,
        "category": "project",
        "tags": ["database"],
        "entities": ["TraceDecay"],
        "metadata": {},
    });
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            ComponentVersion::new("sanitizer.pr7.authority.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&material).unwrap()),
    )
    .unwrap();
    FactPayloadV1::new(
        content.to_owned(),
        FactCategoryV1::Project,
        vec!["database".to_owned()],
        vec!["TraceDecay".to_owned()],
        json!({}),
        receipt,
        RetentionClass::new("retention.pr7.authority").unwrap(),
    )
    .unwrap()
}

fn anchor(entity_id: &str, scope: ObservationScopeV1, ingested_at: i64) -> RetrievalAnchorRecordV2 {
    const DIGEST_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target: RetrievalAnchorTargetV2::Entity(EntityRef {
            id: EntityId::new(entity_id).unwrap(),
            kind: EntityKind::Document,
        }),
        owner: scope,
        aliases: vec![],
        occurred_at: None,
        ingested_at: UtcMicros(ingested_at),
        evidence_class: EvidenceClass::Observed,
        source_generation: AnchorSourceGenerationV2::Unknown,
        projection_generation: ProjectionGenerationId::new("projection.pr7.authority").unwrap(),
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations: vec![],
        source_anchors: vec![],
        authorization: ResolutionAuthorizationV1 {
            resolved_scope_id: ScopeResolutionId::new("scope.pr7.authority").unwrap(),
            privacy_domain_id: PrivacyDomainId::new("privacy.pr7.authority").unwrap(),
            access_policy_digest: AccessPolicyDigest::new(DIGEST_A).unwrap(),
            capability_id: CapabilityId::new("capability.pr7.authority").unwrap(),
            canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(DIGEST_B).unwrap(),
        },
        payload_access: PayloadAccessState::Eligible,
        retention_class: RetentionClass::new("retention.pr7.authority").unwrap(),
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .unwrap()
}

/// One creation batch: a fresh fact identity, one evidence anchor, one
/// assertion, and its recording event. Identical inputs derive identical
/// batches, which is what makes exact-replay races deterministic.
fn assertion_batch(
    owner: &FactOwnerV1,
    scope: ObservationScopeV1,
    operation: &str,
    entity_id: &str,
    content: &str,
    at: i64,
    expected_last_event_id: Option<FactEventId>,
) -> FactWriteBatch {
    let (fact_id, material) = identity(owner, operation);
    let anchor = anchor(entity_id, scope, at);
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
        payload(content, &format!("receipt.{operation}")),
        vec![evidence],
        UtcMicros(at),
        None,
    )
    .unwrap();
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        UtcMicros(at),
        None,
    )
    .unwrap();
    FactWriteBatch::new(
        fact_id,
        owner.clone(),
        Some(assertion),
        vec![event],
        vec![anchor],
        vec![],
        None,
        expected_last_event_id,
    )
    .unwrap()
    .with_identity_material(material)
    .unwrap()
}

/// One append batch against an existing fact, guarded by an optimistic
/// concurrency token (`expected_last_event_id`).
fn trust_batch(
    owner: &FactOwnerV1,
    fact_id: &FactId,
    at: i64,
    expected_last_event_id: Option<FactEventId>,
) -> FactWriteBatch {
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::TrustChanged {
            previous: Confidence::new(0.5).unwrap(),
            current: Confidence::new(0.9).unwrap(),
            evidence_ids: vec![],
        },
        UtcMicros(at),
        None,
    )
    .unwrap();
    FactWriteBatch::new(
        fact_id.clone(),
        owner.clone(),
        None,
        vec![event],
        vec![],
        vec![],
        None,
        expected_last_event_id,
    )
    .unwrap()
}

async fn fact_db(path: &Path) -> Database {
    crate::common::initialize_test_database(path)
        .await
        .unwrap()
        .0
}

async fn profile_runtime(tmp: &TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .unwrap()
}

async fn lineage(
    store: &DatabaseFactStore<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> Vec<FactLineageEventV1> {
    store
        .query_fact_lineage(
            FactLineageQuery::new(owner.clone(), fact_id.clone(), None, 16).unwrap(),
        )
        .await
        .unwrap()
}

async fn current(
    store: &DatabaseFactStore<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> Option<StoredFactV1> {
    store
        .query_fact_current(FactCurrentQuery::new(owner.clone(), fact_id.clone()).unwrap())
        .await
        .unwrap()
}

async fn current_count(store: &DatabaseFactStore<'_>, owner: &FactOwnerV1) -> usize {
    store
        .query_current_facts(CurrentFactsQuery::new(owner.clone(), None, 16).unwrap())
        .await
        .unwrap()
        .len()
}

fn committed(outcome: FactCommitOutcome) -> tracedecay_store::FactCommitReceipt {
    match outcome {
        FactCommitOutcome::Committed(receipt) => receipt,
        other => panic!("expected a committed fact batch, got {other:?}"),
    }
}

#[cfg(feature = "test-transport")]
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn revoked_write_authority_fails_closed_without_partial_fact_commit() {
    let tmp = TempDir::new().unwrap();
    let profile_root = canonical(tmp.path());
    let db_path = profile_root.join("projects/pr7-authority/memory.db");
    let lease =
        acquire_exclusive_for_profile(&profile_root, "pr7 revoked authority fixture").unwrap();
    let scope =
        enter_maintenance_database_scope(&lease, &profile_root, "pr7 revoked authority fixture")
            .unwrap();
    let authority = DatabaseAuthority::for_runtime(&db_path, "pr7 revoked authority fixture")
        .expect("a live maintenance scope must grant the write authority");
    assert_eq!(authority.role(), DatabaseAuthorityRole::Maintenance);
    let db = Database::publish_maintenance_test_runtime(
        &db_path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .unwrap()
    .0;
    let store = DatabaseFactStore::new(&db);
    let owner = profile_owner();
    let batch = assertion_batch(
        &owner,
        ObservationScopeV1::Profile,
        "authority.revoked",
        "entity.authority.revoked",
        "committed before the authority was revoked",
        1,
        None,
    );
    let fact_id = batch.fact_id().clone();
    let receipt = committed(store.commit_fact(batch).await.unwrap());
    let last_event_id = receipt.last_event_id().clone();

    // Revoke the authority scope underneath the retained database handle.
    drop(scope);

    let error = store
        .commit_fact(trust_batch(&owner, &fact_id, 2, Some(last_event_id)))
        .await
        .expect_err("a write with a revoked authority must fail closed");
    match &error {
        FactStoreError::Storage { operation, source } => {
            assert_eq!(*operation, "commit canonical memory fact");
            assert!(
                source
                    .to_string()
                    .contains("active daemon or exclusive maintenance scope"),
                "revoked authority must be reported as a missing live scope, got {source}"
            );
        }
        other => panic!("revoked authority must surface as a storage failure, got {other:?}"),
    }

    let lineage = lineage(&store, &owner, &fact_id).await;
    assert_eq!(
        lineage.len(),
        1,
        "a revoked write must not partially commit lineage"
    );
    let current = current(&store, &owner, &fact_id)
        .await
        .expect("the committed fact must survive the rejected write");
    assert_eq!(
        current.payload().unwrap().content(),
        "committed before the authority was revoked"
    );
    assert_eq!(current_count(&store, &owner).await, 1);
}

#[tokio::test]
async fn stale_fact_authority_cas_conflict_is_typed_and_leaves_lineage_untouched() {
    let tmp = TempDir::new().unwrap();
    let db = fact_db(&tmp.path().join("memory.db")).await;
    let store = DatabaseFactStore::new(&db);
    let owner = profile_owner();
    let batch = assertion_batch(
        &owner,
        ObservationScopeV1::Profile,
        "authority.stale-cas",
        "entity.authority.stale-cas",
        "base fact content",
        1,
        None,
    );
    let fact_id = batch.fact_id().clone();
    let first = committed(store.commit_fact(batch).await.unwrap());
    let first_event = first.last_event_id().clone();
    let second = committed(
        store
            .commit_fact(trust_batch(&owner, &fact_id, 2, Some(first_event.clone())))
            .await
            .unwrap(),
    );
    let second_event = second.last_event_id().clone();

    // A stale authority retained the pre-append frontier (`first_event`) and
    // tries to write against it after the frontier already advanced.
    let stale = store
        .commit_fact(trust_batch(&owner, &fact_id, 3, Some(first_event.clone())))
        .await
        .unwrap();
    match stale {
        FactCommitOutcome::Conflict(FactCommitConflict::LastEventMismatch { expected, actual }) => {
            assert_eq!(expected, Some(first_event));
            assert_eq!(actual, Some(second_event.clone()));
        }
        other => panic!("a stale write authority must get a typed conflict, got {other:?}"),
    }

    let lineage = lineage(&store, &owner, &fact_id).await;
    assert_eq!(lineage.len(), 2, "a stale conflict must not append events");
    assert_eq!(lineage[0].event_id(), &first.last_event_id().clone());
    assert_eq!(lineage[1].event_id(), &second_event);
    let current = current(&store, &owner, &fact_id).await.unwrap();
    assert_eq!(current.payload().unwrap().content(), "base fact content");
}

/// A directory guaranteed to sit outside `std::env::temp_dir()`, for fixture
/// paths that must NOT be classified as an isolated-test path by
/// `db::access::is_isolated_test_path`. `std::env::current_dir()` (the
/// package root under `cargo test`) plus a `target/...` suffix used to serve
/// this purpose, but that only holds when the checkout itself lives outside
/// the OS temp directory; a repo cloned under `/tmp` (as some sandboxed
/// CI/dev environments do) breaks that assumption. Deriving the base from
/// the running test binary's own on-disk location is robust regardless of
/// where the checkout lives, because cargo (or any build-cache shim in front
/// of it) never places build output inside the volatile system temp
/// directory.
fn ephemeral_safe_fixture_base() -> PathBuf {
    let exe = std::env::current_exe().expect("test binary has a current_exe path");
    let profile_dir = exe
        .parent() // .../target/<profile>/deps
        .and_then(Path::parent) // .../target/<profile>
        .expect("test binary sits under a cargo target profile directory")
        .to_path_buf();
    let base = profile_dir.join("tracedecay-pr7-authority");
    std::fs::create_dir_all(&base).expect("failed to create hermetic fixture base directory");
    base
}

#[tokio::test]
async fn missing_daemon_authority_fails_closed_without_a_fallback_store() {
    // A path outside every isolated-test root keeps `for_runtime` on the
    // production branch: no live daemon, no maintenance scope, no fallback.
    let root = ephemeral_safe_fixture_base().join(format!("missing-daemon-{}", std::process::id()));
    let db_path = root.join("global.db");

    let error = match DatabaseAuthority::for_runtime(&db_path, "pr7 missing daemon fixture") {
        Err(error) => error,
        Ok(_) => panic!("a missing daemon must not grant a write authority"),
    };
    assert!(
        error
            .to_string()
            .contains("managed-daemon or exclusive-maintenance authority"),
        "missing daemon must fail closed with the authority error, got {error}"
    );

    assert!(
        DatabaseAuthority::for_runtime(&db_path, "pr7 missing daemon retry").is_err(),
        "a repeated authority request must not mint a fallback writer"
    );
    assert!(
        !db_path.exists(),
        "no fallback database file may be created without an authority"
    );
    assert!(!root.join(".tracedecay-database-locks").exists());

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir(root.parent().unwrap());
}

async fn register_project(
    runtime: &HostAdmissionTestRuntimeV1,
    project_id: &str,
    root: &Path,
    common: &Path,
    remote: &str,
) {
    runtime
        .upsert_code_project(project_id, root, Some(common), Some(remote), Some("main"))
        .await
        .unwrap();
    runtime
        .upsert_store_instance(StoreInstanceUpsert {
            store_id: format!("store_{project_id}"),
            project_id: project_id.to_string(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: format!("projects/{project_id}"),
            manifest_relpath: Some(format!("projects/{project_id}/store_manifest.json")),
            last_verified_at: Some(100),
            last_write_at: Some(101),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn ambiguous_project_scope_fails_closed_and_linked_worktree_uses_canonical_identity() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let remote = "https://github.com/pr7/ambiguous.git";
    let root_a = tmp.path().join("project-a");
    let common_a = root_a.join(".git");
    let root_b = tmp.path().join("project-b");
    let common_b = root_b.join(".git");
    register_project(
        &runtime,
        "pr7.project.ambiguity-a",
        &root_a,
        &common_a,
        remote,
    )
    .await;

    let unique = runtime
        .resolve_unique_project_store_by_git_remote(remote)
        .await
        .expect("a single registered project must resolve through its remote");
    assert_eq!(unique.project.project_id, "pr7.project.ambiguity-a");

    register_project(
        &runtime,
        "pr7.project.ambiguity-b",
        &root_b,
        &common_b,
        remote,
    )
    .await;

    // Ambiguous scope: two canonical projects share one remote identity, so
    // remote-based resolution must fail closed instead of picking a store.
    assert!(
        runtime
            .resolve_unique_project_store_by_git_remote(remote)
            .await
            .is_none(),
        "an ambiguous project scope must fail closed"
    );
    let still_a = runtime
        .resolve_project_store_by_identity(&root_a, Some(&common_a))
        .await
        .unwrap()
        .expect("project A must keep its canonical identity resolution");
    assert_eq!(still_a.project.project_id, "pr7.project.ambiguity-a");
    assert_eq!(still_a.store.store_id, "store_pr7.project.ambiguity-a");

    // A linked worktree has no marker and no path alias of its own; it must
    // resolve to the primary checkout's canonical project identity through
    // the shared git common dir.
    let linked_worktree = tmp.path().join("project-a-linked-worktree");
    let linked = runtime
        .resolve_project_store_by_identity(&linked_worktree, Some(&common_a))
        .await
        .unwrap()
        .expect("a linked worktree must resolve through the canonical project identity");
    assert_eq!(linked.project.project_id, "pr7.project.ambiguity-a");
    assert_eq!(linked.store.store_id, still_a.store.store_id);

    // The canonical identity is what fact ownership binds to: the worktree's
    // facts land under the primary project owner, not a path-derived scope.
    let project_id = ProjectId::new(linked.project.project_id.clone()).unwrap();
    let owner = FactOwnerV1::Project {
        project_id: project_id.clone(),
    };
    let fact_db = fact_db(
        &tmp.path()
            .join("projects/pr7.project.ambiguity-a/memory.db"),
    )
    .await;
    let store = DatabaseFactStore::new(&fact_db);
    let batch = assertion_batch(
        &owner,
        ObservationScopeV1::Project {
            project_id: project_id.clone(),
        },
        "authority.worktree-fact",
        "entity.authority.worktree-fact",
        "fact asserted from the linked worktree",
        1,
        None,
    );
    let fact_id = batch.fact_id().clone();
    committed(store.commit_fact(batch).await.unwrap());
    let current = current(&store, &owner, &fact_id)
        .await
        .expect("the worktree fact must commit under the canonical owner");
    assert_eq!(current.owner(), &owner);

    // Unknown scope fails closed too: no identity, no store, and no alias or
    // fallback store minted as a side effect of the lookup.
    let unknown = tmp.path().join("never-registered");
    assert!(
        runtime
            .resolve_project_store_by_identity(&unknown, None)
            .await
            .unwrap()
            .is_none(),
        "an unknown project scope must fail closed"
    );
    assert!(
        runtime
            .resolve_project_store_by_alias(&unknown)
            .await
            .is_none(),
        "a failed resolution must not mint a fallback alias or store"
    );
    drop(runtime);
}

#[tokio::test]
async fn concurrent_clients_commit_one_fact_and_one_anchor_with_typed_loser_outcomes() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("memory.db");
    let db_left = fact_db(&db_path).await;
    let db_right = fact_db(&db_path).await;
    let store_left = DatabaseFactStore::new(&db_left);
    let store_right = DatabaseFactStore::new(&db_right);
    let owner = profile_owner();
    let scope = ObservationScopeV1::Profile;

    // Exact retry race: both clients submit the identical assertion batch.
    let left = assertion_batch(
        &owner,
        scope.clone(),
        "race.identical",
        "entity.race.identical",
        "identical content",
        10,
        None,
    );
    let right = assertion_batch(
        &owner,
        scope.clone(),
        "race.identical",
        "entity.race.identical",
        "identical content",
        10,
        None,
    );
    let anchor_id = left.new_anchors()[0].anchor_id().clone();
    let (left, right) = tokio::join!(store_left.commit_fact(left), store_right.commit_fact(right));
    let outcomes = [left.unwrap(), right.unwrap()];
    let committed_receipt = outcomes
        .iter()
        .find_map(|outcome| match outcome {
            FactCommitOutcome::Committed(receipt) => Some(receipt),
            FactCommitOutcome::IdempotentReplay(_) | FactCommitOutcome::Conflict(_) => None,
            _ => None,
        })
        .expect("one concurrent client must commit");
    let replayed_receipt = outcomes
        .iter()
        .find_map(|outcome| match outcome {
            FactCommitOutcome::IdempotentReplay(receipt) => Some(receipt),
            FactCommitOutcome::Committed(_) | FactCommitOutcome::Conflict(_) => None,
            _ => None,
        })
        .expect("the other concurrent client must get the idempotent replay");
    assert_eq!(committed_receipt, replayed_receipt);
    let fact_id = committed_receipt.fact_id().clone();
    assert_eq!(lineage(&store_left, &owner, &fact_id).await.len(), 1);
    let stored_anchor = store_left
        .get_retrieval_anchor(RetrievalAnchorQuery::new(owner.clone(), anchor_id.clone()).unwrap())
        .await
        .unwrap()
        .expect("the raced anchor must be committed exactly once");
    assert_eq!(stored_anchor.anchor_id(), &anchor_id);

    // Conflicting race: same fact identity, different assertion content. The
    // loser gets a typed frontier conflict, not a second committed fact.
    let left = assertion_batch(
        &owner,
        scope.clone(),
        "race.conflict",
        "entity.race.left",
        "left content",
        20,
        None,
    );
    let right = assertion_batch(
        &owner,
        scope.clone(),
        "race.conflict",
        "entity.race.right",
        "right content",
        21,
        None,
    );
    let fact_id = left.fact_id().clone();
    let left_event = left.events()[0].event_id().clone();
    let right_event = right.events()[0].event_id().clone();
    let (left, right) = tokio::join!(store_left.commit_fact(left), store_right.commit_fact(right));
    let outcomes = [left.unwrap(), right.unwrap()];
    let winner = outcomes
        .iter()
        .find_map(|outcome| match outcome {
            FactCommitOutcome::Committed(receipt) => Some(receipt),
            FactCommitOutcome::IdempotentReplay(_) | FactCommitOutcome::Conflict(_) => None,
            _ => None,
        })
        .expect("one conflicting client must commit");
    let loser = outcomes
        .iter()
        .find_map(|outcome| match outcome {
            FactCommitOutcome::Conflict(conflict) => Some(conflict),
            FactCommitOutcome::Committed(_) | FactCommitOutcome::IdempotentReplay(_) => None,
            _ => None,
        })
        .expect("the other conflicting client must get a typed conflict");
    match loser {
        FactCommitConflict::LastEventMismatch { expected, actual } => {
            assert_eq!(expected, &None);
            assert_eq!(actual, &Some(winner.last_event_id().clone()));
        }
        other => panic!("a conflicting retry must get a frontier conflict, got {other:?}"),
    }
    let winner_content = if winner.last_event_id() == &left_event {
        "left content"
    } else {
        assert_eq!(winner.last_event_id(), &right_event);
        "right content"
    };
    assert_eq!(lineage(&store_left, &owner, &fact_id).await.len(), 1);
    assert_eq!(
        current(&store_left, &owner, &fact_id)
            .await
            .unwrap()
            .payload()
            .unwrap()
            .content(),
        winner_content
    );

    // Anchor race: two different facts carry the same anchor identity with
    // conflicting content. Exactly one anchor may exist afterwards.
    let left = assertion_batch(
        &owner,
        scope.clone(),
        "race.anchor.left",
        "entity.shared",
        "anchor race left fact",
        30,
        None,
    );
    let right = assertion_batch(
        &owner,
        scope.clone(),
        "race.anchor.right",
        "entity.shared",
        "anchor race right fact",
        31,
        None,
    );
    let anchor_id = left.new_anchors()[0].anchor_id().clone();
    assert_eq!(anchor_id, right.new_anchors()[0].anchor_id().clone());
    let left_fact_id = left.fact_id().clone();
    let right_fact_id = right.fact_id().clone();
    let (left, right) = tokio::join!(store_left.commit_fact(left), store_right.commit_fact(right));
    let outcomes = [left.unwrap(), right.unwrap()];
    outcomes
        .iter()
        .find_map(|outcome| match outcome {
            FactCommitOutcome::Committed(receipt) => Some(receipt),
            FactCommitOutcome::IdempotentReplay(_) | FactCommitOutcome::Conflict(_) => None,
            _ => None,
        })
        .expect("one anchor-racing client must commit");
    let loser = outcomes
        .iter()
        .find_map(|outcome| match outcome {
            FactCommitOutcome::Conflict(conflict) => Some(conflict),
            FactCommitOutcome::Committed(_) | FactCommitOutcome::IdempotentReplay(_) => None,
            _ => None,
        })
        .expect("the other anchor-racing client must get a typed conflict");
    match loser {
        FactCommitConflict::IdentityCollision { kind, id } => {
            assert_eq!(*kind, "retrieval anchor");
            assert_eq!(id, &anchor_id.as_str().to_owned());
        }
        other => panic!("a conflicting anchor identity must be typed, got {other:?}"),
    }
    let stored_anchor = store_left
        .get_retrieval_anchor(RetrievalAnchorQuery::new(owner.clone(), anchor_id).unwrap())
        .await
        .unwrap()
        .expect("the raced anchor must be committed exactly once");
    let left_lineage = lineage(&store_left, &owner, &left_fact_id).await.len();
    let right_lineage = lineage(&store_left, &owner, &right_fact_id).await.len();
    assert_eq!(
        left_lineage + right_lineage,
        1,
        "exactly one anchor-racing fact may commit"
    );
    let winner_ingested_at = if left_lineage == 1 { 30 } else { 31 };
    assert_eq!(stored_anchor.ingested_at(), UtcMicros(winner_ingested_at));

    assert_eq!(
        current_count(&store_left, &owner).await,
        3,
        "each race must leave exactly one committed fact"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn daemon_only_writer_rejects_foreign_authority_and_shares_one_writer_token() {
    let tmp = TempDir::new().unwrap();
    let profile_root = canonical(tmp.path());
    let db_path = profile_root.join("projects/pr7-authority/memory.db");
    let lease = acquire_exclusive_for_profile(&profile_root, "pr7 single writer fixture").unwrap();
    let scope =
        enter_maintenance_database_scope(&lease, &profile_root, "pr7 single writer fixture")
            .unwrap();
    let authority = DatabaseAuthority::for_runtime(&db_path, "pr7 single writer fixture")
        .expect("a live maintenance scope must grant the write authority");
    let db = Database::publish_maintenance_test_runtime(
        &db_path,
        &authority,
        TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .unwrap()
    .0;

    // A foreign authority for the same database cannot be minted while the
    // writer authority is held: there is no second writer lane to join.
    let error = DatabaseAuthority::acquire_test(&db_path, "pr7 second writer fixture")
        .expect_err("a second write authority must be rejected");
    assert!(
        error
            .to_string()
            .contains("incompatible database authority"),
        "the second writer must fail with the authority error, got {error}"
    );

    // A client re-resolving the same database rides the retained authority —
    // one writer token, one owner — instead of opening another writer.
    let joined = DatabaseAuthority::for_runtime(&db_path, "pr7 joined client fixture")
        .expect("a client must re-join the retained authority");
    assert_eq!(joined.role(), authority.role());
    assert_eq!(
        joined.token(),
        authority.token(),
        "clients must share the single writer authority token"
    );

    let store = DatabaseFactStore::new(&db);
    let owner = profile_owner();
    let batch = assertion_batch(
        &owner,
        ObservationScopeV1::Profile,
        "authority.single-writer",
        "entity.authority.single-writer",
        "committed through the single writer authority",
        1,
        None,
    );
    let fact_id = batch.fact_id().clone();
    committed(store.commit_fact(batch).await.unwrap());
    assert_eq!(
        lineage(&store, &owner, &fact_id).await.len(),
        1,
        "the retained writer authority must keep serving after the rejection"
    );
    drop(scope);
}

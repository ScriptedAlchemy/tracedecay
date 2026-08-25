#![allow(clippy::drop_non_drop)] // explicit early drop in test
use std::process::Command;

use tempfile::TempDir;
use tracedecay::global_db::StoreInstanceUpsert;
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_domain::{
    AnchorLineageRefV2, AnchorProvenanceRelationV2, AnchorSourceGenerationV2, ClaudeSourceCursorV1,
    FactOwnerV1, ObservationScopeV1, ObservationSourceGenerationV1, ProjectId, RetrievalAnchorId,
    RetrievalAnchorRecordV2, RetrievalAnchorRecordV2Parts,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationPersistOutcome, ObservationProjectionStore,
    ObservationStore, ObservationStoreError, ObservationWrite,
};
use tracedecay_usecases::anchor_resolution::EvidenceAnchorReportResolver;
use tracedecay_usecases::host_admission::HostAdmissionScope;
use tracedecay_usecases::memory::{EvidenceAnchorResolutionError, EvidenceAnchorResolver};

use super::{
    GENERATION, ProviderObservationFixture, anchor_with_aliases, cursor,
    known_repository_provenance_write, native_observation, observation, observation_in_scope,
    provider_observation, provider_write, user_table_counts, write,
};

fn anchor_with_sources(
    anchor: &RetrievalAnchorRecordV2,
    source_anchors: Vec<AnchorLineageRefV2>,
) -> RetrievalAnchorRecordV2 {
    RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target: anchor.target().clone(),
        owner: anchor.owner().clone(),
        aliases: anchor.aliases().to_vec(),
        occurred_at: anchor.occurred_at(),
        ingested_at: anchor.ingested_at(),
        evidence_class: anchor.evidence_class(),
        source_generation: anchor.source_generation().clone(),
        projection_generation: anchor.projection_generation().clone(),
        projection_watermark: anchor.projection_watermark().clone(),
        coverage: anchor.coverage().clone(),
        source_observations: anchor.source_observations().to_vec(),
        source_anchors,
        authorization: anchor.authorization().clone(),
        payload_access: anchor.payload_access(),
        retention_class: anchor.retention_class().clone(),
        durability: anchor.durability().clone(),
    })
    .unwrap()
}

fn anchor_with_source_generation(
    anchor: &RetrievalAnchorRecordV2,
    source_generation: AnchorSourceGenerationV2,
) -> RetrievalAnchorRecordV2 {
    RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
        target: anchor.target().clone(),
        owner: anchor.owner().clone(),
        aliases: anchor.aliases().to_vec(),
        occurred_at: anchor.occurred_at(),
        ingested_at: anchor.ingested_at(),
        evidence_class: anchor.evidence_class(),
        source_generation,
        projection_generation: anchor.projection_generation().clone(),
        projection_watermark: anchor.projection_watermark().clone(),
        coverage: anchor.coverage().clone(),
        source_observations: anchor.source_observations().to_vec(),
        source_anchors: anchor.source_anchors().to_vec(),
        authorization: anchor.authorization().clone(),
        payload_access: anchor.payload_access(),
        retention_class: anchor.retention_class().clone(),
        durability: anchor.durability().clone(),
    })
    .unwrap()
}

async fn profile_runtime(tmp: &TempDir) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .unwrap()
}

async fn project_runtime(tmp: &TempDir, project_id: &ProjectId) -> HostAdmissionTestRuntimeV1 {
    let project_root = tmp.path().join("project");
    project_runtime_at(tmp, &project_root, project_id).await
}

async fn project_runtime_at(
    tmp: &TempDir,
    project_root: &std::path::Path,
    project_id: &ProjectId,
) -> HostAdmissionTestRuntimeV1 {
    std::fs::create_dir_all(project_root).unwrap();
    if !project_root.join(".git").exists() {
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(project_root)
                .status()
                .unwrap()
                .success()
        );
    }
    assert!(
        tracedecay::storage::write_repository_identity_marker(project_root, project_id.as_str())
            .unwrap()
    );
    HostAdmissionTestRuntimeV1::project(
        tmp.path().join(".tracedecay"),
        project_root,
        project_id.clone(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn copied_prompt_anchor_persists_exact_source_evidence_binding() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let source = match store
        .persist_observation(write(
            observation(0, 100, "receipt.copied-prompt.source", "source prompt"),
            None,
        ))
        .await
        .unwrap()
    {
        ObservationPersistOutcome::Committed(receipt) => receipt,
        other => panic!("source prompt persistence must commit, got {other:?}"),
    };
    let source_anchor_id = source.retrieval_anchor().anchor_id().clone();

    let copied = write(
        observation(100, 200, "receipt.copied-prompt.copy", "copied prompt"),
        Some(cursor(100)),
    );
    let (copied_write, copied_anchor, projection_generation, _) = copied.into_parts();
    let copied_from = AnchorLineageRefV2::new(
        AnchorProvenanceRelationV2::CopiedFrom,
        source_anchor_id.clone(),
        ObservationScopeV1::Profile,
    )
    .unwrap();
    let copied_anchor = anchor_with_sources(&copied_anchor, vec![copied_from.clone()]);
    let copied_anchor_id = copied_anchor.anchor_id().clone();
    store
        .persist_observation(
            AnchoredObservationWrite::new(
                copied_write,
                copied_anchor.clone(),
                projection_generation,
            )
            .unwrap(),
        )
        .await
        .unwrap();

    drop(store);
    drop(runtime);
    let runtime = profile_runtime(&tmp).await;
    let resolved = runtime
        .facade()
        .resolve_evidence_anchor(FactOwnerV1::Profile, copied_anchor_id)
        .await
        .unwrap();
    assert_eq!(resolved.record(), &copied_anchor);
    assert_eq!(resolved.record().source_anchors(), &[copied_from]);
    assert_ne!(resolved.anchor_id(), &source_anchor_id);
}

#[tokio::test]
async fn project_move_reresolves_retained_anchor_through_registered_identity() {
    let tmp = TempDir::new().unwrap();
    let profile_root = tmp.path().join(".tracedecay");
    let old_root = tmp.path().join("old").join("project");
    let moved_root = tmp.path().join("moved").join("project");
    let project_id = ProjectId::new("project.anchor-move").unwrap();
    let runtime = project_runtime_at(&tmp, &old_root, &project_id).await;
    let old_git_common_dir = tracedecay::worktree::git_common_dir(&old_root).unwrap();
    runtime
        .upsert_code_project(
            project_id.as_str(),
            &old_root,
            Some(&old_git_common_dir),
            None,
            Some("main"),
        )
        .await
        .unwrap();
    runtime
        .upsert_store_instance(StoreInstanceUpsert {
            store_id: "store_project_anchor_move".to_owned(),
            project_id: project_id.as_str().to_owned(),
            store_kind: "code_project".to_owned(),
            storage_mode: "profile_sharded".to_owned(),
            store_relpath: format!("projects/{}", project_id.as_str()),
            manifest_relpath: Some(format!(
                "projects/{}/store_manifest.json",
                project_id.as_str()
            )),
            last_verified_at: Some(100),
            last_write_at: Some(101),
        })
        .await
        .unwrap();
    let candidate = observation_in_scope(
        GENERATION,
        0,
        100,
        "receipt.anchor-project-move",
        "retained project evidence",
        ObservationScopeV1::Project {
            project_id: project_id.clone(),
        },
    );
    let anchor = {
        let store = runtime
            .observation_store(HostAdmissionScope::Project)
            .unwrap();
        match store
            .persist_observation(write(candidate, None))
            .await
            .unwrap()
        {
            ObservationPersistOutcome::Committed(receipt) => receipt.retrieval_anchor().clone(),
            other => panic!("project anchor persistence must commit, got {other:?}"),
        }
    };
    drop(runtime);

    std::fs::create_dir_all(moved_root.parent().unwrap()).unwrap();
    std::fs::rename(&old_root, &moved_root).unwrap();
    let moved_git_common_dir = tracedecay::worktree::git_common_dir(&moved_root).unwrap();
    let profile_runtime = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let resolution = profile_runtime
        .resolve_project_store_by_identity(&moved_root, Some(&moved_git_common_dir))
        .await
        .unwrap()
        .expect("moved checkout must resolve to its retained project store");
    assert_eq!(resolution.project.project_id, project_id.as_str());
    let resolved_project_id = ProjectId::new(resolution.project.project_id).unwrap();
    drop(profile_runtime);

    let runtime = HostAdmissionTestRuntimeV1::project(
        &profile_root,
        &moved_root,
        resolved_project_id.clone(),
    )
    .await
    .unwrap();
    let resolved = runtime
        .facade()
        .resolve_evidence_anchor(
            FactOwnerV1::Project {
                project_id: resolved_project_id,
            },
            anchor.anchor_id().clone(),
        )
        .await
        .unwrap();
    assert_eq!(resolved.record(), &anchor);
    assert_eq!(
        resolved.record().owner(),
        &ObservationScopeV1::Project { project_id }
    );
}

#[tokio::test]
async fn daemon_resolves_only_canonical_owner_bound_observation_anchors() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let candidate = observation(0, 100, "receipt.resolver", "stable sanitized payload");
    let receipt = match store
        .persist_observation(write(candidate, None))
        .await
        .unwrap()
    {
        ObservationPersistOutcome::Committed(receipt) => receipt,
        other => panic!("first persistence must commit, got {other:?}"),
    };
    let facade = runtime.facade();

    let resolved = facade
        .resolve_evidence_anchor(
            FactOwnerV1::Profile,
            receipt.retrieval_anchor().anchor_id().clone(),
        )
        .await
        .unwrap();
    assert_eq!(resolved.record(), receipt.retrieval_anchor());

    let error = facade
        .resolve_evidence_anchor(
            FactOwnerV1::Profile,
            RetrievalAnchorId::new("retrieval.missing").unwrap(),
        )
        .await
        .expect_err("a primary anchor cannot be unavailable");
    assert!(matches!(
        error,
        EvidenceAnchorResolutionError::Unavailable { .. }
    ));
}

#[tokio::test]
async fn daemon_denies_observation_anchor_with_corrupt_source_generation() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let receipt = match store
        .persist_observation(write(
            observation(
                0,
                100,
                "receipt.resolver-corrupt",
                "stable sanitized payload",
            ),
            None,
        ))
        .await
        .unwrap()
    {
        ObservationPersistOutcome::Committed(receipt) => receipt,
        other => panic!("first persistence must commit, got {other:?}"),
    };
    let corrupted = anchor_with_source_generation(
        receipt.retrieval_anchor(),
        AnchorSourceGenerationV2::Observation(
            ObservationSourceGenerationV1::new(GENERATION + 1).unwrap(),
        ),
    );
    let raw_conn =
        rusqlite::Connection::open(runtime.database_path(HostAdmissionScope::Profile).unwrap())
            .unwrap();
    raw_conn
        .execute_batch("DROP TRIGGER retrieval_anchors_immutable_update;")
        .unwrap();
    raw_conn
        .execute(
            "UPDATE retrieval_anchors SET anchor_json = ?1 WHERE anchor_id = ?2",
            rusqlite::params![
                serde_json::to_string(&corrupted).unwrap(),
                corrupted.anchor_id().as_str()
            ],
        )
        .unwrap();
    drop(raw_conn);

    let facade = runtime.facade();
    for error in [
        facade
            .resolve_evidence_anchor(FactOwnerV1::Profile, corrupted.anchor_id().clone())
            .await
            .expect_err("record resolution must deny corrupt source generation"),
        facade
            .resolve_evidence_anchor_report(FactOwnerV1::Profile, corrupted.anchor_id().clone())
            .await
            .expect_err("report resolution must deny corrupt source generation"),
    ] {
        let EvidenceAnchorResolutionError::Authority { source, .. } = error else {
            panic!("corrupt source generation must fail closed");
        };
        assert_eq!(
            source.to_string(),
            ObservationStoreError::RetrievalAnchorSourceGenerationMismatch.to_string()
        );
    }
}

#[tokio::test]
async fn repository_provenance_survives_restart_rebuild_and_owner_checks() {
    let tmp = TempDir::new().unwrap();
    let project_a = ProjectId::new("project.provenance-a").unwrap();
    let runtime = project_runtime(&tmp, &project_a).await;
    let candidate = observation_in_scope(
        GENERATION,
        0,
        100,
        "receipt.repository-provenance",
        "stable project-scoped payload",
        ObservationScopeV1::Project {
            project_id: project_a.clone(),
        },
    );
    let next_cursor = ClaudeSourceCursorV1::new(
        candidate.source().clone(),
        candidate.scope().clone(),
        candidate.identity().generation(),
        candidate.identity().position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(candidate.clone(), None, next_cursor).unwrap();

    let (repository_anchor_id, expected_attachment, receipt_sequence) = {
        let store = runtime
            .observation_store(HostAdmissionScope::Project)
            .unwrap();
        let receipt = match store
            .persist_observation(known_repository_provenance_write(write))
            .await
            .unwrap()
        {
            ObservationPersistOutcome::Committed(receipt) => receipt,
            other => panic!("repository provenance write must commit, got {other:?}"),
        };
        let repository_anchor_id = receipt
            .repository_provenance_attachment()
            .anchor()
            .expect("known repository provenance must retain its retrieval anchor")
            .anchor_id()
            .clone();
        let expected_attachment = receipt.repository_provenance_attachment().clone();
        let receipt_sequence = receipt.sequence();
        (repository_anchor_id, expected_attachment, receipt_sequence)
    };

    drop(runtime);
    let runtime = project_runtime(&tmp, &project_a).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Project)
        .unwrap();
    let restored = store
        .get_observation(candidate.observation_id())
        .await
        .unwrap()
        .expect("committed observation must survive database restart");
    assert_eq!(
        restored.repository_provenance_attachment(),
        &expected_attachment
    );

    let mut rebuild_complete = false;
    for _ in 0..32 {
        let outcome = store.rebuild_projection(receipt_sequence).await.unwrap();
        if outcome.is_complete() {
            rebuild_complete = true;
            break;
        }
    }
    assert!(
        rebuild_complete,
        "projection rebuild must complete within the bounded test budget"
    );
    let rebuilt = store
        .get_observation(candidate.observation_id())
        .await
        .unwrap()
        .expect("rebuild must preserve the committed observation");
    assert_eq!(
        rebuilt.repository_provenance_attachment(),
        &expected_attachment
    );

    let facade = runtime.facade();
    let resolved = facade
        .resolve_evidence_anchor(
            FactOwnerV1::Project {
                project_id: project_a.clone(),
            },
            repository_anchor_id.clone(),
        )
        .await
        .unwrap();
    assert_eq!(resolved.anchor_id(), &repository_anchor_id);

    let profile_error = facade
        .resolve_evidence_anchor(FactOwnerV1::Profile, repository_anchor_id.clone())
        .await
        .expect_err("project-scoped repository evidence must not resolve through profile scope");
    assert!(matches!(
        profile_error,
        EvidenceAnchorResolutionError::Unavailable { .. }
    ));

    let project_b_error = facade
        .resolve_evidence_anchor(
            FactOwnerV1::Project {
                project_id: ProjectId::new("project.provenance-b").unwrap(),
            },
            repository_anchor_id,
        )
        .await
        .expect_err("project A repository evidence must not resolve through project B");
    assert!(matches!(
        project_b_error,
        EvidenceAnchorResolutionError::Authority { .. }
    ));
}

#[tokio::test]
async fn retrieval_anchor_alias_collision_is_typed_and_rolls_back_the_candidate() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let first = native_observation(
        1,
        1,
        2,
        "receipt.alias.first",
        "native.alias.first",
        "first payload",
    );
    let second = provider_observation(ProviderObservationFixture {
        provider: "cursor",
        session_id: "session.alias.second",
        generation: 1,
        start: 1,
        end: 2,
        receipt_id: "receipt.alias.second",
        native_record_id: "native.alias.second",
        body: "second payload",
    });
    let second_write = provider_write(second.clone(), None);
    let alias = second_write.retrieval_anchor().aliases()[0].clone();
    let second_anchor_id = second_write.retrieval_anchor_id().clone();
    let (first_write, first_anchor, first_generation, _) = provider_write(first, None).into_parts();
    let first_anchor = anchor_with_aliases(&first_anchor, vec![alias.clone()]);
    let first_anchor_id = first_anchor.anchor_id().clone();
    store
        .persist_observation(
            AnchoredObservationWrite::new(first_write, first_anchor, first_generation).unwrap(),
        )
        .await
        .unwrap();
    let counts_before = user_table_counts(&database_path);

    let error = store
        .persist_observation(second_write)
        .await
        .expect_err("an owner-scoped native alias must identify one anchor");
    assert!(matches!(
        error,
        ObservationStoreError::RetrievalAnchorAliasCollision {
            alias: collided,
            existing_anchor_id,
            candidate_anchor_id,
        } if collided.as_ref() == &alias
            && existing_anchor_id.as_ref() == &first_anchor_id
            && candidate_anchor_id.as_ref() == &second_anchor_id
    ));
    assert!(
        store
            .get_observation(second.observation_id())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(user_table_counts(&database_path), counts_before);
}

#[tokio::test]
async fn unauthorized_anchor_resolution_is_indistinguishable_from_absence() {
    // Owner X persists a profile-scoped observation whose anchor genuinely exists.
    let owner_x = TempDir::new().unwrap();
    let runtime_x = profile_runtime(&owner_x).await;
    let store_x = runtime_x
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let candidate = observation(
        0,
        100,
        "receipt.indistinguishable",
        "owner-x sanitized payload",
    );
    let receipt = match store_x
        .persist_observation(write(candidate, None))
        .await
        .unwrap()
    {
        ObservationPersistOutcome::Committed(receipt) => receipt,
        other => panic!("owner X persistence must commit, got {other:?}"),
    };
    let existing_anchor_id = receipt.retrieval_anchor().anchor_id().clone();

    // Control: the authorized owner X still resolves its own anchor successfully.
    let facade_x = runtime_x.facade();
    let authorized = facade_x
        .resolve_evidence_anchor(FactOwnerV1::Profile, existing_anchor_id.clone())
        .await
        .expect("owner X must resolve its own anchor");
    assert_eq!(authorized.record(), receipt.retrieval_anchor());

    // Owner Y is a different, isolated authority. It must not be able to tell an
    // anchor that exists under owner X apart from one that never existed at all.
    let owner_y = TempDir::new().unwrap();
    let project_y = ProjectId::new("project.unauthorized-owner-y").unwrap();
    let runtime_y = project_runtime(&owner_y, &project_y).await;
    let facade_y = runtime_y.facade();

    let never_existed_anchor_id = RetrievalAnchorId::new("retrieval.never-existed").unwrap();
    assert_ne!(existing_anchor_id, never_existed_anchor_id);

    let owner_y_fact = || FactOwnerV1::Project {
        project_id: project_y.clone(),
    };
    let existing_outcome = facade_y
        .resolve_evidence_anchor(owner_y_fact(), existing_anchor_id.clone())
        .await
        .expect_err("owner Y must not resolve owner X's anchor");
    let absent_outcome = facade_y
        .resolve_evidence_anchor(owner_y_fact(), never_existed_anchor_id.clone())
        .await
        .expect_err("owner Y must not resolve a never-created anchor");

    // Same variant, and the only payload is the caller's own echoed request id —
    // never a signal of whether the target exists under some other owner.
    let existing_echo = match &existing_outcome {
        EvidenceAnchorResolutionError::Unavailable { anchor_id } => anchor_id.clone(),
        other => panic!("existing-but-unauthorized anchor must be Unavailable, got {other:?}"),
    };
    let absent_echo = match &absent_outcome {
        EvidenceAnchorResolutionError::Unavailable { anchor_id } => anchor_id.clone(),
        other => panic!("absent anchor must be Unavailable, got {other:?}"),
    };
    assert_eq!(existing_echo, existing_anchor_id);
    assert_eq!(absent_echo, never_existed_anchor_id);

    // Debug renders must be byte-identical once each caller's echoed request id is
    // normalized out: existence of an unauthorized target is not inferable.
    let normalize = |error: &EvidenceAnchorResolutionError, requested: &RetrievalAnchorId| {
        format!("{error:?}").replace(requested.as_str(), "<requested-anchor-id>")
    };
    assert_eq!(
        normalize(&existing_outcome, &existing_anchor_id),
        normalize(&absent_outcome, &never_existed_anchor_id),
        "an unauthorized owner must not distinguish an existing anchor from absence",
    );
}

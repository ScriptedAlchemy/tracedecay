use tempfile::TempDir;
use tracedecay::application::host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use tracedecay::application::memory::{EvidenceAnchorResolutionError, EvidenceAnchorResolver};
use tracedecay::store::GlobalDbObservationStore;
use tracedecay_domain::{
    ClaudeSourceCursorV1, FactOwnerV1, ObservationScopeV1, ProjectId, RetrievalAnchorId,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationPersistOutcome, ObservationProjectionStore,
    ObservationStore, ObservationStoreError, ObservationWrite,
};

use super::{
    GENERATION, ProviderObservationFixture, anchor_with_aliases, known_repository_provenance_write,
    native_observation, observation, observation_in_scope, provider_observation, provider_write,
    user_table_counts, write,
};
use crate::common::open_lcm_db;

#[tokio::test]
async fn daemon_resolves_only_canonical_owner_bound_observation_anchors() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(0, 100, "receipt.resolver", "stable sanitized payload");
    let receipt = match store
        .persist_observation(write(candidate, None))
        .await
        .unwrap()
    {
        ObservationPersistOutcome::Committed(receipt) => receipt,
        other => panic!("first persistence must commit, got {other:?}"),
    };
    let facade = HostAdmissionFacade::new(HostAdmissionAuthorities::for_profile(&db));

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
async fn repository_provenance_survives_restart_rebuild_and_owner_checks() {
    let tmp = TempDir::new().unwrap();
    let project_a = ProjectId::new("project.provenance-a").unwrap();
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
        let db = open_lcm_db(&tmp).await;
        let store = GlobalDbObservationStore::new(&db);
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

    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
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

    let facade = HostAdmissionFacade::new(
        HostAdmissionAuthorities::for_project(&db, project_a.clone()).with_profile(&db),
    );
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
        EvidenceAnchorResolutionError::Authority { .. }
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
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
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
    let counts_before = user_table_counts(&tmp).await;

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
    assert_eq!(user_table_counts(&tmp).await, counts_before);
}

#[tokio::test]
async fn unauthorized_anchor_resolution_is_indistinguishable_from_absence() {
    // Owner X persists a profile-scoped observation whose anchor genuinely exists.
    let owner_x = TempDir::new().unwrap();
    let db_x = open_lcm_db(&owner_x).await;
    let store_x = GlobalDbObservationStore::new(&db_x);
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
    let facade_x = HostAdmissionFacade::new(HostAdmissionAuthorities::for_profile(&db_x));
    let authorized = facade_x
        .resolve_evidence_anchor(FactOwnerV1::Profile, existing_anchor_id.clone())
        .await
        .expect("owner X must resolve its own anchor");
    assert_eq!(authorized.record(), receipt.retrieval_anchor());

    // Owner Y is a different, isolated authority. It must not be able to tell an
    // anchor that exists under owner X apart from one that never existed at all.
    let owner_y = TempDir::new().unwrap();
    let db_y = open_lcm_db(&owner_y).await;
    let project_y = ProjectId::new("project.unauthorized-owner-y").unwrap();
    let facade_y = HostAdmissionFacade::new(HostAdmissionAuthorities::for_project(
        &db_y,
        project_y.clone(),
    ));

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

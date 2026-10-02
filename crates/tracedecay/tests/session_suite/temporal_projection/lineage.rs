use super::*;

#[tokio::test]
async fn only_explicit_typed_copy_proof_persists_copy_edges() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.copy");
    let first = persist_observation(&observation_store, &session_id, 0, "same text").await;
    let second = persist_observation(&observation_store, &session_id, 1, "same text").await;
    let first = occurrence(&session_id, &first);
    let second = occurrence(&session_id, &second);
    let candidate = begin_candidate(&store, &session_id, 2).await;
    persist_batch(
        &store,
        &candidate,
        batch(&candidate, vec![first.clone()], vec![], vec![])
            .with_checkpoint(0, 1, 1)
            .unwrap(),
    )
    .await
    .unwrap();

    let mut forged = parent_message_copy(&second, &first);
    forged.proof = CopyProofV1::ProviderLinkage {
        source_occurrence_id: first.occurrence_id.clone(),
        provider_record_id: ObservationId::new("provider.copy.nonexistent").unwrap(),
    };
    assert!(
        persist_batch(
            &store,
            &candidate,
            batch(&candidate, vec![second.clone()], vec![forged], vec![])
                .with_checkpoint(1, 2, 2)
                .unwrap(),
        )
        .await
        .is_err()
    );

    persist_batch(
        &store,
        &candidate,
        batch(
            &candidate,
            vec![second.clone()],
            vec![parent_message_copy(&second, &first)],
            vec![],
        )
        .with_checkpoint(1, 2, 2)
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        scalar_runtime(
            &runtime,
            "SELECT COUNT(*) FROM session_occurrences_fts
             WHERE session_occurrences_fts MATCH 'same'"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn each_typed_assertion_relation_authorizes_only_its_matching_kind() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.typed-assertions");
    let mut occurrences = Vec::new();
    let mut assertions = Vec::new();
    for (index, (kind, relation)) in [
        (
            TemporalAssertionKindV1::Corrects,
            AnchorProvenanceRelation::Corrects,
        ),
        (
            TemporalAssertionKindV1::Contradicts,
            AnchorProvenanceRelation::Contradicts,
        ),
        (
            TemporalAssertionKindV1::Supersedes,
            AnchorProvenanceRelation::Supersedes,
        ),
        (
            TemporalAssertionKindV1::Supports,
            AnchorProvenanceRelation::Supports,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let object_ordinal = u64::try_from(index * 2).unwrap();
        let subject_ordinal = object_ordinal + 1;
        let object_observation =
            persist_observation(&observation_store, &session_id, object_ordinal, "object").await;
        let object = occurrence(&session_id, &object_observation);
        let subject_observation = persist_observation_with_lineage(
            &observation_store,
            &session_id,
            subject_ordinal,
            "subject",
            relation,
            object.retrieval_anchor_id.clone(),
            None,
        )
        .await;
        let subject = occurrence(&session_id, &subject_observation);
        assertions.push(assertion_with_kind(kind, &subject, &object));
        occurrences.extend([object, subject]);
    }
    let candidate = begin_candidate(&store, &session_id, 8).await;

    persist_batch(
        &store,
        &candidate,
        batch(&candidate, occurrences, vec![], assertions),
    )
    .await
    .unwrap();

    assert_eq!(
        rows_runtime(
            &runtime,
            "SELECT assertion_kind FROM session_assertions ORDER BY assertion_kind"
        )
        .await,
        vec!["contradicts", "corrects", "supersedes", "supports"]
    );
}

#[tokio::test]
async fn mismatched_typed_assertion_relation_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.mismatched-assertion");
    let object_observation =
        persist_observation(&observation_store, &session_id, 0, "object").await;
    let object = occurrence(&session_id, &object_observation);
    let subject_observation = persist_observation_with_lineage(
        &observation_store,
        &session_id,
        1,
        "subject",
        AnchorProvenanceRelation::Supports,
        object.retrieval_anchor_id.clone(),
        None,
    )
    .await;
    let subject = occurrence(&session_id, &subject_observation);
    let candidate = begin_candidate(&store, &session_id, 2).await;

    assert!(matches!(
        persist_batch(
            &store,
            &candidate,
            batch(
                &candidate,
                vec![object.clone(), subject.clone()],
                vec![],
                vec![assertion_with_kind(
                    TemporalAssertionKindV1::Contradicts,
                    &subject,
                    &object,
                )],
            )
        )
        .await,
        Err(SessionStoreError::Storage { .. })
    ));
}

#[tokio::test]
async fn parent_message_without_typed_assertion_lineage_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.parent-only-assertion");
    let object = occurrence(
        &session_id,
        &persist_observation(&observation_store, &session_id, 0, "object").await,
    );
    let subject = occurrence(
        &session_id,
        &persist_observation(&observation_store, &session_id, 1, "subject").await,
    );
    let candidate = begin_candidate(&store, &session_id, 2).await;

    assert!(matches!(
        persist_batch(
            &store,
            &candidate,
            batch(
                &candidate,
                vec![object.clone(), subject.clone()],
                vec![],
                vec![assertion_with_kind(
                    TemporalAssertionKindV1::Corrects,
                    &subject,
                    &object,
                )],
            )
        )
        .await,
        Err(SessionStoreError::Storage { .. })
    ));
}

#[tokio::test]
async fn parent_message_linkage_copy_proof_requires_exact_parent_id() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.parent-linkage");
    let first = persist_observation(&observation_store, &session_id, 0, "parent").await;
    let second = persist_observation(&observation_store, &session_id, 1, "child").await;
    let first = occurrence(&session_id, &first);
    let second = occurrence(&session_id, &second);
    let candidate = begin_candidate(&store, &session_id, 2).await;
    persist_batch(
        &store,
        &candidate,
        batch(&candidate, vec![first.clone()], vec![], vec![])
            .with_checkpoint(0, 1, 1)
            .unwrap(),
    )
    .await
    .unwrap();

    let mut mismatched = parent_message_copy(&second, &first);
    mismatched.proof = CopyProofV1::ParentMessageLinkage {
        source_occurrence_id: first.occurrence_id.clone(),
        parent_message_id: MessageId::new("message.temporal.forged").unwrap(),
    };
    let error = persist_batch(
        &store,
        &candidate,
        batch(&candidate, vec![second.clone()], vec![mismatched], vec![])
            .with_checkpoint(1, 2, 2)
            .unwrap(),
    )
    .await
    .expect_err("a forged parent message id must not prove linkage");
    assert_eq!(
        error.to_string(),
        "session-temporal storage operation persist session temporal projection batch failed: copy proof is not supported by retained provider, parent-message, or CopiedFrom anchor evidence"
    );

    persist_batch(
        &store,
        &candidate,
        batch(
            &candidate,
            vec![second.clone()],
            vec![parent_message_copy(&second, &first)],
            vec![],
        )
        .with_checkpoint(1, 2, 2)
        .unwrap(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn copied_from_requires_explicit_typed_copy_record() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let observation_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let store = runtime
        .session_temporal_store(HostAdmissionScope::Profile)
        .unwrap();
    let session_id = session("session.temporal.copied-from-explicit");
    let first = occurrence(
        &session_id,
        &persist_observation(&observation_store, &session_id, 0, "source").await,
    );
    let second_observation = persist_custom_observation_with_lineage(
        &observation_store,
        observation_with_message_ids(&session_id, 1, "copy", "message.temporal.copy", None),
        AnchorProvenanceRelation::CopiedFrom,
        first.retrieval_anchor_id.clone(),
    )
    .await;
    let second =
        occurrence_with_message_id(&session_id, &second_observation, "message.temporal.copy");
    let candidate = begin_candidate(&store, &session_id, 2).await;
    persist_batch(
        &store,
        &candidate,
        batch(&candidate, vec![first.clone()], vec![], vec![])
            .with_checkpoint(0, 1, 1)
            .unwrap(),
    )
    .await
    .unwrap();
    persist_batch(
        &store,
        &candidate,
        batch(
            &candidate,
            vec![second.clone()],
            vec![explicit_anchor_copy(&second, &first)],
            vec![],
        )
        .with_checkpoint(1, 2, 2)
        .unwrap(),
    )
    .await
    .unwrap();
    complete_candidate(&store, &candidate).await.unwrap();
    assert_eq!(
        rows_runtime(
            &runtime,
            "SELECT generation || ':' || state
             FROM session_temporal_generations
             WHERE session_id = 'session.temporal.copied-from-explicit'
             ORDER BY generation"
        )
        .await,
        vec!["1:superseded", "2:active"]
    );
}

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
    begin_candidate(&store, &session_id, 2, 2).await;

    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            2,
            vec![first.clone(), second.clone()],
            vec![],
            vec![],
        ))
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

    let mut forged = parent_message_copy(&second, &first);
    forged.proof = CopyProofV1::ProviderLinkage {
        source_occurrence_id: first.occurrence_id.clone(),
        provider_record_id: ObservationId::new("provider.copy.nonexistent").unwrap(),
    };
    assert!(
        store
            .persist_session_temporal_projection_batch(
                batch(&session_id, 2, 2, vec![], vec![forged], vec![])
                    .with_checkpoint(1, 2, 2)
                    .unwrap(),
            )
            .await
            .is_err()
    );

    store
        .persist_session_temporal_projection_batch(
            batch(
                &session_id,
                2,
                2,
                vec![],
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
            AnchorProvenanceRelationV2::Corrects,
        ),
        (
            TemporalAssertionKindV1::Contradicts,
            AnchorProvenanceRelationV2::Contradicts,
        ),
        (
            TemporalAssertionKindV1::Supersedes,
            AnchorProvenanceRelationV2::Supersedes,
        ),
        (
            TemporalAssertionKindV1::Supports,
            AnchorProvenanceRelationV2::Supports,
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
    begin_candidate(&store, &session_id, 2, 8).await;

    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            8,
            occurrences,
            vec![],
            assertions,
        ))
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
        AnchorProvenanceRelationV2::Supports,
        object.retrieval_anchor_id.clone(),
        None,
    )
    .await;
    let subject = occurrence(&session_id, &subject_observation);
    begin_candidate(&store, &session_id, 2, 2).await;

    assert!(matches!(
        store
            .persist_session_temporal_projection_batch(batch(
                &session_id,
                2,
                2,
                vec![object.clone(), subject.clone()],
                vec![],
                vec![assertion_with_kind(
                    TemporalAssertionKindV1::Contradicts,
                    &subject,
                    &object,
                )],
            ))
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
    begin_candidate(&store, &session_id, 2, 2).await;

    assert!(matches!(
        store
            .persist_session_temporal_projection_batch(batch(
                &session_id,
                2,
                2,
                vec![object.clone(), subject.clone()],
                vec![],
                vec![assertion_with_kind(
                    TemporalAssertionKindV1::Corrects,
                    &subject,
                    &object,
                )],
            ))
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
    begin_candidate(&store, &session_id, 2, 2).await;
    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            2,
            vec![first.clone(), second.clone()],
            vec![],
            vec![],
        ))
        .await
        .unwrap();

    let mut mismatched = parent_message_copy(&second, &first);
    mismatched.proof = CopyProofV1::ParentMessageLinkage {
        source_occurrence_id: first.occurrence_id.clone(),
        parent_message_id: MessageId::new("message.temporal.forged").unwrap(),
    };
    assert!(
        store
            .persist_session_temporal_projection_batch(
                batch(&session_id, 2, 2, vec![], vec![mismatched], vec![])
                    .with_checkpoint(1, 2, 2)
                    .unwrap(),
            )
            .await
            .is_err()
    );

    store
        .persist_session_temporal_projection_batch(
            batch(
                &session_id,
                2,
                2,
                vec![],
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
        AnchorProvenanceRelationV2::CopiedFrom,
        first.retrieval_anchor_id.clone(),
    )
    .await;
    let second =
        occurrence_with_message_id(&session_id, &second_observation, "message.temporal.copy");
    begin_candidate(&store, &session_id, 2, 2).await;
    store
        .persist_session_temporal_projection_batch(batch(
            &session_id,
            2,
            2,
            vec![first.clone(), second.clone()],
            vec![],
            vec![],
        ))
        .await
        .unwrap();
    store
        .persist_session_temporal_projection_batch(
            batch(
                &session_id,
                2,
                2,
                vec![],
                vec![explicit_anchor_copy(&second, &first)],
                vec![],
            )
            .with_checkpoint(1, 2, 2)
            .unwrap(),
        )
        .await
        .unwrap();
    store
        .activate_session_temporal_generation(
            SessionGenerationActivationRequestV1::new(
                session_id.clone(),
                generation(2),
                snapshot(&session_id, 1, 2),
                ExecutionControl::default(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
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

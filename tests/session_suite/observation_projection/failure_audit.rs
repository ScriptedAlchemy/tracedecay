use super::*;

#[tokio::test]
async fn projection_failure_rolls_back_effect_fts_provenance_checkpoint_and_queue() {
    for (stage, trigger) in [
        ("message", "BEFORE INSERT ON session_messages"),
        (
            "provenance",
            "BEFORE INSERT ON observation_projection_provenance",
        ),
        ("dequeue", "BEFORE DELETE ON projection_queue"),
        (
            "checkpoint",
            "BEFORE INSERT ON observation_projection_checkpoints",
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let runtime = profile_runtime(&tmp).await;
        let store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        let database_path = runtime
            .database_path(HostAdmissionScope::Profile)
            .unwrap()
            .to_path_buf();
        let message_id = format!("message-rollback-{stage}");
        let searchable = format!("rollback searchable {stage}");
        let candidate = observation(
            &format!("session-rollback-{stage}"),
            0,
            100,
            &format!("receipt.rollback-{stage}"),
            conversational_payload(&message_id, &searchable),
        );
        persist(&store, candidate.clone(), None).await;

        let raw_conn = rusqlite::Connection::open(&database_path).unwrap();
        raw_conn
            .execute_batch(&format!(
                "CREATE TRIGGER fail_projection_{stage}
                 {trigger} BEGIN
                    SELECT RAISE(ABORT, 'injected projection {stage} failure');
                 END;"
            ))
            .unwrap();

        let error = store
            .project_observation(candidate.observation_id())
            .await
            .expect_err("injected projection failure must abort the transaction");
        assert!(
            matches!(error, ProjectionStoreError::Storage { .. }),
            "{stage} failure surfaced as {error:?}"
        );

        drop(raw_conn);
        drop(runtime);

        let reopened_runtime = profile_runtime(&tmp).await;
        let reopened_store = reopened_runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        assert_eq!(
            reopened_store
                .projection_checkpoint()
                .await
                .unwrap()
                .last_sequence(),
            0,
            "{stage} failure advanced the checkpoint"
        );
        assert_eq!(
            projection_counts(&tmp).await,
            (0, 0, 0, 0, 0, 1),
            "{stage} failure committed partial projection state"
        );
        assert_eq!(
            (
                table_count(&tmp, "sanitization_receipts").await,
                table_count(&tmp, "observations").await,
                table_count(&tmp, "source_cursors").await,
            ),
            (1, 1, 1),
            "{stage} failure changed durable ingestion rows"
        );
        assert!(
            projection_provenance_rows(&tmp).await.is_empty(),
            "{stage} failure committed projection provenance"
        );
        assert!(
            search_session_messages(&tmp, &searchable, 10)
                .await
                .is_empty(),
            "{stage} failure leaked a searchable message"
        );
        let stored = reopened_store
            .get_observation(candidate.observation_id())
            .await
            .unwrap()
            .expect("failed projection must preserve the durable observation");
        assert_eq!(
            stored.observation(),
            &candidate,
            "{stage} failure changed the durable observation"
        );
        assert_eq!(
            stored.projection_status(),
            ObservationProjectionStatus::Queued,
            "{stage} failure consumed the queue item"
        );

        let trigger_conn = rusqlite::Connection::open(&database_path).unwrap();
        trigger_conn
            .execute_batch(&format!("DROP TRIGGER fail_projection_{stage};"))
            .unwrap();
        drop(trigger_conn);
        reopened_store
            .project_observation(candidate.observation_id())
            .await
            .unwrap();
        let recovered_counts = projection_counts(&tmp).await;
        assert_eq!(recovered_counts, (1, 1, 1, 1, 0, 0));
        assert_eq!(
            search_session_messages(&tmp, &searchable, 10).await.len(),
            1
        );
        let replay = reopened_store
            .project_observation(candidate.observation_id())
            .await
            .unwrap();
        assert!(matches!(
            replay,
            ProjectionPersistOutcome::ExactDuplicate(_)
        ));
        assert_eq!(projection_counts(&tmp).await, recovered_counts);
    }
}

#[tokio::test]
async fn divergent_output_collision_converges_as_a_skip_without_overwriting() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let first = observation(
        "session-collision",
        0,
        100,
        "receipt.collision-first",
        conversational_payload("shared-message", "original collision canary"),
    );
    let second = observation(
        "session-collision",
        100,
        200,
        "receipt.collision-second",
        conversational_payload("shared-message", "divergent collision canary"),
    );
    persist(&store, first.clone(), None).await;
    persist(
        &store,
        second.clone(),
        Some(cursor("session-collision", 100)),
    )
    .await;
    store
        .project_observation(first.observation_id())
        .await
        .unwrap();
    let counts_before = projection_counts(&tmp).await;

    // A divergent output for an already-bound identity never overwrites the
    // first binder; the second observation converges as a durable, auditable
    // skip so the projection queue keeps draining.
    match store
        .project_observation(second.observation_id())
        .await
        .unwrap()
    {
        ProjectionPersistOutcome::Skipped { reason, .. } => assert_eq!(
            reason,
            tracedecay_store::ProjectionSkipReason::OutputCollision
        ),
        other => panic!("expected a collision skip, got {other:?}"),
    }
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        2
    );
    let counts_after = projection_counts(&tmp).await;
    assert_eq!(
        (counts_after.0, counts_after.1, counts_after.2),
        (counts_before.0, counts_before.1, counts_before.2),
        "no session, message, or provenance rows may change on a collision"
    );
    assert_eq!(
        counts_after.4,
        counts_before.4 + 1,
        "the collision must record exactly one disposition"
    );
    assert_eq!(counts_after.5, 0, "the projection queue must converge");
    assert_eq!(
        search_session_messages(&tmp, "original collision", 10)
            .await
            .len(),
        1
    );
    let texts = projected_message_texts(&tmp).await;
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("original collision canary"));
    assert!(!texts[0].contains("divergent collision canary"));

    // A full rebuild reaches the same converged state: first binder kept,
    // second staged as a skip.
    store
        .rebuild_projection(2)
        .await
        .expect("rebuild must converge past divergent-output collisions");
    let texts = projected_message_texts(&tmp).await;
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("original collision canary"));
}

#[tokio::test]
async fn authority_reopen_accepts_historical_generation_after_supersession() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let original = observation_in_generation(
        "session-authority-supersession",
        GENERATION,
        0,
        100,
        "receipt.authority-supersession-original",
        conversational_payload("message-authority-supersession", "superseded body"),
    );
    let replacement = observation_in_generation(
        "session-authority-supersession",
        GENERATION + 1,
        0,
        100,
        "receipt.authority-supersession-replacement",
        conversational_payload("message-authority-supersession", "current body"),
    );
    persist(&store, original, None).await;
    persist(
        &store,
        replacement,
        Some(cursor_in_generation(
            "session-authority-supersession",
            GENERATION,
            100,
        )),
    )
    .await;
    drain_projection_queue(&store).await;
    drop(runtime);

    let reopened = HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
        .await
        .expect("historical provenance must validate against the current output owner");
    drop(reopened);
    assert!(projected_message_texts(&tmp).await[0].contains("current body"));
    assert_eq!(
        table_count(&tmp, "observation_projection_provenance").await,
        2
    );
}

#[tokio::test]
async fn projected_message_update_invalidates_audit_and_fails_reopen() {
    let tmp = audited_projection_fixture("session-audit-update", "message-audit-update").await;
    let runtime = profile_runtime(&tmp).await;
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    drop(runtime);
    let raw_conn = rusqlite::Connection::open(database_path).unwrap();
    raw_conn
        .execute(
            "UPDATE session_messages SET text = 'tampered projection body'
             WHERE provider = 'claude' AND message_id = 'message-audit-update'",
            (),
        )
        .unwrap();
    drop(raw_conn);

    assert!(
        HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn projected_message_delete_invalidates_audit_and_fails_reopen() {
    let tmp = audited_projection_fixture("session-audit-delete", "message-audit-delete").await;
    let runtime = profile_runtime(&tmp).await;
    let database_path = runtime
        .database_path(HostAdmissionScope::Profile)
        .unwrap()
        .to_path_buf();
    drop(runtime);
    let raw_conn = rusqlite::Connection::open(database_path).unwrap();
    raw_conn
        .execute(
            "DELETE FROM session_messages
             WHERE provider = 'claude' AND message_id = 'message-audit-delete'",
            (),
        )
        .unwrap();
    drop(raw_conn);

    assert!(
        HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
            .await
            .is_err()
    );
}

use super::*;

#[tokio::test]
async fn exact_v1_message_is_adopted_and_richer_session_survives_rebuild() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let candidate = observation(
        "session-v1",
        0,
        100,
        "receipt.v1",
        conversational_payload("message-v1", "v1 parity canary"),
    );
    persist(&store, candidate.clone(), None).await;

    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "INSERT INTO sessions
            (provider, session_id, project_key, project_path, title, is_subagent)
         VALUES (?1, ?2, 'legacy-project', '/legacy/project', 'V1 title', 0)",
        libsql::params!["claude", "session-v1"],
    )
    .await
    .unwrap();
    let metadata_json = serde_json::to_string(&json!({
        "source": "claude_transcript",
        "raw_type": "assistant",
        "source_generation": GENERATION,
    }))
    .unwrap();
    conn.execute(
        "INSERT INTO session_messages
            (provider, message_id, session_id, role, timestamp, ordinal, text, kind, model,
             tool_names, source_path, source_offset, metadata_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        libsql::params![
            "claude",
            "message-v1",
            "session-v1",
            "assistant",
            1_750_000_000_i64,
            0_i64,
            serde_json::to_string(&json!([{"type": "text", "text": "v1 parity canary"}])).unwrap(),
            "message",
            "claude-sonnet-4",
            Option::<String>::None,
            "claude:session-v1",
            0_i64,
            metadata_json,
        ],
    )
    .await
    .unwrap();

    store
        .project_observation(candidate.observation_id())
        .await
        .unwrap();
    assert_eq!(projection_ownership_rows(&tmp).await, vec![0]);
    assert_eq!(projection_counts(&tmp).await, (1, 1, 1, 1, 0, 0));

    rebuild_projection_to_completion(&store, 0).await;
    assert_eq!(projection_counts(&tmp).await, (1, 1, 0, 1, 0, 1));
    rebuild_projection_to_completion(&store, 1).await;
    assert_eq!(projection_ownership_rows(&tmp).await, vec![0]);
    assert_eq!(projection_counts(&tmp).await, (1, 1, 1, 1, 0, 0));
    let mut rows = conn
        .query(
            "SELECT project_key, project_path, title FROM sessions
             WHERE provider = 'claude' AND session_id = 'session-v1'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "legacy-project");
    assert_eq!(row.get::<String>(1).unwrap(), "/legacy/project");
    assert_eq!(row.get::<String>(2).unwrap(), "V1 title");
}

#[tokio::test]
async fn adopted_message_is_not_mutated_by_rollover_and_rebuilds_cleanly() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let original = observation_in_generation(
        "session-adopted-rollover",
        GENERATION,
        0,
        100,
        "receipt.adopted-rollover-1",
        conversational_payload("message-adopted-rollover", "adopted original canary"),
    );
    let replacement = observation_in_generation(
        "session-adopted-rollover",
        GENERATION + 1,
        0,
        100,
        "receipt.adopted-rollover-2",
        conversational_payload(
            "message-adopted-rollover",
            "adopted replacement must not appear",
        ),
    );
    persist(&store, original.clone(), None).await;
    persist(
        &store,
        replacement.clone(),
        Some(cursor_in_generation(
            "session-adopted-rollover",
            GENERATION,
            100,
        )),
    )
    .await;

    let raw_db = libsql::Builder::new_local(isolated_lcm_db_path(&tmp))
        .build()
        .await
        .unwrap();
    let conn = raw_db.connect().unwrap();
    conn.execute(
        "INSERT INTO sessions
            (provider, session_id, project_key, project_path, is_subagent)
         VALUES ('claude', 'session-adopted-rollover', 'user', 'user', 0)",
        (),
    )
    .await
    .unwrap();
    let metadata_json = serde_json::to_string(&json!({
        "source": "claude_transcript",
        "raw_type": "assistant",
        "source_generation": GENERATION,
    }))
    .unwrap();
    conn.execute(
        "INSERT INTO session_messages
            (provider, message_id, session_id, role, timestamp, ordinal, text, kind, model,
             tool_names, source_path, source_offset, metadata_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        libsql::params![
            "claude",
            "message-adopted-rollover",
            "session-adopted-rollover",
            "assistant",
            1_750_000_000_i64,
            0_i64,
            serde_json::to_string(&json!([{"type": "text", "text": "adopted original canary"}]))
                .unwrap(),
            "message",
            "claude-sonnet-4",
            Option::<String>::None,
            "claude:session-adopted-rollover",
            0_i64,
            metadata_json,
        ],
    )
    .await
    .unwrap();

    drain_projection_queue(&store).await;
    assert_eq!(projection_ownership_rows(&tmp).await, vec![0, 0]);
    let texts = projected_message_texts(&tmp).await;
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("adopted original canary"));
    assert!(!texts[0].contains("adopted replacement must not appear"));
    assert!(matches!(
        store
            .project_observation(replacement.observation_id())
            .await
            .unwrap(),
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));

    rebuild_projection_to_completion(&store, 0).await;
    assert_eq!(projection_counts(&tmp).await, (1, 1, 0, 1, 0, 2));
    drain_projection_queue(&store).await;
    assert_eq!(projection_counts(&tmp).await, (1, 1, 2, 1, 0, 0));
    assert_eq!(projection_ownership_rows(&tmp).await, vec![0, 0]);
    let texts = projected_message_texts(&tmp).await;
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("adopted original canary"));
    assert!(!texts[0].contains("adopted replacement must not appear"));
}

#[tokio::test]
async fn reused_message_id_across_sources_converges_without_adoption() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store = GlobalDbObservationStore::new(&db);
    let first = observation_in_generation(
        "session-reused-id-a",
        GENERATION,
        0,
        100,
        "receipt.reused-id-a",
        conversational_payload("shared-cross-source", "cross-source original canary"),
    );
    let second = observation_in_generation(
        "session-reused-id-b",
        GENERATION + 1,
        0,
        100,
        "receipt.reused-id-b",
        conversational_payload("shared-cross-source", "cross-source replacement canary"),
    );
    persist(&store, first.clone(), None).await;
    persist(&store, second.clone(), None).await;
    store
        .project_observation(first.observation_id())
        .await
        .unwrap();
    let provenance_before = projection_provenance_rows(&tmp).await;
    let texts_before = projected_message_texts(&tmp).await;

    // A reused message ID from another typed source must not adopt or
    // replace the first binder's output; it converges as a durable skip.
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
    assert_eq!(projection_provenance_rows(&tmp).await, provenance_before);
    assert_eq!(projected_message_texts(&tmp).await, texts_before);
    assert_eq!(texts_before.len(), 1);
    assert!(texts_before[0].contains("cross-source original canary"));
    assert_eq!(
        store
            .get_observation(second.observation_id())
            .await
            .unwrap()
            .unwrap()
            .projection_status(),
        ObservationProjectionStatus::NotQueued,
        "the disposed observation must leave the queue"
    );
}

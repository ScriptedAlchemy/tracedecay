use super::*;

#[tokio::test]
async fn reordered_delivery_then_frozen_frontier_rebuild_converges() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let observations = [
        observation(
            "session-rebuild",
            0,
            100,
            "receipt.rebuild-1",
            conversational_payload("message-rebuild-1", "frozen frontier alpha"),
        ),
        observation(
            "session-rebuild",
            100,
            200,
            "receipt.rebuild-2",
            conversational_payload("message-rebuild-2", "frozen frontier beta"),
        ),
        observation(
            "session-rebuild",
            200,
            300,
            "receipt.rebuild-3",
            conversational_payload("message-rebuild-3", "past frontier gamma"),
        ),
    ];
    let mut expected_cursor = None;
    for candidate in &observations {
        persist(&store, candidate.clone(), expected_cursor.clone()).await;
        expected_cursor = Some(cursor(
            "session-rebuild",
            candidate.identity().position().end(),
        ));
    }

    let error = store
        .project_observation(observations[1].observation_id())
        .await
        .expect_err("out-of-order projection must not skip the checkpoint frontier");
    assert!(matches!(
        error,
        ProjectionStoreError::Gap {
            expected: 1,
            actual: 2
        }
    ));
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        0
    );
    assert_eq!(projection_counts(&tmp).await, (0, 0, 0, 0, 0, 3));

    for candidate in &observations[..2] {
        store
            .project_observation(candidate.observation_id())
            .await
            .unwrap();
    }
    let mut rows_before = search_session_messages(&tmp, "frozen frontier", 10)
        .await
        .into_iter()
        .map(|hit| (hit.message.message_id, hit.message.text))
        .collect::<Vec<_>>();
    rows_before.sort();
    let counts_before = projection_counts(&tmp).await;
    let provenance_before = projection_provenance_rows(&tmp).await;
    let raw_store_ids_before = projected_raw_store_ids(&tmp).await;
    assert_eq!(counts_before, (1, 2, 2, 1, 0, 1));
    let anchor_store_id = raw_store_ids_before[0].1;
    let raw_conn = rusqlite::Connection::open(isolated_lcm_db_path(&tmp)).unwrap();
    raw_conn
        .execute(
            "INSERT INTO lcm_summary_nodes (
                node_id, provider, conversation_id, session_id, depth, summary_text,
                summary_hash, summary_token_count, source_token_count
             ) VALUES (
                'summary.rebuild-store-id', 'claude', 'session-rebuild',
                'session-rebuild', 0, 'stable raw identity summary', 'hash.fixture', 4, 8
             )",
            (),
        )
        .unwrap();
    raw_conn
        .execute(
            "INSERT INTO lcm_summary_sources (node_id, source_kind, source_id, ordinal)
             VALUES ('summary.rebuild-store-id', 'raw_message', ?1, 0)",
            rusqlite::params![anchor_store_id.to_string()],
        )
        .unwrap();
    raw_conn
        .execute(
            "INSERT INTO lcm_lifecycle_state (
                provider, conversation_id, current_session_id, current_frontier_store_id
             ) VALUES ('claude', 'session-rebuild', 'session-rebuild', ?1)",
            rusqlite::params![anchor_store_id],
        )
        .unwrap();
    drop(raw_conn);

    let rebuilt = rebuild_projection_to_completion(&store, 2).await;
    assert_eq!(rebuilt.checkpoint().last_sequence(), 2);
    assert_eq!(rebuilt.projected_rows(), 2);
    let mut rows_after = search_session_messages(&tmp, "frozen frontier", 10)
        .await
        .into_iter()
        .map(|hit| (hit.message.message_id, hit.message.text))
        .collect::<Vec<_>>();
    rows_after.sort();
    assert_eq!(rows_after, rows_before);
    assert_eq!(projected_raw_store_ids(&tmp).await, raw_store_ids_before);
    let raw_conn = rusqlite::Connection::open(isolated_lcm_db_path(&tmp)).unwrap();
    let identity = raw_conn
        .query_row(
            "SELECT source.source_id, lifecycle.current_frontier_store_id
             FROM lcm_summary_sources AS source
             JOIN lcm_lifecycle_state AS lifecycle
               ON lifecycle.provider = 'claude'
              AND lifecycle.conversation_id = 'session-rebuild'
             WHERE source.node_id = 'summary.rebuild-store-id'",
            (),
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .unwrap();
    assert_eq!(identity.0, anchor_store_id.to_string());
    assert_eq!(identity.1, anchor_store_id);
    drop(raw_conn);
    assert_eq!(projection_provenance_rows(&tmp).await, provenance_before);
    assert_eq!(projection_counts(&tmp).await, counts_before);
    assert!(
        projected_message_texts(&tmp)
            .await
            .iter()
            .all(|text| !text.contains("past frontier gamma"))
    );
    assert_eq!(
        store
            .get_observation(observations[2].observation_id())
            .await
            .unwrap()
            .unwrap()
            .projection_status(),
        ObservationProjectionStatus::Queued
    );

    let final_outcome = store
        .project_observation(observations[2].observation_id())
        .await
        .unwrap();
    assert_eq!(final_outcome.checkpoint().last_sequence(), 3);
    let texts = projected_message_texts(&tmp).await;
    assert_eq!(texts.len(), 3);
    assert_eq!(
        texts
            .iter()
            .filter(|text| text.contains("past frontier gamma"))
            .count(),
        1
    );
    let incrementally_projected_texts = texts;
    let incremental_provenance = projection_provenance_rows(&tmp).await;
    let incremental_ownership = projection_ownership_rows(&tmp).await;
    let incremental_output_ids = projection_output_ids(&incremental_provenance);

    let rebuilt_empty = rebuild_projection_to_completion(&store, 0).await;
    assert_eq!(rebuilt_empty.projected_rows(), 0);
    assert_eq!(rebuilt_empty.skipped_observations(), 0);
    assert_eq!(projection_counts(&tmp).await, (1, 0, 0, 1, 0, 3));
    assert_eq!(table_count(&tmp, "lcm_raw_messages").await, 0);
    assert_eq!(table_count(&tmp, "lcm_raw_messages_fts").await, 0);
    assert_eq!(table_count(&tmp, "session_messages_fts").await, 0);

    let rebuilt_full = rebuild_projection_to_completion(&store, 3).await;
    assert_eq!(rebuilt_full.projected_rows(), 3);
    assert_eq!(rebuilt_full.skipped_observations(), 0);
    assert_eq!(projection_counts(&tmp).await, (1, 3, 3, 1, 0, 0));
    assert_eq!(table_count(&tmp, "lcm_raw_messages").await, 3);
    assert_eq!(table_count(&tmp, "lcm_raw_messages_fts").await, 3);
    assert_eq!(table_count(&tmp, "session_messages_fts").await, 3);
    assert_eq!(
        projected_message_texts(&tmp).await,
        incrementally_projected_texts
    );
    assert_eq!(
        projection_provenance_rows(&tmp).await,
        incremental_provenance
    );
    assert_eq!(projection_ownership_rows(&tmp).await, incremental_ownership);
    let rebuilt_provenance = projection_provenance_rows(&tmp).await;
    assert_eq!(
        projection_output_ids(&rebuilt_provenance),
        incremental_output_ids
    );
}

#[tokio::test]
async fn canonical_provider_incremental_and_rebuild_projection_converge() {
    const PROVIDERS: [&str; 7] = [
        "codex", "cursor", "hermes", "kiro", "cline", "roo-code", "kilo",
    ];

    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();

    for (index, provider) in PROVIDERS.into_iter().enumerate() {
        let observation = canonical_observation(provider, index as u64 + 1);
        let outcome = store
            .persist_observation(canonical_write(observation))
            .await
            .unwrap();
        assert!(matches!(outcome, ObservationPersistOutcome::Committed(_)));
    }
    while let Some(observation_id) = store.next_queued_observation().await.unwrap() {
        assert!(matches!(
            store.project_observation(&observation_id).await.unwrap(),
            ProjectionPersistOutcome::Projected(_)
        ));
    }

    let incremental_texts = all_projected_message_texts(&tmp).await;
    assert_eq!(
        table_count(&tmp, "session_temporal_observation_effects").await,
        PROVIDERS.len() as i64,
        "canonical projection and temporal handoff must commit one authority row together"
    );
    assert_eq!(incremental_texts.len(), PROVIDERS.len());
    for provider in PROVIDERS {
        assert!(
            incremental_texts
                .iter()
                .any(|text| text == &format!("{provider} convergence canary")),
            "missing projected {provider} message"
        );
    }
    let incremental_provenance = projection_provenance_rows(&tmp).await;
    let incremental_ownership = projection_ownership_rows(&tmp).await;
    let incremental_output_ids = projection_output_ids(&incremental_provenance);

    let cleared = rebuild_projection_to_completion(&store, 0).await;
    assert_eq!(cleared.projected_rows(), 0);
    let rebuilt = rebuild_projection_to_completion(&store, PROVIDERS.len() as u64).await;
    assert_eq!(rebuilt.projected_rows(), PROVIDERS.len());
    assert_eq!(rebuilt.skipped_observations(), 0);
    assert_eq!(all_projected_message_texts(&tmp).await, incremental_texts);
    assert_eq!(
        table_count(&tmp, "session_temporal_observation_effects").await,
        PROVIDERS.len() as i64,
        "rebuild must preserve the byte-stable temporal authority handoff"
    );
    let rebuilt_provenance = projection_provenance_rows(&tmp).await;
    assert_eq!(rebuilt_provenance, incremental_provenance);
    assert_eq!(projection_ownership_rows(&tmp).await, incremental_ownership);
    assert_eq!(
        projection_output_ids(&rebuilt_provenance),
        incremental_output_ids
    );
}

#[tokio::test]
async fn generation_rollover_coalesces_same_and_changed_native_output() {
    let tmp = TempDir::new().unwrap();
    let first = observation_in_generation(
        "session-generation",
        GENERATION,
        0,
        100,
        "receipt.generation-1",
        conversational_payload("message-generation", "generation original canary"),
    );
    let same_content = observation_in_generation(
        "session-generation",
        GENERATION + 1,
        0,
        100,
        "receipt.generation-2",
        conversational_payload("message-generation", "generation original canary"),
    );
    let replacement = observation_in_generation(
        "session-generation",
        GENERATION + 2,
        0,
        100,
        "receipt.generation-3",
        conversational_payload("message-generation", "generation replacement canary"),
    );

    {
        let runtime = profile_runtime(&tmp).await;
        let store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        persist(&store, first.clone(), None).await;
        store
            .project_observation(first.observation_id())
            .await
            .unwrap();
    }
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    persist(
        &store,
        same_content.clone(),
        Some(cursor_in_generation("session-generation", GENERATION, 100)),
    )
    .await;
    persist(
        &store,
        replacement.clone(),
        Some(cursor_in_generation(
            "session-generation",
            GENERATION + 1,
            100,
        )),
    )
    .await;
    drain_projection_queue(&store).await;

    assert_eq!(projection_counts(&tmp).await, (1, 1, 3, 1, 0, 0));
    let provenance = projection_provenance_rows(&tmp).await;
    assert_eq!(provenance.len(), 3);
    assert!(provenance.iter().all(|row| row.5 == "message-generation"));
    assert_eq!(
        provenance
            .iter()
            .map(|row| row.6.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "generation-owned metadata and replacement content retain distinct lineage digests"
    );
    let texts = projected_message_texts(&tmp).await;
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("generation replacement canary"));

    for candidate in [&first, &same_content, &replacement] {
        assert!(matches!(
            store
                .project_observation(candidate.observation_id())
                .await
                .unwrap(),
            ProjectionPersistOutcome::ExactDuplicate(_)
        ));
    }
    rebuild_projection_to_completion(&store, 0).await;
    assert_eq!(projection_counts(&tmp).await, (1, 0, 0, 1, 0, 3));
    drain_projection_queue(&store).await;
    assert_eq!(projection_counts(&tmp).await, (1, 1, 3, 1, 0, 0));
    let texts = projected_message_texts(&tmp).await;
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("generation replacement canary"));
}

#[tokio::test]
async fn durable_projection_alias_survives_rebuild_without_rewriting_observation() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let candidate = observation(
        "session-alias",
        0,
        100,
        "receipt.alias",
        conversational_payload("message-alias", "durable alias canary"),
    );
    persist(&store, candidate.clone(), None).await;

    let raw_conn = rusqlite::Connection::open(isolated_lcm_db_path(&tmp)).unwrap();
    raw_conn
        .execute(
            "INSERT INTO observation_projection_aliases
                (projector_version, observation_id, output_provider, output_message_id)
             VALUES (?1, ?2, 'claude', 'consolidated/source/message-alias')",
            rusqlite::params![
                CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
                candidate.observation_id().as_str()
            ],
        )
        .unwrap();
    drop(raw_conn);

    drain_projection_queue(&store).await;
    let provenance = projection_provenance_rows(&tmp).await;
    assert_eq!(provenance[0].5, "consolidated/source/message-alias");
    let anchor_id = provenance[0].2.clone();
    assert_eq!(table_count(&tmp, "observation_projection_aliases").await, 1);
    assert_eq!(
        store
            .get_observation(candidate.observation_id())
            .await
            .unwrap()
            .unwrap()
            .observation()
            .payload()["message"]["id"],
        "message-alias"
    );

    rebuild_projection_to_completion(&store, 0).await;
    assert_eq!(table_count(&tmp, "observation_projection_aliases").await, 1);
    drain_projection_queue(&store).await;
    let provenance = projection_provenance_rows(&tmp).await;
    assert_eq!(provenance[0].2, anchor_id);
    assert_eq!(provenance[0].5, "consolidated/source/message-alias");
    assert_eq!(projected_message_texts(&tmp).await.len(), 1);
}

#[tokio::test]
async fn rebuild_preserves_output_referenced_by_another_projector_version() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let candidate = observation(
        "session-shared-output",
        0,
        100,
        "receipt.shared-output",
        conversational_payload("message-shared-output", "shared output canary"),
    );
    persist(&store, candidate.clone(), None).await;
    drain_projection_queue(&store).await;
    add_other_projector_owner(&tmp, candidate.observation_id()).await;

    rebuild_projection_to_completion(&store, 0).await;
    assert_eq!(table_count(&tmp, "session_messages").await, 1);
    assert_eq!(
        table_count(&tmp, "observation_projection_provenance").await,
        2
    );

    rebuild_projection_to_completion(&store, 1).await;
    assert_eq!(table_count(&tmp, "session_messages").await, 1);
    assert_eq!(
        table_count(&tmp, "observation_projection_provenance").await,
        2
    );
    assert_eq!(projected_message_texts(&tmp).await.len(), 1);
}

#[tokio::test]
async fn cross_projector_owner_blocks_incompatible_generation_rollover() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let original = observation_in_generation(
        "session-global-owner",
        GENERATION,
        0,
        100,
        "receipt.global-owner-original",
        conversational_payload("message-global-owner", "global owner original"),
    );
    persist(&store, original.clone(), None).await;
    drain_projection_queue(&store).await;

    add_other_projector_owner(&tmp, original.observation_id()).await;

    let replacement = observation_in_generation(
        "session-global-owner",
        GENERATION + 1,
        0,
        100,
        "receipt.global-owner-replacement",
        conversational_payload("message-global-owner", "global owner replacement"),
    );
    persist(
        &store,
        replacement.clone(),
        Some(cursor_in_generation(
            "session-global-owner",
            GENERATION,
            100,
        )),
    )
    .await;
    // The cross-projector owner still blocks the rollover from replacing the
    // original output; the incompatible replacement converges as a durable
    // skip instead of wedging the queue.
    match store
        .project_observation(replacement.observation_id())
        .await
        .unwrap()
    {
        ProjectionPersistOutcome::Skipped { reason, .. } => assert_eq!(
            reason,
            tracedecay_store::ProjectionSkipReason::OutputCollision
        ),
        other => panic!("expected a collision skip, got {other:?}"),
    }
    assert!(projected_message_texts(&tmp).await[0].contains("global owner original"));
    assert_eq!(table_count(&tmp, "projection_queue").await, 0);
}

#[tokio::test]
async fn rebuild_freezes_cross_projector_multi_generation_ownership() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let original = observation_in_generation(
        "session-retained-generations",
        GENERATION,
        0,
        100,
        "receipt.retained-generation-original",
        conversational_payload(
            "message-retained-generations",
            "retained generation original",
        ),
    );
    let replacement = observation_in_generation(
        "session-retained-generations",
        GENERATION + 1,
        0,
        100,
        "receipt.retained-generation-replacement",
        conversational_payload(
            "message-retained-generations",
            "retained generation replacement",
        ),
    );
    persist(&store, original.clone(), None).await;
    persist(
        &store,
        replacement.clone(),
        Some(cursor_in_generation(
            "session-retained-generations",
            GENERATION,
            100,
        )),
    )
    .await;
    drain_projection_queue(&store).await;

    add_other_projector_owner(&tmp, replacement.observation_id()).await;

    let rebuilt = rebuild_projection_to_completion(&store, 1).await;
    assert_eq!(rebuilt.projected_rows(), 1);
    assert!(projected_message_texts(&tmp).await[0].contains("retained generation replacement"));
    assert_eq!(
        table_count(&tmp, "observation_projection_provenance").await,
        3
    );
    drain_projection_queue(&store).await;
    assert_eq!(
        store.projection_checkpoint().await.unwrap().last_sequence(),
        2
    );
    assert!(projected_message_texts(&tmp).await[0].contains("retained generation replacement"));
}

#[tokio::test]
async fn projection_owner_cache_refreshes_after_another_registered_store_commits() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let first = observation(
        "session-data-version",
        0,
        100,
        "receipt.data-version-first",
        conversational_payload("message-data-version-first", "data version first"),
    );
    let second = observation(
        "session-data-version",
        100,
        200,
        "receipt.data-version-second",
        conversational_payload("message-data-version-second", "data version second"),
    );
    persist(&store, first.clone(), None).await;
    store
        .project_observation(first.observation_id())
        .await
        .unwrap();
    persist(
        &store,
        second.clone(),
        Some(cursor("session-data-version", 100)),
    )
    .await;

    let other_store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    other_store
        .project_observation(second.observation_id())
        .await
        .unwrap();
    assert!(matches!(
        store
            .project_observation(second.observation_id())
            .await
            .unwrap(),
        ProjectionPersistOutcome::ExactDuplicate(_)
    ));
}

#[tokio::test]
async fn rebuild_processes_more_than_two_pages_at_one_frozen_frontier() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let mut expected_cursor = None;
    for index in 0..257_u64 {
        let start = index * 100;
        let end = start + 100;
        let candidate = observation(
            "session-paged-rebuild",
            start,
            end,
            &format!("receipt.paged-rebuild-{index}"),
            conversational_payload(
                &format!("message-paged-rebuild-{index}"),
                &format!("paged rebuild canary {index}"),
            ),
        );
        persist(&store, candidate, expected_cursor.clone()).await;
        expected_cursor = Some(cursor("session-paged-rebuild", end));
    }

    drain_projection_queue(&store).await;
    let visible_before = projection_counts(&tmp).await;
    let pending = store.rebuild_projection(257).await.unwrap();
    assert!(!pending.is_complete());
    assert_eq!(projection_counts(&tmp).await, visible_before);

    let rebuilt = rebuild_projection_to_completion(&store, 257).await;
    assert_eq!(rebuilt.projected_rows(), 257);
    assert_eq!(rebuilt.skipped_observations(), 0);
    assert_eq!(projection_counts(&tmp).await, (1, 257, 257, 1, 0, 0));
}

#[tokio::test]
async fn interrupted_rebuild_resumes_same_generation_with_pinned_aliases() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let mut expected_cursor = None;
    let mut aliased_observation = None;
    for index in 0..257_u64 {
        let start = index * 100;
        let end = start + 100;
        let candidate = observation(
            "session-interrupted-rebuild",
            start,
            end,
            &format!("receipt.interrupted-rebuild-{index}"),
            conversational_payload(
                &format!("message-interrupted-rebuild-{index}"),
                &format!("interrupted rebuild canary {index}"),
            ),
        );
        if index == 200 {
            aliased_observation = Some(candidate.observation_id().clone());
        }
        persist(&store, candidate, expected_cursor.clone()).await;
        expected_cursor = Some(cursor("session-interrupted-rebuild", end));
    }
    let aliased_observation = aliased_observation.unwrap();
    let raw_conn = rusqlite::Connection::open(isolated_lcm_db_path(&tmp)).unwrap();
    raw_conn
        .execute(
            "INSERT INTO observation_projection_aliases (
                projector_version, observation_id, output_provider, output_message_id
             ) VALUES (
                ?1, ?2, 'claude',
                'consolidated/pinned/message-interrupted-rebuild-200'
             )",
            rusqlite::params![
                CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
                aliased_observation.as_str(),
            ],
        )
        .unwrap();
    rebuild_projection_to_completion(&store, 257).await;

    raw_conn
        .execute_batch(
            "CREATE TRIGGER interrupt_projection_rebuild_second_page
             BEFORE UPDATE OF staged_through ON observation_projection_rebuilds
             WHEN OLD.staged_through >= 128 BEGIN
                SELECT RAISE(ABORT, 'injected rebuild page interruption');
             END;",
        )
        .unwrap();
    let pending = store.rebuild_projection(257).await.unwrap();
    assert!(!pending.is_complete());
    let error = store
        .rebuild_projection(257)
        .await
        .expect_err("the second staged page must fail");
    assert!(matches!(error, ProjectionStoreError::Storage { .. }));
    raw_conn
        .execute(
            "UPDATE observation_projection_aliases
             SET output_message_id = 'consolidated/transient/message-interrupted-rebuild-200'
             WHERE projector_version = ?1 AND observation_id = ?2",
            rusqlite::params![
                CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
                aliased_observation.as_str(),
            ],
        )
        .unwrap();
    let pinned_alias = raw_conn
        .query_row(
            "SELECT output_message_id
             FROM observation_projection_rebuild_aliases
             WHERE projector_version = ?1 AND observation_id = ?2",
            rusqlite::params![
                CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
                aliased_observation.as_str(),
            ],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    assert_eq!(
        pinned_alias,
        "consolidated/pinned/message-interrupted-rebuild-200"
    );
    raw_conn
        .execute(
            "UPDATE observation_projection_aliases
             SET output_message_id = 'consolidated/pinned/message-interrupted-rebuild-200'
             WHERE projector_version = ?1 AND observation_id = ?2",
            rusqlite::params![
                CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
                aliased_observation.as_str(),
            ],
        )
        .unwrap();
    let past_frontier = observation(
        "session-interrupted-rebuild",
        25_700,
        25_800,
        "receipt.interrupted-rebuild-past-frontier",
        conversational_payload(
            "message-interrupted-rebuild-past-frontier",
            "ingest committed while rebuild generation was staged",
        ),
    );
    persist(&store, past_frontier.clone(), expected_cursor).await;
    let (generation, staged_through, state) = raw_conn
        .query_row(
            "SELECT generation, staged_through, state
             FROM observation_projection_rebuilds
             WHERE projector_version = ?1",
            rusqlite::params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(staged_through, 128);
    assert_eq!(state, "building");
    assert_eq!(projection_counts(&tmp).await, (1, 257, 257, 1, 0, 1));
    raw_conn
        .execute_batch(
            "DROP TRIGGER interrupt_projection_rebuild_second_page;
             CREATE TRIGGER interrupt_projection_rebuild_activation
             BEFORE INSERT ON observation_projection_checkpoints BEGIN
                SELECT RAISE(ABORT, 'injected rebuild activation interruption');
             END;",
        )
        .unwrap();
    let error = store
        .rebuild_projection(257)
        .await
        .expect_err("activation interruption must preserve the ready generation");
    assert!(matches!(error, ProjectionStoreError::Storage { .. }));
    assert_eq!(projection_counts(&tmp).await, (1, 257, 257, 1, 0, 1));
    drop(raw_conn);
    drop(runtime);
    checkpoint_database(&tmp).await;
    let raw_conn = rusqlite::Connection::open(isolated_lcm_db_path(&tmp)).unwrap();
    raw_conn
        .execute_batch(
            "DROP TRIGGER interrupt_projection_rebuild_activation;
             CREATE TRIGGER reject_projection_rebuild_replacement
             BEFORE DELETE ON observation_projection_rebuilds
             WHEN OLD.state <> 'ready' BEGIN
                SELECT RAISE(ABORT, 'identical rebuild job was replaced');
             END;",
        )
        .unwrap();
    drop(raw_conn);

    let reopened_runtime = profile_runtime(&tmp).await;
    let reopened_store = reopened_runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let activation_started = std::time::Instant::now();
    let rebuilt = rebuild_projection_to_completion(&reopened_store, 257).await;
    assert!(
        activation_started.elapsed() < std::time::Duration::from_secs(5),
        "bounded activation of 257 outputs exceeded the test budget"
    );
    assert_eq!(rebuilt.projected_rows(), 257);
    assert_eq!(
        reopened_store
            .projection_checkpoint()
            .await
            .unwrap()
            .last_sequence(),
        257
    );
    assert_eq!(
        reopened_store
            .next_queued_observation()
            .await
            .unwrap()
            .as_ref(),
        Some(past_frontier.observation_id())
    );
    reopened_store
        .project_observation(past_frontier.observation_id())
        .await
        .unwrap();
    assert_eq!(
        reopened_store
            .projection_checkpoint()
            .await
            .unwrap()
            .last_sequence(),
        258
    );
    assert_eq!(
        table_count(&tmp, "observation_projection_rebuilds").await,
        0
    );
    let output_ids = projection_output_ids(&projection_provenance_rows(&tmp).await);
    assert!(output_ids.contains(&"consolidated/pinned/message-interrupted-rebuild-200".to_owned()));
    assert!(!generation.is_empty());
}

#[tokio::test]
async fn high_generation_output_uses_constant_size_owner_state() {
    let tmp = TempDir::new().unwrap();
    let runtime = profile_runtime(&tmp).await;
    let store = runtime
        .observation_store(HostAdmissionScope::Profile)
        .unwrap();
    let mut expected_cursor = None;
    for generation_offset in 0..257_u64 {
        let generation = GENERATION + generation_offset;
        let candidate = observation_in_generation(
            "session-high-generation",
            generation,
            0,
            100,
            &format!("receipt.high-generation-{generation}"),
            conversational_payload(
                "message-high-generation",
                &format!("high generation canary {generation}"),
            ),
        );
        persist(&store, candidate, expected_cursor.clone()).await;
        expected_cursor = Some(cursor_in_generation(
            "session-high-generation",
            generation,
            100,
        ));
    }
    drain_projection_queue(&store).await;

    assert_eq!(projection_counts(&tmp).await, (1, 1, 257, 1, 0, 0));
    assert!(projected_message_texts(&tmp).await[0].contains("high generation canary 267"));
}

// These projection-contract cases inject provider-tagged durable/canonical records directly.
// They do not exercise provider transcript parsers or JSONL framing.
#[tokio::test]
async fn cross_provider_projection_duplicate_reorder_conflict_and_restart_are_idempotent() {
    const PROVIDERS: [&str; 8] = [
        "claude", "codex", "cursor", "hermes", "kiro", "cline", "roo-code", "kilo",
    ];

    for provider in PROVIDERS {
        let tmp = TempDir::new().unwrap();
        let runtime = profile_runtime(&tmp).await;
        let store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();

        let (first, second, third, conflicting) = if provider == "claude" {
            let session = format!("session.cross-projection-{provider}");
            (
                observation(
                    &session,
                    0,
                    100,
                    &format!("receipt.cross-proj.{provider}.1"),
                    conversational_payload(
                        &format!("message-{provider}-1"),
                        &format!("{provider} frontier alpha"),
                    ),
                ),
                observation(
                    &session,
                    100,
                    200,
                    &format!("receipt.cross-proj.{provider}.2"),
                    conversational_payload(
                        &format!("message-{provider}-2"),
                        &format!("{provider} frontier beta"),
                    ),
                ),
                observation(
                    &session,
                    200,
                    300,
                    &format!("receipt.cross-proj.{provider}.3"),
                    conversational_payload(
                        &format!("message-{provider}-3"),
                        &format!("{provider} frontier gamma"),
                    ),
                ),
                observation(
                    &session,
                    0,
                    100,
                    &format!("receipt.cross-proj.{provider}.conflict"),
                    conversational_payload(
                        &format!("message-{provider}-conflict"),
                        &format!("{provider} conflicting payload"),
                    ),
                ),
            )
        } else {
            let first =
                canonical_observation_at(provider, 1, 0, 1, &format!("{provider} frontier alpha"));
            let second =
                canonical_observation_at(provider, 2, 1, 2, &format!("{provider} frontier beta"));
            let third =
                canonical_observation_at(provider, 3, 2, 3, &format!("{provider} frontier gamma"));
            let conflict_payload = {
                let provider_id = ProviderId::new(provider).unwrap();
                let session_id = SessionId::new(format!("session.projection-{provider}")).unwrap();
                let range = ObservationSourceRangeV1::new(0, 1).unwrap();
                let record_id =
                    ObservationId::new(format!("record.projection-{provider}.1")).unwrap();
                let envelope = CanonicalObservationEnvelopeV1::new(
                    provider_id,
                    "message",
                    record_id,
                    CanonicalObservationRelationsV1::new(session_id),
                    vec![CanonicalObservationFactV1::Message {
                        role: CanonicalMessageRoleV1::Assistant,
                        content: json!({"text": format!("{provider} conflicting payload")}),
                        model: Some("model.fixture".to_owned()),
                        timestamp: Some(1_750_000_000),
                    }],
                    CanonicalObservationEvidenceV1::new(
                        ObservationOrderingDomainV1::SnapshotOrder,
                        range,
                    ),
                )
                .unwrap();
                serde_json::to_value(envelope).unwrap()
            };
            let conflicting = DurableObservationV1::new(
                first.identity().clone(),
                receipt(
                    &format!("receipt.projection-{provider}.conflict"),
                    &conflict_payload,
                ),
                RetentionClass::new("retention.projection-test").unwrap(),
                conflict_payload,
            )
            .unwrap();
            (first, second, third, conflicting)
        };

        let mut expected = None;
        for candidate in [&first, &second, &third] {
            let write = if provider == "claude" {
                write((*candidate).clone(), expected.clone())
            } else {
                canonical_write_with_cursor((*candidate).clone(), expected.clone())
            };
            let outcome = store.persist_observation(write).await.unwrap();
            let committed = match outcome {
                ObservationPersistOutcome::Committed(receipt) => receipt,
                other => panic!("{provider}: persist must commit, got {other:?}"),
            };
            expected = Some(committed.committed_cursor().clone());
        }

        let conflict_write = if provider == "claude" {
            write(conflicting, None)
        } else {
            canonical_write_with_cursor(conflicting, None)
        };
        let conflict = store
            .persist_observation(conflict_write)
            .await
            .expect_err("{provider}: conflicting identity must fail");
        assert!(
            matches!(
                conflict,
                tracedecay_store::ObservationStoreError::ObservationCollision { .. }
            ),
            "{provider}: expected ObservationCollision, got {conflict:?}"
        );

        let reorder_error = store
            .project_observation(second.observation_id())
            .await
            .expect_err("{provider}: out-of-order projection must gap");
        assert!(
            matches!(
                reorder_error,
                ProjectionStoreError::Gap {
                    expected: 1,
                    actual: 2
                }
            ),
            "{provider}: expected Gap{{1,2}}, got {reorder_error:?}"
        );
        assert_eq!(
            store.projection_checkpoint().await.unwrap().last_sequence(),
            0,
            "{provider}"
        );

        assert!(matches!(
            store
                .project_observation(first.observation_id())
                .await
                .unwrap(),
            ProjectionPersistOutcome::Projected(_)
        ));
        let duplicate_project = store
            .project_observation(first.observation_id())
            .await
            .unwrap();
        assert!(
            matches!(
                duplicate_project,
                ProjectionPersistOutcome::ExactDuplicate(_)
            ),
            "{provider}: reproject must be ExactDuplicate, got {duplicate_project:?}"
        );
        assert!(matches!(
            store
                .project_observation(second.observation_id())
                .await
                .unwrap(),
            ProjectionPersistOutcome::Projected(_)
        ));
        assert!(matches!(
            store
                .project_observation(third.observation_id())
                .await
                .unwrap(),
            ProjectionPersistOutcome::Projected(_)
        ));

        let texts_before = all_projected_message_texts(&tmp).await;
        assert!(
            texts_before
                .iter()
                .any(|text| text.contains(&format!("{provider} frontier alpha"))),
            "{provider}"
        );
        assert!(
            texts_before
                .iter()
                .any(|text| text.contains(&format!("{provider} frontier gamma"))),
            "{provider}"
        );
        let provenance_before = projection_provenance_rows(&tmp).await;
        let counts_before = projection_counts(&tmp).await;
        drop(runtime);

        let runtime = profile_runtime(&tmp).await;
        let store = runtime
            .observation_store(HostAdmissionScope::Profile)
            .unwrap();
        let restarted = store
            .project_observation(first.observation_id())
            .await
            .unwrap();
        assert!(
            matches!(restarted, ProjectionPersistOutcome::ExactDuplicate(_)),
            "{provider}: restart reproject must be ExactDuplicate, got {restarted:?}"
        );
        assert_eq!(
            all_projected_message_texts(&tmp).await,
            texts_before,
            "{provider}"
        );
        assert_eq!(
            projection_provenance_rows(&tmp).await,
            provenance_before,
            "{provider}"
        );
        assert_eq!(projection_counts(&tmp).await, counts_before, "{provider}");

        let rebuilt = rebuild_projection_to_completion(&store, 3).await;
        assert_eq!(rebuilt.projected_rows(), 3, "{provider}");
        assert_eq!(rebuilt.skipped_observations(), 0, "{provider}");
        assert_eq!(
            all_projected_message_texts(&tmp).await,
            texts_before,
            "{provider}"
        );
        assert_eq!(
            projection_provenance_rows(&tmp).await,
            provenance_before,
            "{provider}"
        );
        assert_eq!(projection_counts(&tmp).await, counts_before, "{provider}");
    }
}

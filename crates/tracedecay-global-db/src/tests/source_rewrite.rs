//! Regression tests for registered source-rewrite completion.
//!
//! Rewrite completion retires every record the rewritten source no longer
//! offers. One long host session can hold more unoffered records than the
//! exact-SQL engine's per-query materialization cap, so completion must read
//! them in bounded writer pages. Each committed page must persist enough
//! progress that a restart does not repeat its already-retired records.

use tempfile::TempDir;

use tracedecay_domain::{
    ObservationScopeV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1, ProviderId,
    SessionId,
};
use tracedecay_runtime_core::db::engine::{QueryExecutor, params};
use tracedecay_store::{ProjectionSkipReason, ProjectionStoreError};

use crate::ObservationSourcePresenceV1;
use crate::tests::harness::{
    HostAdmissionScope, HostAdmissionTestRuntimeV1, RegisteredGlobalDbHarness,
    seed_projected_messages, writer_telemetry,
};

/// More unoffered records than one keyset page *and* more than the engine's
/// per-query materialization cap, so the test fails against the unbounded
/// read and exercises repeated, independently committed writer pages.
const UNOFFERED_RECORDS: i64 = 10_003;

#[asupersync::test]
async fn rewrite_completion_retires_unoffered_records_beyond_the_query_cap() {
    let harness = RegisteredGlobalDbHarness::open("source-rewrite-unoffered-cap").await;
    let source =
        ObservationSourceIdentityV1::new(SessionId::new("session.rewrite-cap").unwrap()).unwrap();
    let scope = ObservationScopeV1::Profile;
    let previous = ObservationSourceGenerationV1::new(1).unwrap();
    let generation = ObservationSourceGenerationV1::new(2).unwrap();

    let key = serde_json::to_string(&(source.clone(), scope.clone())).unwrap();
    let writer = harness.registered.writer_connection().unwrap();
    // Keep the regression's 10,003 ghost records without turning fixture
    // preparation into 10,003 serial writer round-trips.
    for start in (0..UNOFFERED_RECORDS).step_by(128) {
        let end = (start + 128).min(UNOFFERED_RECORDS);
        writer
            .execute(
                "WITH RECURSIVE generated(i) AS (
                     SELECT ?2
                     UNION ALL
                     SELECT i + 1 FROM generated WHERE i + 1 < ?3
                 )
                 INSERT INTO observation_source_presence (
                     source_key, observation_id, generation, start_offset
                 )
                 SELECT ?1, printf('sha256:unoffered-%06d', i), ?4, 0
                 FROM generated",
                params![
                    key.as_str(),
                    start,
                    end,
                    previous.generation_id().to_string()
                ],
            )
            .await
            .unwrap();
    }
    assert_eq!(
        table_count(&writer, "SELECT COUNT(*) FROM observation_source_presence").await,
        UNOFFERED_RECORDS,
        "fixture must cover more rows than the query materialization cap"
    );

    harness
        .registered
        .begin_observation_source_rewrite(&source, &scope, previous, generation, 0)
        .await
        .unwrap();

    // These are presence-only ghosts: no observations, dispositions, or
    // queued revivals exist to retire. The final settle must still delete
    // all presence rows, without issuing per-ghost UPDATE/receipt queries
    // while holding the SQLite writer's fixed transaction lease.
    let before = writer_telemetry(&harness.registered).sqlite_vm.statements;
    // Each rewrite page is one exact-SQL writer job. Its individual SQL
    // statements stay inside that job. The committed_transactions counter
    // belongs to batch execution, not exact-SQL jobs.
    let before_commands = writer_telemetry(&harness.registered).transactions.commands;
    let retired = harness
        .registered
        .complete_observation_source_rewrite(&source, &scope, generation)
        .await
        .unwrap_or_else(|error| panic!("rewrite completion must not exceed query caps: {error}"));
    assert_eq!(retired, 0);
    let statements = writer_telemetry(&harness.registered).sqlite_vm.statements - before;
    let after = writer_telemetry(&harness.registered);
    let writer_jobs = after.transactions.commands - before_commands;
    assert_eq!(
        writer_jobs,
        (UNOFFERED_RECORDS as u64).div_ceil(128),
        "each bounded page must use exactly one writer job"
    );
    assert!(
        statements.saturating_mul(16) < UNOFFERED_RECORDS as u64,
        "presence-only ghosts must not cause per-record writer commands: {statements} statements for {UNOFFERED_RECORDS} records"
    );

    let mut remaining = writer
        .query(
            "SELECT COUNT(*) FROM observation_source_presence WHERE source_key = ?1",
            params![key.as_str()],
        )
        .await
        .unwrap();
    let count = remaining.next().await.unwrap().unwrap();
    let presence: i64 = count.get(0).unwrap();
    assert_eq!(presence, 0, "unoffered presence rows must be settled");

    let mut pending = writer
        .query(
            "SELECT COUNT(*) FROM observation_source_rewrites WHERE source_key = ?1",
            params![key.as_str()],
        )
        .await
        .unwrap();
    let row = pending.next().await.unwrap().unwrap();
    let rewrites: i64 = row.get(0).unwrap();
    assert_eq!(rewrites, 0, "completed rewrite row must be removed");
}

/// An exact multiple of the bounded page must retire its final records and
/// clear the pending marker in the same productive writer transaction. A
/// lookahead row may be read, but must never be retired in the previous page.
#[asupersync::test]
async fn rewrite_completion_exact_page_boundaries_need_no_empty_writer() {
    for (unoffered, fixture) in [
        (128_i64, "source-rewrite-exact-128"),
        (256, "source-rewrite-exact-256"),
        (257, "source-rewrite-after-exact-256"),
    ] {
        let harness = RegisteredGlobalDbHarness::open(fixture).await;
        let source =
            ObservationSourceIdentityV1::new(SessionId::new("session.exact-pages").unwrap())
                .unwrap();
        let scope = ObservationScopeV1::Profile;
        let previous = ObservationSourceGenerationV1::new(1).unwrap();
        let generation = ObservationSourceGenerationV1::new(2).unwrap();
        let key = serde_json::to_string(&(source.clone(), scope.clone())).unwrap();
        let writer = harness.registered.writer_connection().unwrap();

        writer
            .execute(
                "INSERT INTO observation_source_presence
                 (source_key, observation_id, generation, start_offset)
                 VALUES (?1, 'sha256:current', ?2, 0)",
                params![key.as_str(), generation.generation_id().to_string()],
            )
            .await
            .unwrap();
        for start in (0..unoffered).step_by(128) {
            let end = (start + 128).min(unoffered);
            assert_eq!(
                writer
                    .execute(
                        "WITH RECURSIVE generated(i) AS (
                             SELECT ?2
                             UNION ALL
                             SELECT i + 1 FROM generated WHERE i + 1 < ?3
                         )
                         INSERT INTO observation_source_presence
                         (source_key, observation_id, generation, start_offset)
                         SELECT ?1, printf('sha256:obsolete-%06d', i), ?4, 0
                         FROM generated",
                        params![
                            key.as_str(),
                            start,
                            end,
                            previous.generation_id().to_string(),
                        ],
                    )
                    .await
                    .unwrap(),
                u64::try_from(end - start).unwrap()
            );
        }

        harness
            .registered
            .begin_observation_source_rewrite(&source, &scope, previous, generation, 0)
            .await
            .unwrap();
        let before = writer_telemetry(&harness.registered).transactions.commands;
        assert_eq!(
            harness
                .registered
                .complete_observation_source_rewrite(&source, &scope, generation)
                .await
                .unwrap(),
            0,
            "presence-only ghosts must not count as projected retirements"
        );
        let writer_jobs = writer_telemetry(&harness.registered).transactions.commands - before;
        assert_eq!(
            writer_jobs,
            u64::try_from(unoffered).unwrap().div_ceil(128),
            "{fixture}: no empty writer transaction after an exact full page"
        );
        assert_eq!(
            table_count(&writer, "SELECT COUNT(*) FROM observation_source_rewrites").await,
            0,
            "the durable pending rewrite marker must be cleared"
        );
        assert_eq!(
            table_count(&writer, "SELECT COUNT(*) FROM observation_source_presence").await,
            1,
            "every obsolete identity must be retired"
        );
        assert_eq!(
            table_count(
                &writer,
                "SELECT COUNT(*) FROM observation_source_presence
                 WHERE observation_id = 'sha256:current' AND generation = '2'"
            )
            .await,
            1,
            "the current-generation survivor must remain"
        );
    }
}

async fn table_count(conn: &impl QueryExecutor, sql: &str) -> i64 {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let row = rows.next().await.unwrap().unwrap();
    row.get(0).unwrap()
}

/// One source page can retire many effects of the same session. The
/// temporal reset is a session-level invalidation, not a per-record writer
/// upsert; an unchanged second update must be impossible in this page.
#[asupersync::test]
async fn rewrite_completion_coalesces_temporal_reset_for_one_session() {
    let directory = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
        .await
        .unwrap();
    let seeded = seed_projected_messages(&runtime, 0..4).await;
    let registered = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let source = seeded[0].0.source().clone();
    let scope = ObservationScopeV1::Profile;
    let previous = ObservationSourceGenerationV1::new(1).unwrap();
    let generation = ObservationSourceGenerationV1::new(2).unwrap();
    let writer = registered.writer_connection().unwrap();

    assert_eq!(
        table_count(
            &writer,
            "SELECT COUNT(*) FROM session_temporal_observation_effects WHERE output_count > 0"
        )
        .await,
        4,
        "all projected observations must own materialized temporal effects"
    );
    assert_eq!(
        table_count(&writer, "SELECT COUNT(*) FROM session_temporal_resets").await,
        0,
        "the fixture must not already have pending reset requests"
    );

    let offered: Vec<_> = seeded
        .iter()
        .map(|(observation, _)| ObservationSourcePresenceV1 {
            observation_id: observation.observation_id().clone(),
            generation: previous,
            start_offset: observation.identity().position().start(),
        })
        .collect();
    registered
        .record_observation_source_presence(&source, &scope, &offered)
        .await
        .unwrap();
    registered
        .begin_observation_source_rewrite(&source, &scope, previous, generation, 0)
        .await
        .unwrap();

    // The previous per-observation upsert fails on its second update. The
    // replacement must coalesce same-session effects but still retire all four.
    writer
        .execute(
            "CREATE TRIGGER reject_redundant_temporal_reset
             BEFORE UPDATE ON session_temporal_resets
             BEGIN SELECT RAISE(ABORT, 'duplicate temporal reset'); END",
            (),
        )
        .await
        .unwrap();

    assert_eq!(
        registered
            .complete_observation_source_rewrite(&source, &scope, generation)
            .await
            .unwrap(),
        4
    );
    assert_eq!(
        table_count(&writer, "SELECT COUNT(*) FROM session_temporal_resets").await,
        1,
        "one session must receive exactly one durable reset request"
    );
    assert_eq!(
        table_count(
            &writer,
            "SELECT COUNT(*) FROM observation_projection_dispositions
             WHERE reason = 'source_record_retired'"
        )
        .await,
        4,
        "coalescing must not skip retirement of any projected record"
    );
    assert_eq!(
        table_count(&writer, "SELECT COUNT(*) FROM observation_source_presence").await,
        0,
        "all retired presence rows must be settled"
    );
    assert_eq!(
        table_count(&writer, "SELECT COUNT(*) FROM observation_source_rewrites").await,
        0,
        "the rewrite marker must be committed with the reset"
    );
}

/// Retiring an unoffered record also removes its projections and workflow
/// facts. A collision on a later page must roll that page back, but earlier
/// page commits must remain durable for the next daemon invocation.
/// Reopening the registered store must resume from those committed pages,
/// preserving the reoffered survivor and retiring each obsolete record once.
#[asupersync::test]
async fn rewrite_completion_resumes_committed_pages_after_failure_and_restart() {
    // More real projected records than one page: prior page commits must
    // remain durable when the final page's provenance collision aborts.
    const RECORDS: usize = 1_005;

    let directory = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
        .await
        .unwrap();
    let registered = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    // Seed the full 1,005-observation production journey in bounded
    // admission/projection windows. A single 1,005-item projection writer
    // transaction in fixture setup obscures the restart behavior under test
    // and can monopolize the writer before the rewrite even starts.
    let mut seeded = Vec::with_capacity(RECORDS);
    for start in (0..RECORDS).step_by(128) {
        let end = (start + 128).min(RECORDS);
        seeded.extend(seed_projected_messages(&runtime, start..end).await);
    }
    assert_eq!(
        seeded.len(),
        RECORDS,
        "all source records must be projected"
    );

    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new("codex").unwrap(),
        SessionId::new("session.audit-batch").unwrap(),
    )
    .unwrap();
    let scope = ObservationScopeV1::Profile;
    let previous = ObservationSourceGenerationV1::new(1).unwrap();
    let generation = ObservationSourceGenerationV1::new(2).unwrap();

    let offered: Vec<ObservationSourcePresenceV1> = seeded
        .iter()
        .map(|(observation, _)| ObservationSourcePresenceV1 {
            observation_id: observation.observation_id().clone(),
            generation: previous,
            start_offset: observation.identity().position().start(),
        })
        .collect();
    registered
        .record_observation_source_presence(&source, &scope, &offered)
        .await
        .unwrap();
    registered
        .begin_observation_source_rewrite(&source, &scope, previous, generation, 0)
        .await
        .unwrap();

    // The rewrite re-offers the first record under the new generation; its
    // presence, projection, and outputs must survive completion untouched.
    let survivor = &seeded[0].0;
    registered
        .record_observation_source_presence(
            &source,
            &scope,
            &[ObservationSourcePresenceV1 {
                observation_id: survivor.observation_id().clone(),
                generation,
                start_offset: survivor.identity().position().start(),
            }],
        )
        .await
        .unwrap();

    // The unoffered record the keyset order retires last, plus the output
    // its projection owns.
    let collided_index = seeded
        .iter()
        .enumerate()
        .skip(1)
        .max_by(|left, right| {
            left.1
                .0
                .observation_id()
                .as_str()
                .cmp(right.1.0.observation_id().as_str())
        })
        .map(|(index, _)| index)
        .unwrap();
    let collided_message_id = format!("message.record.audit-batch-{collided_index}");

    // Plant a provenance row making the re-offered record co-own the last
    // record's output. When the last record retires, the surviving owner is
    // read back and its stored rendering derives a different message, which
    // the retire path refuses with `ProvenanceCollision`.
    let already_retired = {
        let writer = registered.writer_connection().unwrap();
        writer
            .execute(
                "INSERT INTO observation_projection_provenance (
                    projector_version, observation_id, output_ordinal, receipt_id,
                    output_provider, output_message_id, output_digest, message_created,
                    retrieval_anchor_id
                 )
                 SELECT projector_version, observation_id, 1, receipt_id,
                        output_provider, ?1, output_digest, message_created,
                        retrieval_anchor_id
                 FROM observation_projection_provenance
                 WHERE observation_id = ?2",
                params![
                    collided_message_id.as_str(),
                    survivor.observation_id().as_str()
                ],
            )
            .await
            .unwrap();

        let before_commands = writer_telemetry(registered).transactions.commands;
        let error = registered
            .complete_observation_source_rewrite(&source, &scope, generation)
            .await
            .unwrap_err();
        let writer_jobs = writer_telemetry(registered).transactions.commands - before_commands;
        assert!(
            matches!(error, ProjectionStoreError::ProvenanceCollision),
            "colliding output ownership must fail completion, got {error:?}"
        );

        // Earlier committed pages survive the failure; only the page containing
        // the collision rolls back. This is the progress a new process must reuse.
        let already_retired = table_count(
            &writer,
            "SELECT COUNT(*) FROM observation_projection_dispositions",
        )
        .await;
        assert!(
            already_retired >= 128 && already_retired < (RECORDS - 1) as i64,
            "failure must preserve completed pages, but not the colliding page"
        );
        assert_eq!(
            writer_jobs,
            u64::try_from(already_retired).unwrap().div_ceil(128) + 1,
            "each durable page and the final rolled-back page need exactly one writer job"
        );
        assert_eq!(
            table_count(
                &writer,
                "SELECT COUNT(*) FROM observation_projection_provenance",
            )
            .await,
            i64::try_from(RECORDS).unwrap() + 1 - already_retired,
            "committed pages must remove their provenance"
        );
        assert_eq!(
            table_count(&writer, "SELECT COUNT(*) FROM lcm_raw_messages").await,
            i64::try_from(RECORDS).unwrap() - already_retired,
            "committed pages must remove only their projected outputs"
        );
        assert_eq!(
            table_count(&writer, "SELECT COUNT(*) FROM observation_source_rewrites").await,
            1,
            "failed page must leave the rewrite pending for restart"
        );

        // Repair the injected corruption before reopening: admission audits
        // the invalid survivor provenance and refuses the store by contract.
        writer
            .execute(
                "DELETE FROM observation_projection_provenance
                 WHERE observation_id = ?1 AND output_ordinal = 1",
                params![survivor.observation_id().as_str()],
            )
            .await
            .unwrap();
        already_retired
    };

    // Reopen the registered profile store as a new daemon fixture. Durable
    // rewrite state, dispositions and the source-presence page frontier must
    // survive rather than replay the already committed retirement work.
    drop(runtime);
    let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
        .await
        .unwrap();
    let registered = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("reopened registered profile database");
    let writer = registered.writer_connection().unwrap();
    assert_eq!(
        table_count(
            &writer,
            "SELECT COUNT(*) FROM observation_projection_dispositions",
        )
        .await,
        already_retired,
        "restart must retain the completed retirement frontier"
    );

    let retired = registered
        .complete_observation_source_rewrite(&source, &scope, generation)
        .await
        .unwrap_or_else(|error| {
            panic!("retry after clearing the collision must converge: {error}")
        });
    assert_eq!(
        retired,
        u64::try_from(RECORDS - 1).unwrap() - already_retired as u64,
        "restart must retire only records that were not already settled"
    );

    let retired_reason = ProjectionSkipReason::SourceRecordRetired.as_str();
    assert_eq!(
        table_count(
            &writer,
            &format!(
                "SELECT COUNT(*) FROM observation_projection_dispositions
                 WHERE reason = '{retired_reason}'"
            )
        )
        .await,
        i64::try_from(RECORDS - 1).unwrap(),
        "every unoffered record must take a source_record_retired disposition"
    );
    assert_eq!(
        table_count(
            &writer,
            "SELECT COUNT(*) FROM observation_projection_provenance"
        )
        .await,
        1,
        "only the re-offered record's provenance survives"
    );
    assert_eq!(
        table_count(&writer, "SELECT COUNT(*) FROM lcm_raw_messages").await,
        1,
        "only the re-offered record's projected output survives"
    );
    assert_eq!(
        table_count(&writer, "SELECT COUNT(*) FROM observation_source_presence").await,
        1,
        "only the re-offered record's presence row survives"
    );
    assert_eq!(
        table_count(&writer, "SELECT COUNT(*) FROM observation_source_rewrites").await,
        0,
        "completed rewrite row must be removed"
    );
    assert_eq!(
        table_count(
            &writer,
            "SELECT COUNT(*) FROM session_temporal_resets
             WHERE session_id = 'session.audit-batch'"
        )
        .await,
        1,
        "retired records must request their session's temporal rebuild"
    );
}

/// Beginning the same in-progress layout again must not rewrite retained
/// records, accepted revivals, or the unchanged pending rewrite marker.
/// SQL triggers reject every redundant UPDATE, while a real generation
/// transition must still advance all three authorities.
#[asupersync::test]
async fn repeated_source_rewrite_begin_preserves_already_advanced_prefix() {
    let directory = TempDir::new().unwrap();
    let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
        .await
        .unwrap();
    let registered = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let seeded = seed_projected_messages(&runtime, 0..2).await;
    let source =
        ObservationSourceIdentityV1::new(SessionId::new("session.repeat-prefix").unwrap()).unwrap();
    let scope = ObservationScopeV1::Profile;
    let previous = ObservationSourceGenerationV1::new(1).unwrap();
    let generation = ObservationSourceGenerationV1::new(2).unwrap();
    let next = ObservationSourceGenerationV1::new(3).unwrap();
    let offered = seeded
        .iter()
        .map(|(observation, _)| ObservationSourcePresenceV1 {
            observation_id: observation.observation_id().clone(),
            generation: previous,
            start_offset: observation.identity().position().start(),
        })
        .collect::<Vec<_>>();
    registered
        .record_observation_source_presence(&source, &scope, &offered)
        .await
        .unwrap();
    let writer = registered.writer_connection().unwrap();
    writer
        .execute(
            "INSERT INTO projection_queue (
                 observation_id, observation_sequence, revival_generation, revival_start_offset
             )
             SELECT observation_id, sequence, ?2, ?3
             FROM observations WHERE observation_id = ?1
             ON CONFLICT(observation_id) DO UPDATE SET
                 revival_generation = excluded.revival_generation,
                 revival_start_offset = excluded.revival_start_offset",
            params![
                offered[0].observation_id.as_str(),
                previous.generation_id().to_string(),
                i64::try_from(offered[0].start_offset).unwrap()
            ],
        )
        .await
        .unwrap();

    let limit = i64::MAX as u64;
    registered
        .begin_observation_source_rewrite(&source, &scope, previous, generation, limit)
        .await
        .unwrap();
    assert_eq!(
        table_count(
            &writer,
            "SELECT COUNT(*) FROM observation_source_presence WHERE generation = '2'"
        )
        .await,
        2,
        "the first rewrite must advance both retained presence rows"
    );
    assert_eq!(
        table_count(
            &writer,
            "SELECT COUNT(*) FROM projection_queue WHERE revival_generation = '2'"
        )
        .await,
        1,
        "the first rewrite must advance the queued accepted revival"
    );

    // The second beginning is exactly what an interrupted host re-import
    // repeats. Rejecting no-op updates on both tables proves the changed
    // predicates actually prevent writer amplification, not just identical
    // resulting values.
    writer
        .execute_batch(
            "CREATE TRIGGER reject_unchanged_presence
             BEFORE UPDATE OF generation ON observation_source_presence
             WHEN NEW.generation IS OLD.generation
             BEGIN SELECT RAISE(ABORT, 'repeated retained presence update'); END;
             CREATE TRIGGER reject_unchanged_revival
             BEFORE UPDATE OF revival_generation ON projection_queue
             WHEN NEW.revival_generation IS OLD.revival_generation
             BEGIN SELECT RAISE(ABORT, 'repeated accepted revival update'); END;
             CREATE TRIGGER reject_unchanged_rewrite
             BEFORE UPDATE ON observation_source_rewrites
             WHEN NEW.generation IS OLD.generation
               AND NEW.retained_through IS OLD.retained_through
             BEGIN SELECT RAISE(ABORT, 'repeated pending rewrite update'); END;",
        )
        .await
        .unwrap();
    registered
        .begin_observation_source_rewrite(&source, &scope, previous, generation, limit)
        .await
        .expect("restarting an identical layout must not update retained rows");
    assert_eq!(
        table_count(
            &writer,
            "SELECT COUNT(*) FROM observation_source_presence WHERE generation = '2'"
        )
        .await,
        2
    );
    assert_eq!(
        table_count(
            &writer,
            "SELECT COUNT(*) FROM projection_queue WHERE revival_generation = '2'"
        )
        .await,
        1
    );

    registered
        .begin_observation_source_rewrite(&source, &scope, generation, next, limit)
        .await
        .expect("a genuinely new rewrite must still advance the retained prefix");
    assert_eq!(
        table_count(
            &writer,
            "SELECT COUNT(*) FROM observation_source_presence WHERE generation = '3'"
        )
        .await,
        2
    );
    assert_eq!(
        table_count(
            &writer,
            "SELECT COUNT(*) FROM projection_queue WHERE revival_generation = '3'"
        )
        .await,
        1
    );
    assert_eq!(
        table_count(
            &writer,
            "SELECT COUNT(*) FROM observation_source_rewrites WHERE generation = '3'"
        )
        .await,
        1,
        "a new source layout must still advance the pending rewrite"
    );
}

/// Capturing one host page previously dispatched one SQLite statement per
/// record, even for reoffers identical to the already durable generation.
/// The JSON batch must preserve last-offer ordering, exact identity and
/// rollback while reducing writer commands to bounded pages.
#[asupersync::test]
async fn source_presence_pages_are_atomic_and_unchanged_reoffers_do_not_write() {
    const RECORDS: usize = 257;
    let harness = RegisteredGlobalDbHarness::open("source-presence-batched").await;
    let source =
        ObservationSourceIdentityV1::new(SessionId::new("session.presence-batched").unwrap())
            .unwrap();
    let scope = ObservationScopeV1::Profile;
    let first = ObservationSourceGenerationV1::new(1).unwrap();
    let second = ObservationSourceGenerationV1::new(2).unwrap();
    let offered = (0..RECORDS)
        .map(|index| ObservationSourcePresenceV1 {
            observation_id: tracedecay_domain::CanonicalObservationIdV1::new(format!(
                "sha256:{index:064x}"
            ))
            .unwrap(),
            generation: first,
            start_offset: index as u64,
        })
        .collect::<Vec<_>>();
    // SQLite accounts for the outer INSERT's rows when its statement ends.
    // Subtract the probe's own writes so all rows from one statement share
    // a marker, even though their AFTER INSERT trigger fires individually.
    let setup = harness.registered.begin_write_transaction().await.unwrap();
    setup
        .execute_batch(
            "CREATE TABLE presence_page_probe (prior_changes INTEGER NOT NULL);
             CREATE TRIGGER observe_presence_page AFTER INSERT ON observation_source_presence
             BEGIN
                 INSERT INTO presence_page_probe VALUES (
                     total_changes() - (SELECT COALESCE(MAX(rowid), 0) FROM presence_page_probe)
                 );
             END;",
        )
        .await
        .unwrap();
    setup.commit().await.unwrap();
    let statements = || writer_telemetry(&harness.registered).sqlite_vm.statements;
    // Measure the registered writer's own transaction bookkeeping on this shard.
    let before_empty = statements();
    let empty = harness.registered.begin_write_transaction().await.unwrap();
    empty.commit().await.unwrap();
    let transaction_overhead = statements() - before_empty;
    let before = statements();
    harness
        .registered
        .record_observation_source_presence(&source, &scope, &offered)
        .await
        .unwrap();
    assert_eq!(statements() - before, transaction_overhead + 3);
    let snapshot = harness.registered.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT COUNT(*) FROM presence_page_probe GROUP BY prior_changes ORDER BY prior_changes",
            (),
        )
        .await
        .unwrap();
    let mut pages = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        pages.push(row.get::<i64>(0).unwrap());
    }
    assert_eq!(pages, [128, 128, 1]);
    drop(rows);
    drop(snapshot);

    // A SQLite trigger makes a redundant UPDATE observable and fatal, even
    // if SQLite would have written unchanged column values.
    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "CREATE TRIGGER forbid_identical_presence_reoffer
             BEFORE UPDATE ON observation_source_presence
             WHEN NEW.generation = OLD.generation
                  AND NEW.start_offset = OLD.start_offset
             BEGIN SELECT RAISE(ABORT, 'identical presence reoffer'); END",
            (),
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let before = statements();
    harness
        .registered
        .record_observation_source_presence(&source, &scope, &offered)
        .await
        .unwrap();
    assert_eq!(statements() - before, transaction_overhead + 3);

    let mut replacement = offered
        .iter()
        .map(|record| ObservationSourcePresenceV1 {
            observation_id: record.observation_id.clone(),
            generation: second,
            start_offset: record.start_offset,
        })
        .collect::<Vec<_>>();
    replacement.last_mut().unwrap().start_offset = u64::MAX;
    assert!(
        harness
            .registered
            .record_observation_source_presence(&source, &scope, &replacement)
            .await
            .is_err(),
        "a malformed final-page offset must roll back all preceding pages"
    );

    let key = serde_json::to_string(&(source.clone(), scope.clone())).unwrap();
    let snapshot = harness.registered.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT COUNT(*), SUM(generation = '2')
             FROM observation_source_presence WHERE source_key = ?1",
            params![key.as_str()],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), RECORDS as i64);
    assert_eq!(
        row.get::<i64>(1).unwrap(),
        0,
        "the failing last page must not partially publish any generation changes"
    );
    drop(rows);
    drop(snapshot);

    replacement.last_mut().unwrap().start_offset = (RECORDS - 1) as u64;
    // A duplicate in the final page must keep the last offer's offset rather
    // than the first occurrence in an earlier page.
    replacement.push(ObservationSourcePresenceV1 {
        observation_id: replacement[0].observation_id.clone(),
        generation: second,
        start_offset: 10_000,
    });
    harness
        .registered
        .record_observation_source_presence(&source, &scope, &replacement)
        .await
        .unwrap();
    let snapshot = harness.registered.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT generation, start_offset FROM observation_source_presence
             WHERE source_key = ?1 AND observation_id = ?2",
            params![key.as_str(), replacement[0].observation_id.as_str()],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "2");
    assert_eq!(row.get::<i64>(1).unwrap(), 10_000);
    assert!(rows.next().await.unwrap().is_none());
}

/// Once retirement has committed, a reoffered source can still carry old
/// presence entries. Repeating their retirement must not perform an UPDATE
/// and receipt read for every already-disposed observation. This exercises
/// real projected records and the canonical disposition/queue authorities.
#[asupersync::test]
async fn rewrite_completion_skips_settled_records_without_revival() {
    async fn completed_writer_statements(records: usize) -> u64 {
        let directory = TempDir::new().unwrap();
        let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
            .await
            .unwrap();
        let registered = runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");
        let seeded = seed_projected_messages(&runtime, 0..records).await;
        let source = ObservationSourceIdentityV1::for_provider(
            ProviderId::new("codex").unwrap(),
            SessionId::new("session.settled-rewrite").unwrap(),
        )
        .unwrap();
        let scope = ObservationScopeV1::Profile;
        let first = ObservationSourceGenerationV1::new(1).unwrap();
        let second = ObservationSourceGenerationV1::new(2).unwrap();
        let third = ObservationSourceGenerationV1::new(3).unwrap();
        let offered = seeded
            .iter()
            .map(|(observation, _)| ObservationSourcePresenceV1 {
                observation_id: observation.observation_id().clone(),
                generation: first,
                start_offset: observation.identity().position().start(),
            })
            .collect::<Vec<_>>();
        registered
            .record_observation_source_presence(&source, &scope, &offered)
            .await
            .unwrap();
        registered
            .begin_observation_source_rewrite(&source, &scope, first, second, 0)
            .await
            .unwrap();
        assert_eq!(
            registered
                .complete_observation_source_rewrite(&source, &scope, second)
                .await
                .unwrap(),
            records as u64
        );

        let replay = offered
            .iter()
            .map(|record| ObservationSourcePresenceV1 {
                observation_id: record.observation_id.clone(),
                generation: second,
                start_offset: record.start_offset,
            })
            .collect::<Vec<_>>();
        registered
            .record_observation_source_presence(&source, &scope, &replay)
            .await
            .unwrap();
        registered
            .begin_observation_source_rewrite(&source, &scope, second, third, 0)
            .await
            .unwrap();

        // A settled observation can still have a newly accepted revival.
        // The optimization must clear this queued offer before skipping the
        // retired disposition; otherwise projection can resurrect old output.
        let writer = registered.writer_connection().unwrap();
        writer
            .execute(
                "INSERT INTO projection_queue (
                    observation_id, observation_sequence, revival_generation, revival_start_offset
                 )
                 SELECT observation_id, sequence, '2', 0
                 FROM observations WHERE observation_id = ?1
                 ON CONFLICT(observation_id) DO UPDATE SET
                     revival_generation = excluded.revival_generation,
                     revival_start_offset = excluded.revival_start_offset",
                params![seeded[0].0.observation_id().as_str()],
            )
            .await
            .unwrap();

        let statements = || writer_telemetry(registered).sqlite_vm.statements;
        let before = statements();
        assert_eq!(
            registered
                .complete_observation_source_rewrite(&source, &scope, third)
                .await
                .unwrap(),
            0,
            "a previously retired observation must not be retired twice"
        );
        let writer_commands = statements() - before;
        let snapshot = registered.read_snapshot().await.unwrap();
        let key = serde_json::to_string(&(source.clone(), scope.clone())).unwrap();
        let mut remaining = snapshot
            .query(
                "SELECT COUNT(*) FROM observation_source_presence WHERE source_key = ?1",
                params![key.as_str()],
            )
            .await
            .unwrap();
        let row = remaining.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 0);
        drop(remaining);
        let mut pending = snapshot
            .query(
                "SELECT COUNT(*) FROM observation_source_rewrites WHERE source_key = ?1",
                params![key.as_str()],
            )
            .await
            .unwrap();
        let row = pending.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 0);
        drop(pending);
        let mut revival = snapshot
            .query(
                "SELECT revival_generation, revival_start_offset
                 FROM projection_queue WHERE observation_id = ?1",
                params![seeded[0].0.observation_id().as_str()],
            )
            .await
            .unwrap();
        let row = revival.next().await.unwrap().unwrap();
        assert_eq!(row.get::<Option<String>>(0).unwrap(), None);
        assert_eq!(row.get::<Option<i64>>(1).unwrap(), None);
        writer_commands
    }

    let small = completed_writer_statements(8).await;
    let large = completed_writer_statements(64).await;
    assert!(small > 0);
    assert_eq!(
        large, small,
        "eightfold more settled records must not create per-record writer commands"
    );
}

/// With a large current-generation prefix, each older-generation page must
/// seek beyond already checked survivors instead of walking them repeatedly.
async fn source_rewrite_steps_with_survivor_prefix(unoffered: usize) -> u64 {
    const SURVIVORS: usize = 4_096;
    let harness = RegisteredGlobalDbHarness::open("source-rewrite-seek-prefix").await;
    let source =
        ObservationSourceIdentityV1::new(SessionId::new("session.seek-prefix").unwrap()).unwrap();
    let scope = ObservationScopeV1::Profile;
    let previous = ObservationSourceGenerationV1::new(1).unwrap();
    let generation = ObservationSourceGenerationV1::new(2).unwrap();
    let key = serde_json::to_string(&(source.clone(), scope.clone())).unwrap();
    let old_id = previous.generation_id().to_string();
    let new_id = generation.generation_id().to_string();
    let writer = harness.registered.writer_connection().unwrap();
    for start in (0..(SURVIVORS + unoffered)).step_by(128) {
        let end = (start + 128).min(SURVIVORS + unoffered);
        let values = (start..end)
            .map(|index| {
                (
                    format!("sha256:{index:06}"),
                    if index < SURVIVORS {
                        new_id.as_str()
                    } else {
                        old_id.as_str()
                    },
                )
            })
            .collect::<Vec<_>>();
        let encoded = serde_json::to_string(&values).unwrap();
        writer
            .execute(
                "INSERT INTO observation_source_presence (
                    source_key, observation_id, generation, start_offset
                 ) SELECT ?1, json_extract(value, '$[0]'),
                          json_extract(value, '$[1]'), 0 FROM json_each(?2)",
                params![key.as_str(), encoded.as_str()],
            )
            .await
            .unwrap();
    }
    harness
        .registered
        .begin_observation_source_rewrite(&source, &scope, previous, generation, 0)
        .await
        .unwrap();
    let before = writer_telemetry(&harness.registered);
    assert_eq!(
        harness
            .registered
            .complete_observation_source_rewrite(&source, &scope, generation)
            .await
            .unwrap(),
        0,
        "presence-only rows have no projected observations to retire"
    );
    let after = writer_telemetry(&harness.registered);
    assert_eq!(
        after.transactions.commands - before.transactions.commands,
        unoffered.div_ceil(128) as u64,
        "each bounded page must use exactly one writer job"
    );
    assert_eq!(
        table_count(
            &writer,
            &format!(
                "SELECT COUNT(*) FROM observation_source_presence WHERE generation = '{old_id}'"
            ),
        )
        .await,
        0,
        "all unoffered older records must be deleted"
    );
    assert_eq!(
        table_count(
            &writer,
            &format!(
                "SELECT COUNT(*) FROM observation_source_presence WHERE generation = '{new_id}'"
            ),
        )
        .await,
        SURVIVORS as i64,
        "the current-generation survivors must remain untouched"
    );
    after.sqlite_vm.vm_steps - before.sqlite_vm.vm_steps
}

#[asupersync::test]
async fn source_rewrite_pages_do_not_rescan_the_same_survivor_prefix() {
    let one_page_steps = source_rewrite_steps_with_survivor_prefix(127).await;
    let eight_page_steps = source_rewrite_steps_with_survivor_prefix(1_023).await;
    assert!(
        eight_page_steps < one_page_steps.saturating_mul(3),
        "eight retirement pages must seek beyond the 4096 survivor records: \
         one page {one_page_steps} VM steps; eight pages {eight_page_steps}"
    );
}

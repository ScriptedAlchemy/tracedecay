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
    for index in 0..UNOFFERED_RECORDS {
        writer
            .execute(
                "INSERT INTO observation_source_presence (
                    source_key, observation_id, generation, start_offset
                 ) VALUES (?1, ?2, ?3, 0)",
                params![
                    key.as_str(),
                    format!("sha256:unoffered-{index:06}"),
                    previous.generation_id().to_string()
                ],
            )
            .await
            .unwrap();
    }

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
    let before_commits = writer_telemetry(&harness.registered).transactions.committed_transactions;
    let retired = harness
        .registered
        .complete_observation_source_rewrite(&source, &scope, generation)
        .await
        .unwrap_or_else(|error| panic!("rewrite completion must not exceed query caps: {error}"));
    assert_eq!(retired, 0);
    let statements = writer_telemetry(&harness.registered).sqlite_vm.statements - before;
    let after = writer_telemetry(&harness.registered);
    let commits = after.transactions.committed_transactions - before_commits;
    assert_eq!(
        commits,
        (UNOFFERED_RECORDS as u64).div_ceil(128),
        "each bounded page must durably commit before the next page"
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

async fn table_count(conn: &impl QueryExecutor, sql: &str) -> i64 {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let row = rows.next().await.unwrap().unwrap();
    row.get(0).unwrap()
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
    let seeded = seed_projected_messages(&runtime, 0..RECORDS).await;

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

    let before_commits = writer_telemetry(registered).transactions.committed_transactions;
    let error = registered
        .complete_observation_source_rewrite(&source, &scope, generation)
        .await
        .unwrap_err();
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
    let commits = writer_telemetry(registered).transactions.committed_transactions - before_commits;
    assert!(commits >= 1, "at least one bounded retirement page must commit");
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

    // Reopen the registered profile store as a new daemon fixture. Durable
    // rewrite state, dispositions and the source-presence page frontier must
    // survive rather than replay the already committed retirement work.
    drop(writer);
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

    writer
        .execute(
            "DELETE FROM observation_projection_provenance
             WHERE observation_id = ?1 AND output_ordinal = 1",
            params![survivor.observation_id().as_str()],
        )
        .await
        .unwrap();

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

        let statements = || writer_telemetry(&registered).sqlite_vm.statements;
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

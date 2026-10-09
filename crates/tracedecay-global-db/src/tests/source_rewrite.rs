//! Regression tests for registered source-rewrite completion.
//!
//! Rewrite completion retires every record the rewritten source no longer
//! offers. One long host session can hold more unoffered records than the
//! exact-SQL engine's per-query materialization cap, so completion must read
//! them in keyset pages rather than one unbounded statement; the unbounded
//! read failed deterministically for exactly those sources and wedged the
//! host catch-up in a retry loop that never converged.

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
    seed_projected_messages,
};

/// More unoffered records than one keyset page *and* more than the engine's
/// per-query materialization cap, so the test fails against the unbounded
/// read and exercises repeated pagination against the fix.
const UNOFFERED_RECORDS: i64 = 10_003;

#[tokio::test]
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

    // None of the seeded records are re-offered under the new generation, so
    // completion must retire-path all of them. The `observations` table is
    // empty, so each retire is a no-op returning false; what matters is that
    // the reads stay under the materialization cap and the settle deletes
    // still clear the source.
    let retired = harness
        .registered
        .complete_observation_source_rewrite(&source, &scope, generation)
        .await
        .unwrap_or_else(|error| panic!("rewrite completion must not exceed query caps: {error}"));
    assert_eq!(retired, 0);

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

/// Retiring an unoffered record is real work, not a presence-row cleanup:
/// its provenance and workflow facts are removed, a `source_record_retired`
/// disposition takes their place, and a sole-owned projected output is
/// deleted. A completion that dies mid-transaction must roll every earlier
/// retirement back; the pending rewrite survives, so a retry after the
/// failure clears converges — the retry loop the paged read exists to keep
/// alive would otherwise stay wedged.
///
/// The fixture plants an inconsistent provenance row (a surviving co-owner
/// that claims an output its durable record does not derive) on the
/// record the keyset order retires last, so the first attempt retires real
/// records across pages and then fails inside
/// `retire_source_record` with `ProvenanceCollision`. Removing the planted
/// row and completing again retires every unoffered record for real.
#[tokio::test]
async fn rewrite_completion_retires_projected_records_and_rolls_back_a_failed_attempt() {
    // More real projected records than one keyset page, so a failed attempt
    // leaves proven retirements to roll back on every page.
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

    let error = registered
        .complete_observation_source_rewrite(&source, &scope, generation)
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProjectionStoreError::ProvenanceCollision),
        "colliding output ownership must fail completion, got {error:?}"
    );

    // Every retirement the attempt already performed must be rolled back:
    // provenance and projected outputs restored, no dispositions recorded,
    // and the pending rewrite still waiting for a converging retry.
    assert_eq!(
        table_count(
            &writer,
            "SELECT COUNT(*) FROM observation_projection_dispositions"
        )
        .await,
        0,
        "retirements before the collision must roll back with the transaction"
    );
    assert_eq!(
        table_count(
            &writer,
            "SELECT COUNT(*) FROM observation_projection_provenance"
        )
        .await,
        i64::try_from(RECORDS).unwrap() + 1,
        "rolled-back retirements must restore provenance"
    );
    assert_eq!(
        table_count(&writer, "SELECT COUNT(*) FROM lcm_raw_messages").await,
        i64::try_from(RECORDS).unwrap(),
        "rolled-back retirements must restore projected outputs"
    );
    assert_eq!(
        table_count(&writer, "SELECT COUNT(*) FROM observation_source_rewrites").await,
        1,
        "the failed attempt must leave the rewrite pending"
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
    assert_eq!(retired, u64::try_from(RECORDS - 1).unwrap());

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

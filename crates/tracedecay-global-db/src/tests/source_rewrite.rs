//! Regression tests for registered source-rewrite completion.
//!
//! Rewrite completion retires every record the rewritten source no longer
//! offers. One long host session can hold more unoffered records than the
//! exact-SQL engine's per-query materialization cap, so completion must read
//! them in keyset pages rather than one unbounded statement; the unbounded
//! read failed deterministically for exactly those sources and wedged the
//! host catch-up in a retry loop that never converged.

use tracedecay_domain::{
    ObservationScopeV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1, SessionId,
};
use tracedecay_runtime_core::db::engine::params;

use crate::tests::harness::RegisteredGlobalDbHarness;

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
        .begin_observation_source_rewrite(
            &source,
            &scope,
            previous,
            generation.clone(),
            0,
        )
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

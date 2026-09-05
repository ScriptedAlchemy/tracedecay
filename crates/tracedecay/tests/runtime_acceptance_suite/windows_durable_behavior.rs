//! Durable temporal, LCM, storage, and contract behavior used by Windows CI.
//!
//! This target consolidates the retained behavior through the same product
//! helpers as the ordinary suites. CI selects this binary as a whole instead
//! of maintaining module-prefix filters or expected test counts.

use crate::common;

#[tokio::test]
async fn sqlite_writer_uses_production_wal_normal_policy() {
    let tmp = tempfile::TempDir::new().expect("create SQLite policy fixture");
    let database_path = tmp.path().join("policy.db");
    let (database, _) = common::initialize_test_database(&database_path)
        .await
        .expect("initialize SQLite policy fixture");

    // `journal_mode` is a database-level property, so any connection observes
    // the writer's WAL choice. `synchronous` and `wal_autocheckpoint` are
    // connection-scoped, and production applies them only to the writer
    // (`apply_pragmas` under `ConnectionMode::Writer`), which then verifies
    // `synchronous == NORMAL` and `wal_autocheckpoint == 0` and refuses to
    // publish the connection otherwise. Reading those two from
    // `read_connection()` measured the reader's SQLite defaults (FULL, 1000),
    // never the writer's policy -- so this asserts the writer's policy through
    // what it durably produces instead.
    let reader = database.read_connection();
    let journal_mode = {
        let mut rows = reader
            .query("SELECT lower(journal_mode) FROM pragma_journal_mode()", ())
            .await
            .expect("inspect production SQLite journal policy");
        let row = rows
            .next()
            .await
            .expect("read production SQLite journal policy")
            .expect("production SQLite journal policy row");
        row.get::<String>(0).expect("journal_mode")
    };
    assert_eq!(journal_mode, "wal");

    database
        .execute_write(
            "production WAL policy fixture",
            "CREATE TABLE wal_policy_probe (id INTEGER PRIMARY KEY)",
            (),
        )
        .await
        .expect("commit through the production writer broker");

    // With the writer's auto-checkpoint disabled, a committed write stays in
    // the write-ahead log instead of being folded back into the database file.
    let wal = database_path.with_extension("db-wal");
    let wal_bytes = std::fs::metadata(&wal)
        .unwrap_or_else(|error| panic!("production writer must retain {}: {error}", wal.display()))
        .len();
    assert!(
        wal_bytes > 0,
        "an unchecked-pointed production writer must retain its write-ahead log"
    );
}

mod temporal_kernel_behavior {
    use tracedecay_temporal_query::candidates::{CandidateChannel, plan_candidates};

    #[test]
    fn public_candidate_plan_preserves_exact_phrase_and_time_authority() {
        let query = "\"durable checkpoint\" 2026-07-31";
        let plan = plan_candidates(query);

        assert!(plan.contains(CandidateChannel::ExactMessage, query));
        assert!(plan.contains(CandidateChannel::Phrase, "durable checkpoint"));
        assert!(plan.contains(CandidateChannel::Time, "2026-07-31"));
        assert!(!plan.has_semantic_channel());
    }
}

mod lcm_payload_behavior {
    use tempfile::TempDir;
    use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
    use tracedecay_lcm::{LcmExpandRequest, LcmExpandTarget};
    use tracedecay_sessions::admission::HostAdmissionScope;

    use super::common::{lcm_payload_message, lcm_payload_session};

    #[tokio::test]
    async fn canonical_external_payload_survives_ingest_and_expansion() {
        let tmp = TempDir::new().unwrap();
        let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
            .await
            .expect("open canonical profile runtime");
        runtime
            .upsert_session_for_test(
                HostAdmissionScope::Profile,
                &lcm_payload_session("cursor", "durable-payload-session"),
            )
            .await
            .expect("store canonical session");
        let content = format!("durable payload\n{}", "P".repeat(260 * 1024));
        let message = lcm_payload_message(
            "cursor",
            "durable-payload-message",
            "durable-payload-session",
            "tool",
            &content,
        );
        runtime
            .lcm_ingest_raw_message_for_test(HostAdmissionScope::Profile, &message)
            .await
            .expect("externalize canonical payload");
        let payload_ref = runtime
            .lcm_load_raw_message_for_test("cursor", "durable-payload-message")
            .await
            .and_then(|stored| stored.payload_ref)
            .expect("external payload reference");
        let expanded = runtime
            .lcm_expand_for_test(LcmExpandRequest {
                provider: "cursor".to_string(),
                session_id: "durable-payload-session".to_string(),
                target: LcmExpandTarget::ExternalPayload { payload_ref },
                content_slice: None,
                source_offset: 0,
                source_limit: None,
            })
            .await
            .expect("expand canonical payload");
        assert_eq!(expanded.content, content);
    }
}

mod lcm_query_behavior {
    use tempfile::TempDir;
    use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
    use tracedecay_sessions::admission::HostAdmissionScope;

    use super::common::{lcm_payload_message, lcm_payload_session};

    #[tokio::test]
    async fn canonical_status_reads_ingested_session_state() {
        let tmp = TempDir::new().unwrap();
        let runtime = HostAdmissionTestRuntimeV1::profile(tmp.path().join(".tracedecay"))
            .await
            .expect("open canonical profile runtime");
        runtime
            .upsert_session_for_test(
                HostAdmissionScope::Profile,
                &lcm_payload_session("cursor", "durable-query-session"),
            )
            .await
            .expect("store canonical session");
        runtime
            .lcm_ingest_raw_message_for_test(
                HostAdmissionScope::Profile,
                &lcm_payload_message(
                    "cursor",
                    "durable-query-message",
                    "durable-query-session",
                    "assistant",
                    "durable query payload",
                ),
            )
            .await
            .expect("store canonical LCM record");

        let status = runtime
            .lcm_status_for_test("cursor", Some("durable-query-session"))
            .await
            .expect("read canonical LCM status");
        assert_eq!(status.raw_message_count, 1);
        assert_eq!(status.store.messages, 1);
        assert!(status.store.token_estimate.complete);
    }
}

mod lcm_schema_durability {
    use tempfile::TempDir;
    use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
    use tracedecay_sessions::admission::HostAdmissionScope;

    use super::common::{lcm_payload_message, lcm_payload_session};

    #[tokio::test]
    async fn canonical_profile_reopen_preserves_lcm_records() {
        let tmp = TempDir::new().unwrap();
        let profile_root = tmp.path().join(".tracedecay");
        let runtime = HostAdmissionTestRuntimeV1::profile(profile_root.clone())
            .await
            .expect("open canonical profile runtime");
        let session = lcm_payload_session("cursor", "durable-schema-session");
        runtime
            .upsert_session_for_test(HostAdmissionScope::Profile, &session)
            .await
            .expect("store canonical session");
        let message = lcm_payload_message(
            "cursor",
            "durable-schema-message",
            "durable-schema-session",
            "tool",
            "durable schema payload",
        );
        runtime
            .lcm_ingest_raw_message_for_test(HostAdmissionScope::Profile, &message)
            .await
            .expect("store canonical LCM record");
        drop(runtime);

        let reopened = HostAdmissionTestRuntimeV1::profile(profile_root)
            .await
            .expect("reopen canonical profile runtime");
        let stored = reopened
            .lcm_load_raw_message_for_test("cursor", "durable-schema-message")
            .await
            .expect("load durable LCM record after reopen");
        assert_eq!(stored.session_id, "durable-schema-session");
        assert_eq!(stored.content, "durable schema payload");
    }
}

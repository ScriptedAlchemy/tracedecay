//! Durable temporal, LCM, storage, and contract behavior used by Windows CI.
//!
//! This target consolidates the retained behavior through the same product
//! helpers as the ordinary suites. CI selects this binary as a whole instead
//! of maintaining module-prefix filters or expected test counts.

#[path = "common/mod.rs"]
mod common;
#[path = "storage_suite/support.rs"]
mod support;

#[path = "session_suite/lcm_summary_lineage_review.rs"]
mod lcm_summary_lineage_review;
#[path = "session_suite/temporal_projection/mod.rs"]
mod temporal_projection;

#[path = "storage_suite/corruption_test.rs"]
mod corruption_test;
#[path = "storage_suite/db_query_test.rs"]
mod db_query_test;
#[path = "storage_suite/fact_merge_hydration_test.rs"]
mod fact_merge_hydration;

#[path = "../crates/tracedecay-domain/tests/session_contract.rs"]
mod domain_session_contract;

#[tokio::test]
async fn storage_fixture_authorities_are_available() {
    let _guard = support::HOME_ENV_LOCK.lock().await;
    assert!(support::ephemeral_safe_fixture_base().is_absolute());
}

#[tokio::test]
async fn sqlite_writer_uses_production_wal_normal_policy() {
    let tmp = tempfile::TempDir::new().expect("create SQLite policy fixture");
    let (database, _) = common::initialize_test_database(&tmp.path().join("policy.db"))
        .await
        .expect("initialize SQLite policy fixture");
    let writer = database
        .writer_connection("inspect production SQLite policy")
        .await
        .expect("acquire SQLite writer");
    let (journal_mode, synchronous) = {
        let mut rows = writer
            .engine_connection()
            .query(
                "SELECT lower(journal_mode), synchronous
                 FROM pragma_journal_mode(), pragma_synchronous()",
                (),
            )
            .await
            .expect("inspect production SQLite journal policy");
        let row = rows
            .next()
            .await
            .expect("read production SQLite journal policy")
            .expect("production SQLite journal policy row");
        (
            row.get::<String>(0).expect("journal_mode"),
            row.get::<i64>(1).expect("synchronous"),
        )
    };
    let wal_autocheckpoint = {
        let mut rows = writer
            .engine_connection()
            .query("PRAGMA wal_autocheckpoint", ())
            .await
            .expect("inspect production SQLite checkpoint policy");
        let row = rows
            .next()
            .await
            .expect("read production SQLite checkpoint policy")
            .expect("production SQLite checkpoint policy row");
        row.get::<i64>(0).expect("wal_autocheckpoint")
    };

    assert_eq!(journal_mode, "wal");
    assert_eq!(synchronous, 1);
    assert_eq!(wal_autocheckpoint, 0);
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
    use tracedecay::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
    use tracedecay::sessions::lcm::{LcmExpandRequest, LcmExpandTarget};

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
    use tracedecay::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

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
    use tracedecay::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

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

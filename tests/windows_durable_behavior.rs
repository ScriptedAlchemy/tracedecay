//! Durable temporal, LCM, storage, and contract behavior used by Windows CI.
//!
//! This target consolidates the retained behavior through the same product
//! helpers as the ordinary suites. CI selects this binary as a whole instead
//! of maintaining module-prefix filters or expected test counts.

#[path = "common/mod.rs"]
mod common;
#[path = "storage_suite/support.rs"]
mod support;

#[path = "session_suite/lcm_payload.rs"]
mod lcm_payload;
#[path = "session_suite/lcm_query/mod.rs"]
mod lcm_query;
#[path = "session_suite/lcm_summary_lineage_review.rs"]
mod lcm_summary_lineage_review;
#[path = "session_suite/temporal_application.rs"]
mod temporal_kernel_behavior;
#[path = "session_suite/temporal_projection/mod.rs"]
mod temporal_projection;

#[path = "storage_suite/corruption_test.rs"]
mod corruption_test;
#[path = "storage_suite/db_query_test.rs"]
mod db_query_test;
#[path = "storage_suite/fact_merge_hydration_test.rs"]
mod fact_merge_hydration;
#[path = "storage_suite/migration_manifest_test.rs"]
mod migration_manifest;

#[path = "../crates/tracedecay-domain/tests/session_contract.rs"]
mod domain_session_contract;

mod lcm_schema_durability {
    use tempfile::TempDir;
    use tracedecay::application::host_admission::{
        HostAdmissionScope, HostAdmissionTestRuntimeV1,
    };

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
            1,
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

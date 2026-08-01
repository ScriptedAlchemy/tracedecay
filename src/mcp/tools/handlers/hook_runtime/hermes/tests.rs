use super::super::*;
use crate::application::host_admission::{
    HostAdmissionScope, HostAdmissionStatus, HostAdmissionTestRuntimeV1,
};

use super::super::test_support::*;
use super::*;

#[tokio::test]
async fn projectless_hermes_receipt_uses_user_profile_without_local_writer() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("tracedecay-profile");
    let hermes_home = temp.path().join("hermes-home");
    let hermes_profile = hermes_home.join("profiles/test");
    std::fs::create_dir_all(&hermes_profile).unwrap();
    std::fs::create_dir_all(&profile_root).unwrap();
    let fixture = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let broker = fixture
        .host_admission_broker_for_test(HostAdmissionScope::Profile)
        .unwrap();

    let result = hermes_receipt(
        &json!({
            "action": "hermes_receipt",
            "event": hermes_turn_completed_event("session-local-writer", "wm-local-1"),
        }),
        &profile_root,
        None,
        required_user_db(fixture.mcp_session_authorities()).unwrap(),
        &broker,
    )
    .await
    .expect("projectless Hermes receipt should commit through the user-profile broker");

    assert_eq!(result["action"], "hermes_receipt");
    assert_eq!(result["status"], "recorded");
    assert_eq!(broker.pending_count().await, 0);
    let automation_root = crate::automation::runner::user_automation_root(&profile_root);
    assert!(
        automation_root.join("host_receipts.json").is_file(),
        "receipt watermark state must live under the user TraceDecay profile"
    );
    for forbidden in [
        hermes_profile.join("host_receipts.json"),
        hermes_profile.join("sessions.db"),
        hermes_profile.join(".tracedecay"),
        hermes_home.join("host_receipts.json"),
        hermes_home.join(".tracedecay"),
    ] {
        assert!(
            !forbidden.exists(),
            "projectless Hermes receipt must not create a local fallback writer at {}",
            forbidden.display()
        );
    }
}

#[tokio::test]
async fn projectless_hermes_receipt_is_durable_before_apply_and_replays_after_restart() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("tracedecay-profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let fixture = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let automation_root = crate::automation::runner::user_automation_root(&profile_root);
    // Block canonical apply so admission can prove durability-before-attempt.
    std::fs::write(&automation_root, "not-a-directory").unwrap();

    let broker = fixture
        .host_admission_broker_for_test(HostAdmissionScope::Profile)
        .unwrap();
    let err = hermes_receipt(
        &json!({
            "action": "hermes_receipt",
            "event": hermes_turn_completed_event("session-restart", "wm-restart-1"),
        }),
        &profile_root,
        None,
        required_user_db(fixture.mcp_session_authorities()).unwrap(),
        &broker,
    )
    .await
    .expect_err("blocked user-automation root must retain the durable Hermes receipt");
    let data = structured_hook_error_data(&err).expect("bounded hook error");
    assert_eq!(data["reason_code"], "canonical_admission_failed");
    assert_eq!(data["retryable"], true);
    assert_eq!(broker.pending_count().await, 1);
    drop(broker);

    std::fs::remove_file(&automation_root).unwrap();
    let recovered = fixture
        .host_admission_broker_for_test(HostAdmissionScope::Profile)
        .unwrap();
    let outcome = replay_projectless_hermes_host_admission(&recovered, &profile_root).await;
    // A full drain with no target seq reports accepted_for_replay once the
    // retained prefix is committed; the durable watermark is the authority.
    assert!(matches!(
        outcome.status,
        HostAdmissionStatus::Committed | HostAdmissionStatus::AcceptedForReplay
    ));
    assert_eq!(recovered.pending_count().await, 0);
    assert!(
        automation_root.join("host_receipts.json").is_file(),
        "restart replay must write receipts only under the user TraceDecay profile"
    );
}

#[tokio::test]
async fn malformed_profile_source_does_not_starve_valid_sibling_source() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("tracedecay-profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let fixture = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let broker = fixture
        .host_admission_broker_for_test(HostAdmissionScope::Profile)
        .unwrap();
    let valid_payload = valid_hermes_terminal_receipt_payload("session-sibling", "wm-sibling-1");

    let malformed = broker
        .admit(
            "hermes:malformed-source",
            br#"{"version":1,"plan":{"kind":"add_branch","branch":""}}"#,
        )
        .await
        .unwrap();
    broker
        .admit("hermes:valid-source", &valid_payload)
        .await
        .unwrap();

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        replay_projectless_hermes_receipts(&broker, &profile_root, Some(malformed.seq)),
    )
    .await
    .expect("bounded profile replay must not spin on the malformed record")
    .expect("replay should finish with a typed disposition");

    assert_eq!(outcome.reason_code, Some("host_event_payload_malformed"));
    assert!(!outcome.retryable);
    assert_eq!(
        broker.pending_count().await,
        0,
        "terminal evidence is quarantined and the committed sibling releases active capacity"
    );
    assert_eq!(broker.quarantine_count().await, 1);
    let automation_root = crate::automation::runner::user_automation_root(&profile_root);
    assert!(
        automation_root.join("host_receipts.json").is_file(),
        "valid sibling must apply under the user TraceDecay profile"
    );

    let reopen = replay_projectless_hermes_host_admission(&broker, &profile_root).await;
    assert_eq!(reopen.status, HostAdmissionStatus::AcceptedForReplay);
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(broker.quarantine_count().await, 1);
}

#[tokio::test]
async fn malformed_profile_payload_is_quarantined_across_reopen() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("tracedecay-profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let fixture = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let broker = fixture
        .host_admission_broker_for_test(HostAdmissionScope::Profile)
        .unwrap();
    let admitted = broker
        .admit(
            "hermes:invalid-plan-fixture",
            br#"{"version":1,"plan":{"kind":"add_branch","branch":""}}"#,
        )
        .await
        .unwrap();

    let outcome = replay_projectless_hermes_receipts(&broker, &profile_root, Some(admitted.seq))
        .await
        .expect("replay should finish with a typed disposition");

    assert_eq!(outcome.status, HostAdmissionStatus::Unavailable);
    assert_eq!(outcome.reason_code, Some("host_event_payload_malformed"));
    assert!(!outcome.retryable);
    assert_eq!(broker.pending_count().await, 0);
    assert_eq!(broker.quarantine_count().await, 1);
    let rendered = serde_json::to_string(&outcome).unwrap();
    assert!(!rendered.contains("invalid-plan-fixture"));
    assert!(!rendered.contains("\"branch\":\"\""));
    drop(broker);

    let recovered = fixture
        .host_admission_broker_for_test(HostAdmissionScope::Profile)
        .unwrap();
    assert_eq!(recovered.pending_count().await, 0);
    assert_eq!(recovered.quarantine_count().await, 1);
    let reopen = replay_projectless_hermes_host_admission(&recovered, &profile_root).await;
    assert_eq!(reopen.status, HostAdmissionStatus::AcceptedForReplay);
    assert_eq!(recovered.pending_count().await, 0);
    assert_eq!(recovered.quarantine_count().await, 1);
}

#[tokio::test]
async fn unsupported_profile_payload_version_is_retained_without_apply() {
    let temp = tempfile::TempDir::new().unwrap();
    let profile_root = temp.path().join("tracedecay-profile");
    std::fs::create_dir_all(&profile_root).unwrap();
    let fixture = HostAdmissionTestRuntimeV1::profile(&profile_root)
        .await
        .unwrap();
    let broker = fixture
        .host_admission_broker_for_test(HostAdmissionScope::Profile)
        .unwrap();
    let admitted = broker
        .admit(
            "hermes:future-plan-fixture",
            br#"{"version":2,"plan":{"kind":"future_host_event","opaque":"private"}}"#,
        )
        .await
        .unwrap();

    let outcome = replay_projectless_hermes_receipts(&broker, &profile_root, Some(admitted.seq))
        .await
        .expect("replay should finish with a typed disposition");

    assert_eq!(outcome.status, HostAdmissionStatus::Unavailable);
    assert_eq!(
        outcome.reason_code,
        Some("host_event_payload_unsupported_version")
    );
    assert!(outcome.retryable);
    assert_eq!(broker.pending_count().await, 1);
    assert_eq!(broker.quarantine_count().await, 0);
    let automation_root = crate::automation::runner::user_automation_root(&profile_root);
    assert!(
        !automation_root.join("host_receipts.json").is_file(),
        "unsupported version must not attempt canonical profile apply"
    );
}

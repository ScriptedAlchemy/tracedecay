use super::super::*;
use crate::application::host_admission::HostAdmissionTestRuntimeV1;

use super::*;

#[test]
fn cursor_compaction_response_matches_hook_contract() {
    let value = cursor_compact_skipped("no messages to compact");
    let outcome: crate::hooks::CursorPreCompactOutcome = serde_json::from_value(value).unwrap();
    assert_eq!(outcome.status, "skipped");
    assert_eq!(outcome.reason, "no messages to compact");
    assert_eq!(outcome.summary_nodes_created, 0);
    assert!(outcome.summary_node_ids.is_empty());
}

#[tokio::test]
async fn daemon_profile_ingest_rejects_an_unregistered_database() {
    let temp = tempfile::TempDir::new().unwrap();
    let fixture = HostAdmissionTestRuntimeV1::profile(temp.path())
        .await
        .unwrap();
    let admission = host_admission_facade(
        None,
        HostAdmissionScope::Profile,
        fixture.unregistered_mcp_session_authorities_for_test(HostAdmissionScope::Profile),
    )
    .unwrap()
    .accept_replay("cursor", HostAdmissionScope::Profile);

    assert_eq!(admission.status, HostAdmissionStatus::Unavailable);
    assert_eq!(
        admission.reason_code,
        Some("registered_authority_unavailable")
    );
}

#[tokio::test]
async fn transcript_admission_rejects_unknown_provider_without_echoing_hook_payload() {
    let secret = "hook-secret-unknown-provider";
    let error = ingest_transcript(
        None,
        &json!({
            "provider": "unknown-provider-v99",
            "event_json": format!("{{\"raw_source\":\"{secret}\"}}"),
        }),
        None,
        None,
        SessionAuthorities::default(),
    )
    .await
    .unwrap_err();

    let data = structured_hook_error_data(&error).unwrap();
    assert_eq!(data["status"], "unknown");
    assert_eq!(data["reason_code"], "unknown_provider");
    assert_eq!(data["retryable"], false);
    assert!(!error.to_string().contains(secret));
    assert!(!data.to_string().contains(secret));
}

#[tokio::test]
async fn supported_transcript_admission_requires_its_authority_without_echoing_payload() {
    let secret = "hook-secret-unavailable-authority";
    let error = ingest_transcript(
        None,
        &json!({
            "provider": "claude",
            "event_json": format!("{{\"malformed\":\"{secret}\"}}"),
        }),
        None,
        None,
        SessionAuthorities::default(),
    )
    .await
    .unwrap_err();

    let data = structured_hook_error_data(&error).unwrap();
    assert_eq!(data["status"], "unavailable");
    assert_eq!(data["reason_code"], "authority_unavailable");
    assert_eq!(data["retryable"], true);
    assert!(!error.to_string().contains(secret));
    assert!(!data.to_string().contains(secret));
}

#[test]
fn capture_registry_owns_every_supported_transcript_route() {
    for route in [
        ("claude", true),
        ("codex", true),
        ("cursor", true),
        ("kiro", true),
        ("codex", false),
        ("cursor", false),
        ("kiro", false),
    ] {
        assert!(
            super::kernels::transcript_capture_kernel(route.0, route.1).is_some(),
            "no capture kernel registered for {route:?}"
        );
    }
    // Routes with no registered kernel are reported through the typed
    // `unknown_provider` admission status rather than a generic config error.
    for route in [
        ("claude", false),
        ("hermes", true),
        ("hermes", false),
        ("unknown-provider-v99", true),
    ] {
        assert!(
            super::kernels::transcript_capture_kernel(route.0, route.1).is_none(),
            "unexpected capture kernel registered for {route:?}"
        );
    }
}

#[test]
fn cursor_event_numbers_accept_numeric_and_string_forms() {
    let event = json!({ "tokens": "42", "message_count": 7 });
    assert_eq!(event_i64(&event, &["tokens"]), Some(42));
    assert_eq!(event_usize(&event, &["message_count"]), Some(7));
}

use super::super::*;
use crate::host_admission::HostAdmissionTestRuntimeV1;

use super::*;

#[test]
fn cursor_compaction_response_matches_hook_contract() {
    let value = cursor_compact_skipped("no messages to compact");
    let outcome: crate::hooks::CursorPreCompactOutcome = serde_json::from_value(value).unwrap();
    assert_eq!(outcome.status, "skipped");
    assert_eq!(outcome.reason, "no messages to compact");
    assert_eq!(outcome.summary_nodes_created, 0);
    assert!(outcome.summary_node_ids.is_empty());
    assert_eq!(
        outcome.relation_projection_status,
        tracedecay_sessions::runtime::lcm::LcmRelationProjectionStatus::NotApplicable
    );
}

#[test]
fn codex_and_cursor_compaction_requests_never_carry_host_payload() {
    let event_digest = tracedecay_domain::canonical_sha256(&"compaction-event").unwrap();
    for protocol in [
        LcmHostProtocol::CodexContextCompacted {
            protocol_revision: "codex.context-compacted.v1".to_owned(),
            event_digest: event_digest.clone(),
        },
        LcmHostProtocol::CursorPreCompact {
            protocol_revision: "cursor.precompact.v1".to_owned(),
            event_digest: event_digest.clone(),
        },
    ] {
        let provider = protocol.provider().to_owned();
        let LcmAuthorityRequest::Compact(command) = pressure_only_command(
            &provider,
            "session-1",
            Some(1_000),
            Some(200_000),
            None,
            None,
            protocol,
        ) else {
            panic!("pressure-only evidence must dispatch as a compaction command");
        };
        assert_eq!(command.preflight.provider, provider);
        assert!(
            command.preflight.messages.is_empty(),
            "compaction pressure evidence must never carry host transcript payload"
        );
    }
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
        None,
        SessionAuthorities::default(),
    )
    .await
    .unwrap_err();

    let data = structured_hook_error_data(&error).unwrap();
    assert_eq!(data["status"], "unavailable");
    // The admission authority's own verdict reaches the host: an unbound
    // project authority is reported by its own reason code and its own
    // (non-retryable) classification, not laundered into a generic retryable
    // `authority_unavailable`.
    assert_eq!(data["reason_code"], "project_authority_unbound");
    assert_eq!(data["retryable"], false);
    assert!(!error.to_string().contains(secret));
    assert!(!data.to_string().contains(secret));
}

#[tokio::test]
async fn claude_postcompact_without_machine_provenance_is_read_only_unavailable() {
    let outcome = claude_compact(
        &json!({
            "event_json": r#"{"compact_summary":"self-asserted","digest":"self-asserted"}"#,
        }),
        SessionAuthorities::default(),
    )
    .await
    .unwrap();

    assert_eq!(outcome["status"], "unavailable");
    assert_eq!(
        outcome["reason"],
        "claude_postcompact_provenance_unavailable"
    );
    assert_eq!(outcome["summary_nodes_created"], 0);
    assert_eq!(outcome["summary_node_ids"], json!([]));
}

#[test]
fn capture_registry_owns_every_supported_transcript_route() {
    use super::kernels::TranscriptPayloadRouteV1::{InlineMessages, SourceScan};

    for route in [
        ("claude", true, SourceScan),
        ("codex", true, SourceScan),
        ("cursor", true, SourceScan),
        ("hermes", true, SourceScan),
        ("kiro", true, SourceScan),
        ("codex", false, SourceScan),
        ("cursor", false, SourceScan),
        ("hermes", false, SourceScan),
        ("kiro", false, SourceScan),
        // The Hermes turn callback inlines its messages; it is a registered
        // capture route, not a branch above the lookup.
        ("hermes", true, InlineMessages),
        ("hermes", false, InlineMessages),
    ] {
        assert!(
            super::kernels::transcript_capture_kernel(route.0, route.1, route.2).is_some(),
            "no capture kernel registered for {route:?}"
        );
    }
    // Routes with no registered kernel are reported through the typed
    // `unknown_provider` admission status rather than a generic config error.
    for route in [
        ("claude", false, SourceScan),
        ("claude", true, InlineMessages),
        ("codex", false, InlineMessages),
        ("unknown-provider-v99", true, SourceScan),
    ] {
        assert!(
            super::kernels::transcript_capture_kernel(route.0, route.1, route.2).is_none(),
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

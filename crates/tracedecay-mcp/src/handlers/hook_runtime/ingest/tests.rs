use super::super::*;
use crate::structured_hook_error_data;
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_sessions::admission::{HostAdmissionOutcome, HostAdmissionStatus};

use super::*;

fn status_for(account: &IngestCommitAccount) -> HostAdmissionStatus {
    let verdict = ingest_commit_verdict(account);
    complete_ingest_admission(
        HostAdmissionOutcome::accepted_for_replay(),
        verdict.authority_changed,
        verdict.exact_duplicate,
        false,
    )
    .status
}

/// A shared-queue residual is not this pass's commit. Nine projected rows
/// left by a peer, or by another provider on the same scope queue, must stay
/// `accepted_for_replay` when admission persisted nothing and cannot prove a
/// duplicate.
#[test]
fn drain_residual_does_not_commit_an_admission_owned_pass() {
    let status = status_for(&IngestCommitAccount {
        admission_owns_commit: true,
        observations_committed: 0,
        route_exact_duplicate: false,
        messages_upserted: 9,
        snapshot_messages_upserted: 0,
        claude_observations_committed: 0,
        claude_cursor_advances: 0,
        claude_observation_duplicates: 0,
        claude_cursor_duplicates: 0,
    });

    assert_eq!(status, HostAdmissionStatus::AcceptedForReplay);
}

/// The pass persisted two observations and the drain found nothing. The
/// commit still stands.
#[test]
fn admission_commit_stands_when_the_drain_is_empty() {
    let status = status_for(&IngestCommitAccount {
        admission_owns_commit: true,
        observations_committed: 2,
        route_exact_duplicate: false,
        messages_upserted: 0,
        snapshot_messages_upserted: 4,
        claude_observations_committed: 1,
        claude_cursor_advances: 1,
        claude_observation_duplicates: 0,
        claude_cursor_duplicates: 0,
    });

    assert_eq!(status, HostAdmissionStatus::Committed);
}

/// A peer already admitted the source. Residual projected rows must not
/// rewrite that duplicate into a fresh commit.
#[test]
fn drain_residual_does_not_promote_an_exact_duplicate() {
    let status = status_for(&IngestCommitAccount {
        admission_owns_commit: true,
        observations_committed: 0,
        route_exact_duplicate: true,
        messages_upserted: 3,
        snapshot_messages_upserted: 0,
        claude_observations_committed: 0,
        claude_cursor_advances: 0,
        claude_observation_duplicates: 0,
        claude_cursor_duplicates: 0,
    });

    assert_eq!(status, HostAdmissionStatus::ExactDuplicate);
}

/// Hermes and the other routes that have no admission tally still commit
/// from the messages they themselves upserted.
#[test]
fn message_counted_route_still_commits_from_its_own_upserts() {
    let status = status_for(&IngestCommitAccount {
        admission_owns_commit: false,
        observations_committed: 0,
        route_exact_duplicate: false,
        messages_upserted: 1,
        snapshot_messages_upserted: 0,
        claude_observations_committed: 0,
        claude_cursor_advances: 0,
        claude_observation_duplicates: 0,
        claude_cursor_duplicates: 0,
    });

    assert_eq!(status, HostAdmissionStatus::Committed);
}

#[test]
fn cursor_compaction_response_matches_hook_contract() {
    let value =
        serde_json::to_value(HookRuntimeResultV1::CursorCompact(cursor_compact_skipped())).unwrap();
    assert_eq!(value["action"], "cursor_compact");
    let outcome: tracedecay_agent_hosts::hooks::CursorPreCompactOutcome =
        serde_json::from_value(value).unwrap();
    assert_eq!(outcome.status, "skipped");
    assert_eq!(outcome.reason, "no messages to compact");
    assert_eq!(outcome.summary_nodes_created, 0);
    assert!(outcome.summary_node_ids.is_empty());
    assert_eq!(
        outcome.relation_projection_status,
        tracedecay_lcm::LcmRelationProjectionStatus::NotApplicable
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

/// A profile store is either mounted as the registered profile-session
/// lease or absent; there is no separately supplied "registered" alias. With
/// no lease, profile admission is typed unavailable rather than accepted.
#[tokio::test]
async fn daemon_profile_ingest_without_a_registered_profile_store_is_unavailable() {
    let admission = host_admission_facade(
        None,
        HostAdmissionScope::Profile,
        SessionAuthorities::default(),
    )
    .unwrap()
    .accept_replay("cursor", HostAdmissionScope::Profile);

    assert_eq!(admission.status, HostAdmissionStatus::Unavailable);
    assert_eq!(
        admission.reason_code,
        Some("registered_authority_unavailable")
    );
}

/// The mounted profile-session lease is the admission authority: the same
/// lease that ingests is the one host admission replays through.
#[tokio::test]
async fn daemon_profile_ingest_admits_through_the_mounted_profile_store() {
    let temp = tempfile::TempDir::new().unwrap();
    let fixture = HostAdmissionTestRuntimeV1::profile(temp.path())
        .await
        .unwrap();
    let profile_identity =
        tracedecay_daemon_identity::profile_identity::load_or_create(temp.path()).unwrap();
    let authorities = crate::handlers::mcp_session_authorities(&fixture)
        .with_profile_identity(Some(std::sync::Arc::new(profile_identity)));
    let admission = host_admission_facade(None, HostAdmissionScope::Profile, authorities)
        .unwrap()
        .accept_replay("cursor", HostAdmissionScope::Profile);

    assert_eq!(admission.status, HostAdmissionStatus::AcceptedForReplay);
}

#[tokio::test]
async fn transcript_admission_rejects_unknown_provider_without_echoing_hook_payload() {
    let secret = "hook-secret-unknown-provider";
    let error = ingest_transcript(
        None,
        &HookIngestTranscriptRequestV1 {
            provider: "unknown-provider-v99".to_owned(),
            user_scope: false,
            session_id: None,
            event_json: Some(format!("{{\"raw_source\":\"{secret}\"}}")),
            messages: None,
            max_new_bytes: None,
        },
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
        &HookIngestTranscriptRequestV1 {
            provider: "claude".to_owned(),
            user_scope: false,
            session_id: None,
            event_json: Some(format!("{{\"malformed\":\"{secret}\"}}")),
            messages: None,
            max_new_bytes: None,
        },
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

#[test]
fn claude_postcompact_without_machine_provenance_is_read_only_unavailable() {
    let outcome =
        claude_compact(r#"{"compact_summary":"self-asserted","digest":"self-asserted"}"#).unwrap();

    assert_eq!(
        serde_json::to_value(HookRuntimeResultV1::ClaudeCompact(outcome)).unwrap(),
        json!({
            "action": "claude_compact",
            "status": "unavailable",
            "reason": "claude_postcompact_provenance_unavailable",
            "summary_nodes_created": 0,
            "summary_node_ids": [],
        })
    );
}

#[tokio::test]
async fn capture_registry_owns_every_supported_transcript_route() {
    use super::kernels::TranscriptPayloadRouteV1::{InlineMessages, SourceScan};
    use super::kernels::{TranscriptCaptureContext, transcript_capture_kernel};
    use tracedecay_domain::errors::TraceDecayError;
    use tracedecay_host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
    use tracedecay_sessions::observation::ObservationCancellation;

    let request = HookIngestTranscriptRequestV1 {
        provider: "fixture".to_owned(),
        user_scope: false,
        session_id: None,
        event_json: None,
        messages: None,
        max_new_bytes: None,
    };
    let cancellation = ObservationCancellation::default();
    let facade = HostAdmissionFacade::new(HostAdmissionAuthorities::default());
    let context = TranscriptCaptureContext {
        cg: None,
        request: &request,
        user_scope: false,
        profile_root: None,
        global_db: None,
        session_authorities: SessionAuthorities::default(),
        facade: &facade,
        max_new_bytes: None,
        cancellation: &cancellation,
    };

    let registered = [
        ("claude", true, SourceScan, "missing client profile"),
        ("codex", true, SourceScan, "missing client profile"),
        ("cursor", true, SourceScan, "missing client profile"),
        ("hermes", true, SourceScan, "missing client profile"),
        ("kiro", true, SourceScan, "missing client profile"),
        (
            "codex",
            false,
            SourceScan,
            "project transcript ingest requires a project",
        ),
        (
            "cursor",
            false,
            SourceScan,
            "project transcript ingest requires a project",
        ),
        (
            "hermes",
            false,
            SourceScan,
            "project transcript ingest requires a project",
        ),
        (
            "kiro",
            false,
            SourceScan,
            "project transcript ingest requires a project",
        ),
        (
            "pi",
            false,
            SourceScan,
            "project transcript ingest requires a project",
        ),
        (
            "hermes",
            true,
            InlineMessages,
            "missing required parameter `session_id`",
        ),
        (
            "hermes",
            false,
            InlineMessages,
            "missing required parameter `session_id`",
        ),
    ];
    for (provider, user_scope, route, message) in registered {
        let kernel = transcript_capture_kernel(provider, user_scope, route).unwrap_or_else(|| {
            panic!("no capture kernel registered for ({provider}, {user_scope}, {route:?})")
        });
        match kernel.capture(context.clone()).await {
            Err(TraceDecayError::Config { message: actual }) => assert_eq!(actual, message),
            Err(other) => panic!("expected a config refusal, got {other}"),
            Ok(_) => panic!("capture must refuse an empty context for {provider}"),
        }
    }
    for (provider, user_scope, route) in [
        ("claude", false, SourceScan),
        ("claude", true, InlineMessages),
        ("codex", false, InlineMessages),
        ("unknown-provider-v99", true, SourceScan),
    ] {
        assert!(
            transcript_capture_kernel(provider, user_scope, route).is_none(),
            "unexpected capture kernel registered for ({provider}, {user_scope}, {route:?})"
        );
    }
}

#[test]
fn cursor_hook_event_numbers_accept_numeric_and_string_forms() {
    let event = json!({ "tokens": "42", "message_count": 7 });
    assert_eq!(event_i64(&event, &["tokens"]), Some(42));
    assert_eq!(event_usize(&event, &["message_count"]), Some(7));
}

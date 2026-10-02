use super::*;
use crate::agents::context_scout::ContextScoutDeliveryReceiptHookV1;
use crate::hooks::daemon_ports::daemon_admission_response;
use std::sync::Mutex;
use tracedecay_contracts::context_scout::{
    ContextScoutDeliveryOutcomeV1, ContextScoutDeliveryReceiptV1,
};
use tracedecay_domain::feedback::{FeedbackCycleId, FeedbackResultId, FeedbackScopeV1};
use tracedecay_domain::{CodeGenerationId, CommitId, ManifestDigest, RepositoryId, WorktreeId};
use tracedecay_hooks::{
    HookAdmissionReceiptV1, HookDeliveryFutureV1, HookFeedbackDeliveryOutcomeV1,
    HookGuidanceDispositionV1, HookScopedFeedbackV1,
};
use tracedecay_runtime_core::config::ProfileRoot;

/// Test shim over [`super::native_material`], which now takes the identity
/// fields `prepare_bound_hook` already decoded. These cases start from the raw
/// host payload, so they decode it here exactly as production does once.
fn native_material(
    event_json: &str,
    family: tracedecay_hooks::HookEventFamily,
    observed_at: UtcMicros,
) -> Option<NativeEnvelopeMaterialV1> {
    super::native_material(
        &serde_json::from_str::<NativeIdentityFields>(event_json).unwrap_or_default(),
        family,
        event_json.as_bytes(),
        observed_at,
    )
}

fn scope(worktree: &str) -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new("project.hook-dispatch-test").unwrap(),
        RepositoryId::new("repository.hook-dispatch-test").unwrap(),
        WorktreeId::new(worktree).unwrap(),
        None,
    )
    .unwrap()
}

#[test]
fn hook_binding_uses_exact_resolved_worktree_and_revision_epoch() {
    let first = binding_identity_from_scope(&scope("worktree.first"), 17);
    let second = binding_identity_from_scope(&scope("worktree.second"), 19);

    assert_eq!(first.0, second.0);
    assert_eq!(first.1, second.1);
    assert_ne!(first.2, second.2);
    assert_eq!(first.3, 17);
    assert_eq!(second.3, 19);
}

#[test]
fn every_host_with_a_native_advisory_event_receives_a_daemon_binding() {
    let hosts = [
        NativeHostIdentityV1::ClaudeCode,
        NativeHostIdentityV1::Codex,
        NativeHostIdentityV1::CursorDesktop,
        NativeHostIdentityV1::CursorCloud,
        NativeHostIdentityV1::Hermes,
        NativeHostIdentityV1::Kiro,
        NativeHostIdentityV1::KimiCode,
        NativeHostIdentityV1::OpenCode,
        NativeHostIdentityV1::Cline,
        NativeHostIdentityV1::RooCode,
        NativeHostIdentityV1::Kilo,
    ];
    let families = [
        tracedecay_hooks::HookEventFamily::SessionBoundary,
        tracedecay_hooks::HookEventFamily::PromptBoundary,
        tracedecay_hooks::HookEventFamily::ToolLifecycle,
        tracedecay_hooks::HookEventFamily::SavedEdit,
        tracedecay_hooks::HookEventFamily::TestLifecycle,
    ];

    for host in hosts {
        let has_native = families.into_iter().any(|family| {
            tracedecay_hooks::stock_event_support(host, family)
                == tracedecay_hooks::HookEventSupportV1::Native
        });
        assert_eq!(NATIVE_HOOK_HOSTS.contains(&host), has_native, "{host:?}");
    }
}

#[test]
fn daemon_catchup_disposition_is_not_reclassified_as_unavailable() {
    let response = serde_json::json!({
        "action": "hook_v2_admit",
        "status": "rejected",
        "disposition": "catchup_required",
    });

    assert!(matches!(
        daemon_admission_response(&response).immediate,
        HookImmediateAdmissionV1::CatchupRequired
    ));
}

#[test]
fn admission_window_switches_to_replay_at_twenty_five_milliseconds() {
    let (_, initial) = admission_window_after_elapsed(0).unwrap();
    assert_eq!(
        initial,
        Duration::from_micros(HOOK_ADMISSION_ACK_BUDGET_MICROS)
    );
    let (_, last) = admission_window_after_elapsed(HOOK_ADMISSION_ACK_BUDGET_MICROS - 1).unwrap();
    assert_eq!(last, Duration::from_micros(1));
    assert!(admission_window_after_elapsed(HOOK_ADMISSION_ACK_BUDGET_MICROS).is_none());
}

#[test]
fn daemon_admission_response_rejects_open_or_incoherent_actions() {
    let open = serde_json::json!({
        "action": "hook_v2_admit",
        "status": "accepted",
        "disposition": HookTransportDispositionV1::Accepted,
        "orchestration": null,
        "ready_guidance": null,
        "feedback_notice": null,
        "reason": null,
        "unexpected": true,
    });
    let incoherent = serde_json::json!({
        "action": "hook_v2_admit",
        "status": "accepted",
        "disposition": HookTransportDispositionV1::CatchupRequired,
        "orchestration": null,
        "ready_guidance": null,
        "feedback_notice": null,
        "reason": null,
    });

    for response in [&open, &incoherent] {
        assert!(matches!(
            daemon_admission_response(response).immediate,
            HookImmediateAdmissionV1::Unavailable
        ));
    }
}

#[test]
fn daemon_feedback_notice_survives_into_host_delivery() {
    let notice = tracedecay_application::advisory::AdvisoryHookLookupNoticeV1 {
        scope: FeedbackScopeV1 {
            project_id: ProjectId::new("project.hook-dispatch-test").unwrap(),
            repository_id: RepositoryId::new("repository.hook-dispatch-test").unwrap(),
            worktree_id: WorktreeId::new("worktree.hook-dispatch-test").unwrap(),
            branch_ref: "refs/heads/feature".to_owned(),
            head_commit_id: CommitId::new("a".repeat(40)).unwrap(),
        },
        result_id: FeedbackResultId::new("result.hook-dispatch-test").unwrap(),
        cycle_id: FeedbackCycleId::new("cycle.hook-dispatch-test").unwrap(),
        generation_id: CodeGenerationId::new("generation.hook-dispatch-test").unwrap(),
        generation_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
        returned_findings: 2,
        omitted_findings: 1,
    };
    let current_envelope = HookEventEnvelopeV2 {
        schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
        event_id: [1; 16],
        producer: NativeHostIdentityV1::ClaudeCode,
        protected_session_id: [2; 32],
        project_id: envelope_identity_hash16("project", notice.scope.project_id.as_str()),
        repository_id: envelope_identity_hash16("repository", notice.scope.repository_id.as_str()),
        worktree_id: envelope_identity_hash16("worktree", notice.scope.worktree_id.as_str()),
        worktree_epoch: 1,
        binding_token: [3; 32],
        ordering: tracedecay_hooks::HookOrderingV1::Unknown,
        observed_at: UtcMicros(1),
        event: tracedecay_hooks::HookEventV2::SessionBoundary {
            boundary: tracedecay_hooks::HookBoundaryV1::TurnComplete,
        },
    };
    assert!(notice.matches_envelope(&current_envelope));
    let mut stale_envelope = current_envelope;
    stale_envelope.worktree_id = [9; 16];
    assert!(!notice.matches_envelope(&stale_envelope));
    let response = serde_json::json!({
        "action": "hook_v2_admit",
        "status": "accepted",
        "disposition": "accepted",
        "orchestration": "enqueued",
        "context_scout_address": null,
        "ready_guidance": null,
        "feedback_notice": notice,
        "github_stack_signal_available": false,
    });

    let admitted = daemon_admission_response(&response);
    assert!(matches!(
        admitted.immediate,
        HookImmediateAdmissionV1::Accepted {
            ready_guidance: None,
            ..
        }
    ));
    assert_eq!(admitted.feedback_notice, Some(notice.clone()));

    let rendered = render_host_delivery(None, None, Some(&notice), false)
        .expect("feedback notice serializes")
        .expect("feedback notice renders");
    assert!(rendered.starts_with("TraceDecay feedback ready for authorized lookup: "));
    let encoded = rendered.split_once(": ").unwrap().1;
    assert_eq!(
        serde_json::from_str::<tracedecay_application::advisory::AdvisoryHookLookupNoticeV1>(
            encoded
        )
        .unwrap(),
        notice
    );
}

#[test]
fn context_scout_address_is_rendered_for_the_admitted_host() {
    let address = ContextScoutAddressV1 {
        profile_id: [1; 16],
        provider_id: [2; 16],
        protected_session_id: [3; 32],
        thread_id: [4; 16],
        turn_id: [5; 16],
        agent_id: [6; 16],
        logical_message_id: [7; 16],
        project_id: [8; 16],
    };
    let rendered = render_host_delivery(None, Some(&address), None, false)
        .expect("authorized Scout address serializes")
        .expect("authorized Scout address renders");
    let encoded = rendered
        .strip_prefix("TraceDecay Context Scout address for authorized operations: ")
        .expect("bounded address prefix");
    assert_eq!(
        serde_json::from_str::<ContextScoutAddressV1>(encoded).unwrap(),
        address
    );
}

#[test]
fn github_stack_wakeup_is_content_free() {
    let response = serde_json::json!({
        "action": "hook_v2_admit",
        "status": "accepted",
        "disposition": "accepted",
        "orchestration": "enqueued",
        "context_scout_address": null,
        "ready_guidance": null,
        "feedback_notice": null,
        "github_stack_signal_available": true,
    });
    let admitted = daemon_admission_response(&response);
    assert!(admitted.github_stack_signal_available);

    let rendered = render_host_delivery(None, None, None, true)
        .expect("stack wakeup serialization succeeds")
        .expect("stack wakeup renders");

    assert_eq!(
        rendered,
        "TraceDecay GitHub stack update available for authenticated expansion."
    );
    for protected_detail in ["signal_id", "watermark", "actor", "recipient", "state"] {
        assert!(
            !rendered.contains(protected_detail),
            "hook wakeup must not disclose {protected_detail}",
        );
    }
}

struct RecordingFeedbackDeliveryPort {
    calls: Mutex<usize>,
}

impl AsyncHookFeedbackDeliveryPortV1<tracedecay_application::advisory::AdvisoryHookLookupNoticeV1>
    for RecordingFeedbackDeliveryPort
{
    fn deliver_hook_v2<'a>(
        &'a self,
        _envelope: &'a HookEventEnvelopeV2,
        _feedback: &'a tracedecay_application::advisory::AdvisoryHookLookupNoticeV1,
        _deadline: HookSynchronousDeadlineV1,
    ) -> HookDeliveryFutureV1<'a> {
        Box::pin(async move {
            *self.calls.lock().unwrap() += 1;
            HookFeedbackDeliveryOutcomeV1::Delivered
        })
    }
}

fn sample_notice() -> tracedecay_application::advisory::AdvisoryHookLookupNoticeV1 {
    tracedecay_application::advisory::AdvisoryHookLookupNoticeV1 {
        scope: FeedbackScopeV1 {
            project_id: ProjectId::new("project.hook-dispatch-test").unwrap(),
            repository_id: RepositoryId::new("repository.hook-dispatch-test").unwrap(),
            worktree_id: WorktreeId::new("worktree.hook-dispatch-test").unwrap(),
            branch_ref: "refs/heads/feature".to_owned(),
            head_commit_id: CommitId::new("a".repeat(40)).unwrap(),
        },
        result_id: FeedbackResultId::new("result.hook-dispatch-test").unwrap(),
        cycle_id: FeedbackCycleId::new("cycle.hook-dispatch-test").unwrap(),
        generation_id: CodeGenerationId::new("generation.hook-dispatch-test").unwrap(),
        generation_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
        returned_findings: 2,
        omitted_findings: 1,
    }
}

fn sample_envelope(
    notice: &tracedecay_application::advisory::AdvisoryHookLookupNoticeV1,
) -> HookEventEnvelopeV2 {
    HookEventEnvelopeV2 {
        schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
        event_id: [1; 16],
        producer: NativeHostIdentityV1::ClaudeCode,
        protected_session_id: [2; 32],
        project_id: envelope_identity_hash16("project", notice.scope.project_id.as_str()),
        repository_id: envelope_identity_hash16("repository", notice.scope.repository_id.as_str()),
        worktree_id: envelope_identity_hash16("worktree", notice.scope.worktree_id.as_str()),
        worktree_epoch: 1,
        binding_token: [3; 32],
        ordering: tracedecay_hooks::HookOrderingV1::Unknown,
        observed_at: UtcMicros(1),
        event: tracedecay_hooks::HookEventV2::SessionBoundary {
            boundary: tracedecay_hooks::HookBoundaryV1::TurnComplete,
        },
    }
}

fn sample_receipt(
    immediate: HookImmediateAdmissionStateV1,
    deadline_exceeded: bool,
) -> HookAdmissionReceiptV1 {
    HookAdmissionReceiptV1 {
        event_id: [1; 16],
        protected_session_id: [2; 32],
        configuration_revision: 1,
        completed_at: UtcMicros(10),
        elapsed_micros: 1,
        deadline_exceeded,
        immediate,
        disposition: HookTransportDispositionV1::Accepted,
        guidance: HookGuidanceDispositionV1::NotReady,
    }
}

#[tokio::test]
async fn feedback_notice_never_delivers_after_deadline_or_failed_admission() {
    let notice = sample_notice();
    let envelope = sample_envelope(&notice);
    let port = RecordingFeedbackDeliveryPort {
        calls: Mutex::new(0),
    };
    let rollback = HookFeedbackRollbackSwitchV1 {
        configuration_revision: 1,
    };
    let deadline = HookSynchronousDeadlineV1::after_elapsed(0);

    let accepted = deliver_hook_feedback(
        &envelope,
        &sample_receipt(HookImmediateAdmissionStateV1::Accepted, false),
        rollback,
        Some(notice.clone()),
        deadline,
        &port,
    )
    .await
    .unwrap();
    assert!(accepted.feedback.is_some());
    assert_eq!(*port.calls.lock().unwrap(), 1);

    let after_deadline = deliver_hook_feedback(
        &envelope,
        &sample_receipt(HookImmediateAdmissionStateV1::Accepted, true),
        rollback,
        Some(notice.clone()),
        deadline,
        &port,
    )
    .await
    .unwrap();
    assert!(after_deadline.feedback.is_none());

    let backpressured = deliver_hook_feedback(
        &envelope,
        &sample_receipt(HookImmediateAdmissionStateV1::Backpressured, false),
        rollback,
        Some(notice),
        deadline,
        &port,
    )
    .await
    .unwrap();
    assert!(backpressured.feedback.is_none());
    assert_eq!(*port.calls.lock().unwrap(), 1);
}

#[test]
fn opencode_event_uses_nested_data_identity() {
    let material = native_material(
        r#"{
                "id": "evt-17",
                "type": "session.execution.succeeded",
                "data": {
                    "sessionID": "session-23"
                }
            }"#,
        tracedecay_hooks::HookEventFamily::SessionBoundary,
        UtcMicros(41),
    )
    .unwrap();

    assert_ne!(material.event_id, hash16(b"evt-17"));
    assert_eq!(material.protected_session_id, hash32(b"session-23"));
    assert_eq!(material.effect_receipt_id, None);
    assert_eq!(material.file_id, None);
}

#[tokio::test]
async fn delivery_receipt_withheld_when_ineligible_or_foreign_envelope() {
    let profile_home = tempfile::tempdir().unwrap();
    let profile = ProfileRoot::under_home(profile_home.path());
    let project = tempfile::tempdir().unwrap();
    let notice = sample_notice();
    let mut envelope = sample_envelope(&notice);
    envelope.event_id = [9; 16];
    let envelope_id = [11; 16];
    let receipt = ContextScoutDeliveryReceiptV1 {
        receipt_id: context_scout_delivery_receipt_id(envelope.event_id, envelope_id),
        envelope_id,
        delivered_at: UtcMicros(23),
        outcome: ContextScoutDeliveryOutcomeV1::Attempted,
    };
    let foreign = ContextScoutDeliveryReceiptV1 {
        receipt_id: [3; 16],
        ..receipt.clone()
    };
    let runtime = crate::ports::hook_runtime::crate_test_runtime(profile.clone());
    let port = DaemonDeliveryReceiptPort::new(&runtime, project.path());
    let rollback = HookFeedbackRollbackSwitchV1 {
        configuration_revision: 1,
    };
    let deadline = HookSynchronousDeadlineV1::after_elapsed(0);
    let guard = crate::hooks::TestDaemonHookActionGuard::install([serde_json::json!({
        "action": "hook_v2_delivery_receipt",
        "status": "stored"
    })]);

    let after_deadline = deliver_hook_feedback(
        &envelope,
        &sample_receipt(HookImmediateAdmissionStateV1::Accepted, true),
        rollback,
        Some(ContextScoutDeliveryReceiptHookV1 {
            receipt: receipt.clone(),
        }),
        deadline,
        &port,
    )
    .await
    .unwrap();
    assert!(after_deadline.feedback.is_none());

    let foreign_scope = deliver_hook_feedback(
        &envelope,
        &sample_receipt(HookImmediateAdmissionStateV1::Accepted, false),
        rollback,
        Some(ContextScoutDeliveryReceiptHookV1 { receipt: foreign }),
        deadline,
        &port,
    )
    .await
    .unwrap();
    assert!(foreign_scope.feedback.is_none());

    let accepted = deliver_hook_feedback(
        &envelope,
        &sample_receipt(HookImmediateAdmissionStateV1::Accepted, false),
        rollback,
        Some(ContextScoutDeliveryReceiptHookV1 { receipt }),
        deadline,
        &port,
    )
    .await
    .unwrap();
    assert_eq!(
        accepted.outcome,
        Some(HookFeedbackDeliveryOutcomeV1::Delivered)
    );
    assert_eq!(guard.calls().len(), 1);
}

#[test]
fn opencode_tool_event_uses_call_and_input_path_identity() {
    let material = native_material(
        r#"{
                "tool": "edit",
                "sessionID": "session-29",
                "id": "call-31",
                "input": {
                    "path": "/project/src/main.rs",
                    "oldString": "a",
                    "newString": "b"
                },
                "status": "completed",
                "result": {"content": []}
            }"#,
        tracedecay_hooks::HookEventFamily::SavedEdit,
        UtcMicros(43),
    )
    .unwrap();

    assert_ne!(material.event_id, hash16(b"call-31"));
    assert_eq!(material.protected_session_id, hash32(b"session-29"));
    assert_eq!(material.effect_receipt_id, Some(hash16(b"call-31")));
    assert_eq!(material.file_id, Some(hash16(b"/project/src/main.rs")));
}

#[test]
fn native_path_tool_and_payload_aliases_cannot_change_native_identity() {
    let first = native_material(
        r#"{
                "tool": "edit",
                "sessionID": "session-29",
                "id": "call-31",
                "input": {"path": "/project/first.rs", "oldString": "first", "newString": "payload"},
                "status": "completed"
            }"#,
        tracedecay_hooks::HookEventFamily::SavedEdit,
        UtcMicros(43),
    )
    .unwrap();
    let aliases_changed = native_material(
        r#"{
                "tool": "write",
                "sessionID": "session-29",
                "id": "call-31",
                "input": {"path": "/project/first.rs", "content": "unrelated payload"},
                "status": "completed"
            }"#,
        tracedecay_hooks::HookEventFamily::SavedEdit,
        UtcMicros(43),
    )
    .unwrap();
    let path_changed = native_material(
        r#"{
                "tool": "write",
                "sessionID": "session-29",
                "id": "call-31",
                "input": {"path": "/elsewhere/alias.rs", "content": "unrelated payload"},
                "status": "completed"
            }"#,
        tracedecay_hooks::HookEventFamily::SavedEdit,
        UtcMicros(43),
    )
    .unwrap();
    let different_native_event = native_material(
        r#"{
                "tool": "write",
                "sessionID": "session-29",
                "id": "call-32",
                "input": {},
                "status": "completed"
            }"#,
        tracedecay_hooks::HookEventFamily::SavedEdit,
        UtcMicros(43),
    )
    .unwrap();

    assert_eq!(aliases_changed.event_id, first.event_id);
    assert_eq!(aliases_changed.file_id, first.file_id);
    assert_ne!(path_changed.event_id, first.event_id);
    assert_ne!(path_changed.file_id, first.file_id);
    assert_ne!(different_native_event.event_id, first.event_id);
    assert_ne!(different_native_event.file_id, first.file_id);
}

#[test]
fn retry_identity_and_timestamp_reuse_are_stable() {
    let event = r#"{
        "session_id": "session.retry",
        "turn_id": "turn.retry",
        "hook_event_name": "Stop"
    }"#;
    let family = tracedecay_hooks::HookEventFamily::SessionBoundary;
    let first = native_material(event, family, UtcMicros(10)).unwrap();
    let retry = native_material(event, family, UtcMicros(99)).unwrap();
    assert_eq!(retry.event_id, first.event_id);

    let temporary = tempfile::tempdir().unwrap();
    let host = NativeHostIdentityV1::ClaudeCode;
    let binding = spool_binding(host, [family]);
    let decoded = tracedecay_hooks::decode_native_hook_event(
        host,
        include_bytes!(
            "../../../../../crates/tracedecay-hooks/fixtures/host_events/claude/stop.json"
        ),
    )
    .unwrap();
    let first_envelope = decoded.into_envelope(&binding, first).unwrap();
    assert_eq!(
        append_for_replay(
            temporary.path(),
            host,
            &first_envelope,
            None,
            &binding,
            UtcMicros(10),
        ),
        SpoolAppendOutcomeV1::Accepted,
    );
    let retry_envelope = decoded.into_envelope(&binding, retry).unwrap();
    assert_eq!(
        replay_envelope_if_pending(
            temporary.path(),
            host,
            &binding,
            &retry_envelope,
            UtcMicros(99),
        ),
        PendingEnvelopeV1::Exact(first_envelope),
    );
}

fn spool_binding(
    host: NativeHostIdentityV1,
    families: impl IntoIterator<Item = tracedecay_hooks::HookEventFamily>,
) -> HookScopeBindingV1 {
    HookScopeBindingV1 {
        host,
        project_id: [1; 16],
        repository_id: [2; 16],
        worktree_id: [3; 16],
        worktree_epoch: 4,
        binding_token: [5; 32],
        capabilities: families
            .into_iter()
            .map(|family| tracedecay_hooks::HookCapabilityV1 {
                family,
                support: tracedecay_hooks::stock_event_support(host, family),
            })
            .collect(),
    }
}

#[test]
fn native_identity_is_qualified_by_event_family() {
    let event = r#"{"session_id":"session.shared","id":"native.shared"}"#;
    let session = native_material(
        event,
        tracedecay_hooks::HookEventFamily::SessionBoundary,
        UtcMicros(10),
    )
    .unwrap();
    let tool = native_material(
        event,
        tracedecay_hooks::HookEventFamily::ToolLifecycle,
        UtcMicros(10),
    )
    .unwrap();
    assert_ne!(session.event_id, tool.event_id);
}

#[test]
fn cursor_saved_edits_preserve_exact_file_identity() {
    let first = native_material(
        r#"{
            "session_id":"session.edit",
            "generation_id":"generation.edit",
            "file_path":"/project/src/first.rs",
            "edits":[{}]
        }"#,
        tracedecay_hooks::HookEventFamily::SavedEdit,
        UtcMicros(10),
    )
    .unwrap();
    let second = native_material(
        r#"{
            "session_id":"session.edit",
            "generation_id":"generation.edit",
            "file_path":"/project/src/second.rs",
            "edits":[{}]
        }"#,
        tracedecay_hooks::HookEventFamily::SavedEdit,
        UtcMicros(10),
    )
    .unwrap();
    assert_ne!(first.event_id, second.event_id);
    assert_eq!(first.file_id, Some(hash16(b"/project/src/first.rs")));
    assert_eq!(second.file_id, Some(hash16(b"/project/src/second.rs")));
}

#[test]
fn kimi_rendered_hook_fixture_queues_only_native_session_and_call_identity() {
    let fixture = include_str!(
        "../../../../../tests/fixtures/packaged_host_events/kimi/post-tool-use-edit.json"
    )
    .replace("<SESSION_ID>", "session.kimi.native")
    .replace("<TOOL_CALL_ID>", "call.kimi.native");
    let fields = serde_json::from_str::<NativeIdentityFields>(&fixture).unwrap();
    let material = native_material(
        &fixture,
        tracedecay_hooks::HookEventFamily::SavedEdit,
        UtcMicros(10),
    )
    .unwrap();

    let lifecycle =
        native_context_scout_lifecycle(NativeHostIdentityV1::KimiCode, &fields, material.event_id)
            .unwrap();

    assert_eq!(lifecycle.session_id.as_str(), "session.kimi.native");
    assert_eq!(lifecycle.call_id.as_str(), "call.kimi.native");
    assert_eq!(material.file_id, Some(hash16(b"<SAVED_PATH>")));
}

#[test]
fn hermes_real_tool_fixture_uses_terminal_receipt_identity() {
    let fixture =
        include_str!("../../../../../tests/fixtures/packaged_host_events/hermes/saved-edit.json");
    let material = native_material(
        fixture,
        tracedecay_hooks::HookEventFamily::ToolLifecycle,
        UtcMicros(43),
    )
    .unwrap();

    assert_ne!(material.event_id, hash16(b"<TOOL_CALL_ID>"));
    assert_eq!(material.protected_session_id, hash32(b"<SESSION_ID>"));
    assert_eq!(material.tool_id, Some(material.event_id));
    assert_eq!(material.effect_receipt_id, Some(hash16(b"<TOOL_CALL_ID>")));
    assert_eq!(material.file_id, None);
}

#[test]
fn hermes_adapter_fixture_preserves_native_terminal_identity() {
    let fixture = include_str!(
        "../../../../../tests/fixtures/packaged_host_events/hermes/terminal-receipt.json"
    );
    let material = native_material(
        fixture,
        tracedecay_hooks::HookEventFamily::ToolLifecycle,
        UtcMicros(47),
    )
    .unwrap();

    assert_ne!(material.event_id, hash16(b"<TOOL_CALL_ID>"));
    assert_eq!(material.protected_session_id, hash32(b"<SESSION_ID>"));
    assert_eq!(material.tool_id, Some(material.event_id));
    assert_eq!(material.effect_receipt_id, Some(hash16(b"<TOOL_CALL_ID>")));
    assert_eq!(material.file_id, None);
}

#[test]
fn opencode_rendered_plugin_queues_only_tool_after_lifecycle_identity() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/packaged_host_events/opencode/baseline.json"
    ))
    .unwrap();
    let tool_after = fixture["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["identity"] == "post_tool_use")
        .unwrap()["request"]
        .to_string()
        .replace("<SESSION_ID>", "session.opencode.native")
        .replace("<CALL_ID>", "call.opencode.native");
    let fields = serde_json::from_str::<NativeIdentityFields>(&tool_after).unwrap();
    let material = native_material(
        &tool_after,
        tracedecay_hooks::HookEventFamily::SavedEdit,
        UtcMicros(10),
    )
    .unwrap();
    let lifecycle =
        native_context_scout_lifecycle(NativeHostIdentityV1::OpenCode, &fields, material.event_id)
            .unwrap();
    assert_eq!(lifecycle.session_id.as_str(), "session.opencode.native");
    assert_eq!(lifecycle.call_id.as_str(), "call.opencode.native");

    let decoded = tracedecay_hooks::decode_opencode_plugin_event(
        tracedecay_hooks::OpenCodePluginSurfaceV1::ToolExecuteAfter,
        tool_after.as_bytes(),
    )
    .unwrap();
    let binding = spool_binding(
        NativeHostIdentityV1::OpenCode,
        [tracedecay_hooks::HookEventFamily::SavedEdit],
    );
    let envelope = decoded.into_envelope(&binding, material).unwrap();
    let temporary = tempfile::tempdir().unwrap();
    assert_eq!(
        append_for_replay(
            temporary.path(),
            NativeHostIdentityV1::OpenCode,
            &envelope,
            Some(lifecycle.clone()),
            &binding,
            UtcMicros(10),
        ),
        SpoolAppendOutcomeV1::Accepted,
    );
    let spool_root = temporary
        .path()
        .join("hook-v2-spool")
        .join(NativeHostIdentityV1::OpenCode.hook_key());
    let (mut spool, _) = HookSpoolV1::open(
        spool_root,
        HookSpoolConfigV1::stock(NativeHostIdentityV1::OpenCode),
        UtcMicros(10),
    )
    .unwrap();
    let replayed = spool.claim_replay_batches(UtcMicros(10), 1).unwrap();
    assert_eq!(replayed[0].records[0].native_lifecycle, Some(lifecycle));

    // The execution boundary names a session but no tool call, so it carries
    // no Context Scout lifecycle identity.
    let boundary = fixture["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["identity"] == "stop")
        .unwrap()["request"]
        .to_string();
    let fields = serde_json::from_str::<NativeIdentityFields>(&boundary).unwrap();
    assert!(
        native_context_scout_lifecycle(NativeHostIdentityV1::OpenCode, &fields, [1; 16]).is_none()
    );
}

/// A hook callback appending to an already-prepared spool holds its writer
/// lease for one bounded append. Project open republishing the binding at that
/// moment waits for the peer instead of failing the open with `Busy`.
#[test]
fn binding_publication_waits_for_a_live_callback_holding_the_spool() {
    let profile_home = tempfile::tempdir().unwrap();
    let profile = ProfileRoot::under_home(profile_home.path());
    let project = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        "proj_hook_binding_contention",
    )
    .unwrap();
    let layout = tracedecay_runtime_core::storage::profile_sharded_layout(
        &project_root,
        profile.data_dir(),
        "proj_hook_binding_contention",
    )
    .unwrap();
    fn contention_scope(_: &Path, _: &ProjectId) -> Result<ResolvedScope, String> {
        Ok(scope("worktree.binding-contention"))
    }
    let runtime = HookRuntimeV1 {
        scope_resolver: contention_scope,
        ..crate::ports::hook_runtime::crate_test_runtime(profile.clone())
    };
    publish_daemon_bindings(&runtime, &layout).unwrap();

    let host = NativeHostIdentityV1::ClaudeCode;
    let held = std::time::Duration::from_millis(20);
    let (capture, _) = HookSpoolV1::open(
        tracedecay_hooks::hook_v2_spool_root(&layout.data_root, host),
        HookSpoolConfigV1::stock(host),
        UtcMicros(1),
    )
    .unwrap();
    let delivery = tracedecay_hooks::HookDeliveryReceiptSpoolV1::open(
        tracedecay_hooks::hook_delivery_receipt_spool_root(&layout.data_root, host),
        std::time::Duration::ZERO,
    )
    .unwrap();
    let callback = std::thread::spawn(move || {
        std::thread::sleep(held);
        drop(capture);
        drop(delivery);
    });
    assert!(held < tracedecay_hooks::HOOK_SYNCHRONOUS_BUDGET);

    publish_daemon_bindings(&runtime, &layout)
        .expect("publication waits for the live callback instead of failing busy");
    callback.join().unwrap();
}

/// A hook that fires while the daemon is away spools its event under the
/// published binding. The daemon republishes that binding when it next opens
/// the project, and its replay must still commit the queued event.
#[tokio::test]
async fn events_spooled_before_a_binding_republication_replay_after_it() {
    let profile_home = tempfile::tempdir().unwrap();
    let profile = ProfileRoot::under_home(profile_home.path());
    let project = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        "proj_hook_binding_republication",
    )
    .unwrap();
    let layout = tracedecay_runtime_core::storage::profile_sharded_layout(
        &project_root,
        profile.data_dir(),
        "proj_hook_binding_republication",
    )
    .unwrap();
    fn republication_scope(_: &Path, _: &ProjectId) -> Result<ResolvedScope, String> {
        Ok(scope("worktree.binding-republication"))
    }
    let runtime = HookRuntimeV1 {
        scope_resolver: republication_scope,
        ..crate::ports::hook_runtime::crate_test_runtime(profile.clone())
    };
    let host = NativeHostIdentityV1::ClaudeCode;
    let source = tracedecay_hooks::NativeHookCaptureSourceV1::Host(host);
    let stop = include_str!(
        "../../../../../crates/tracedecay-hooks/fixtures/host_events/claude/stop.json"
    )
    .as_bytes();
    let (project_id, worktree_id) =
        project_and_worktree_locators_for_scope(&scope("worktree.binding-republication"));

    publish_daemon_bindings(&runtime, &layout).unwrap();
    let observed_at = now_utc();
    assert_eq!(
        tracedecay_hooks::capture_native_event_for_replay(
            &layout.data_root,
            worktree_id,
            source,
            stop,
            native_capture_material(source, stop, observed_at).unwrap(),
            observed_at,
            tracedecay_hooks::HOOK_SYNCHRONOUS_BUDGET,
        ),
        tracedecay_hooks::NativeHookCaptureOutcomeV1::Captured,
    );

    std::thread::sleep(Duration::from_millis(2));
    publish_daemon_bindings(&runtime, &layout).unwrap();
    let now = now_utc();
    let binding =
        tracedecay_hooks::published_hook_scope_binding(&layout.data_root, worktree_id, host, now)
            .unwrap();
    let pass = tracedecay_hooks::drain_host_spool_once(
        &tracedecay_hooks::hook_v2_spool_root(&layout.data_root, host),
        HookSpoolConfigV1::stock(host),
        project_id,
        Some(&binding),
        now,
        |_, _| std::future::ready(tracedecay_hooks::HookReplayAdmissionOutcomeV1::Admitted),
    )
    .await
    .unwrap();

    assert_eq!(
        (pass.committed, pass.tombstoned, pass.retained),
        (1, 0, 0),
        "the queued Stop must replay, not be tombstoned as stale: {pass:?}"
    );
}

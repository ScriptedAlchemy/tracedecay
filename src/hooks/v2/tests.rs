use super::*;
use crate::hooks::daemon_ports::daemon_admission_response;
use std::sync::Mutex;
use tracedecay_domain::feedback::{FeedbackCycleId, FeedbackResultId, FeedbackScopeV1};
use tracedecay_domain::{CodeGenerationId, CommitId, ManifestDigest, RepositoryId, WorktreeId};
use tracedecay_hooks::{
    HookAdmissionReceiptV1, HookDeliveryFutureV1, HookFeedbackDeliveryOutcomeV1,
    HookGuidanceDispositionV1,
};

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
        observed_at,
    )
}

fn scope(worktree: &str) -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new("project.hook-v2-test").unwrap(),
        RepositoryId::new("repository.hook-v2-test").unwrap(),
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
fn every_host_with_a_native_pr13_event_receives_a_daemon_binding() {
    let hosts = [
        HookHostV1::ClaudeCode,
        HookHostV1::Codex,
        HookHostV1::CursorDesktop,
        HookHostV1::CursorCloud,
        HookHostV1::Hermes,
        HookHostV1::Kiro,
        HookHostV1::KimiCode,
        HookHostV1::OpenCode,
        HookHostV1::Cline,
        HookHostV1::RooCode,
        HookHostV1::Kilo,
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
        assert_eq!(HOOK_V2_BOUND_HOSTS.contains(&host), has_native, "{host:?}");
    }
}

#[test]
fn daemon_catchup_disposition_is_not_reclassified_as_unavailable() {
    let response = serde_json::json!({
        "action": "hook_v2_admit",
        "status": "rejected",
        "disposition": HookTransportDispositionV1::CatchupRequired,
        "orchestration": null,
        "ready_guidance": null,
        "feedback_notice": null,
        "reason": null,
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
    let notice = crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1 {
        scope: FeedbackScopeV1 {
            project_id: ProjectId::new("project.hook-v2-test").unwrap(),
            repository_id: RepositoryId::new("repository.hook-v2-test").unwrap(),
            worktree_id: WorktreeId::new("worktree.hook-v2-test").unwrap(),
            branch_ref: "refs/heads/feature".to_owned(),
            head_commit_id: CommitId::new("a".repeat(40)).unwrap(),
        },
        result_id: FeedbackResultId::new("result.hook-v2-test").unwrap(),
        cycle_id: FeedbackCycleId::new("cycle.hook-v2-test").unwrap(),
        generation_id: CodeGenerationId::new("generation.hook-v2-test").unwrap(),
        generation_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
        returned_findings: 2,
        omitted_findings: 1,
    };
    let current_envelope = HookEventEnvelopeV2 {
        schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
        event_id: [1; 16],
        producer: HookHostV1::ClaudeCode,
        protected_session_id: [2; 32],
        project_id: domain_hash16(notice.scope.project_id.as_str(), "project"),
        repository_id: domain_hash16(notice.scope.repository_id.as_str(), "repository"),
        worktree_id: domain_hash16(notice.scope.worktree_id.as_str(), "worktree"),
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
        "disposition": HookTransportDispositionV1::Accepted,
        "orchestration": null,
        "ready_guidance": null,
        "feedback_notice": notice,
        "reason": null,
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

    let rendered = render_host_delivery(None, Some(&notice)).unwrap();
    assert!(rendered.starts_with("TraceDecay feedback ready for authorized lookup: "));
    let encoded = rendered.split_once(": ").unwrap().1;
    assert_eq!(
        serde_json::from_str::<crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1>(
            encoded
        )
        .unwrap(),
        notice
    );
}

struct RecordingFeedbackDeliveryPort {
    calls: Mutex<usize>,
}

impl AsyncHookFeedbackDeliveryPortV1<crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1>
    for RecordingFeedbackDeliveryPort
{
    fn deliver_hook_v2<'a>(
        &'a self,
        _envelope: &'a HookEventEnvelopeV2,
        _feedback: &'a crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1,
        _deadline: HookSynchronousDeadlineV1,
    ) -> HookDeliveryFutureV1<'a> {
        Box::pin(async move {
            *self.calls.lock().unwrap() += 1;
            HookFeedbackDeliveryOutcomeV1::Delivered
        })
    }

    fn deliver_legacy<'a>(
        &'a self,
        _envelope: &'a HookEventEnvelopeV2,
        _feedback: &'a crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1,
        _deadline: HookSynchronousDeadlineV1,
    ) -> HookDeliveryFutureV1<'a> {
        Box::pin(async { HookFeedbackDeliveryOutcomeV1::Unavailable })
    }
}

fn sample_notice() -> crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1 {
    crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1 {
        scope: FeedbackScopeV1 {
            project_id: ProjectId::new("project.hook-v2-test").unwrap(),
            repository_id: RepositoryId::new("repository.hook-v2-test").unwrap(),
            worktree_id: WorktreeId::new("worktree.hook-v2-test").unwrap(),
            branch_ref: "refs/heads/feature".to_owned(),
            head_commit_id: CommitId::new("a".repeat(40)).unwrap(),
        },
        result_id: FeedbackResultId::new("result.hook-v2-test").unwrap(),
        cycle_id: FeedbackCycleId::new("cycle.hook-v2-test").unwrap(),
        generation_id: CodeGenerationId::new("generation.hook-v2-test").unwrap(),
        generation_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
        returned_findings: 2,
        omitted_findings: 1,
    }
}

fn sample_envelope(
    notice: &crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1,
) -> HookEventEnvelopeV2 {
    HookEventEnvelopeV2 {
        schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
        event_id: [1; 16],
        producer: HookHostV1::ClaudeCode,
        protected_session_id: [2; 32],
        project_id: domain_hash16(notice.scope.project_id.as_str(), "project"),
        repository_id: domain_hash16(notice.scope.repository_id.as_str(), "repository"),
        worktree_id: domain_hash16(notice.scope.worktree_id.as_str(), "worktree"),
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
        route: HookFeedbackDeliveryRouteV1::HookV2,
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

#[tokio::test]
async fn host_delivery_and_explicit_feedback_use_typed_daemon_commits() {
    let project = tempfile::tempdir().unwrap();
    let notice = sample_notice();
    let mut envelope = sample_envelope(&notice);
    envelope.event_id = [7; 16];
    let envelope_id = [22; 16];
    let receipt = crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1 {
        receipt_id: context_scout_delivery_receipt_id(envelope.event_id, envelope_id),
        envelope_id,
        delivered_at: UtcMicros(23),
        outcome: crate::agents::context_scout_v2::ContextScoutOutcomeV1::Displayed,
    };
    let feedback = crate::agents::context_scout_v2::ContextScoutFeedbackV1 {
        receipt_id: receipt.receipt_id,
        kind: crate::agents::context_scout_v2::ContextScoutFeedbackKindV1::ExplicitlyAccepted,
    };
    let commit = ContextScoutFeedbackCommitV1 {
        receipt: receipt.clone(),
        feedback,
    };
    let guard = crate::hooks::TestDaemonHookActionGuard::install([
        serde_json::json!({ "status": "stored" }),
        serde_json::json!({ "status": "duplicate" }),
        serde_json::json!({ "status": "stored" }),
    ]);
    let admission = sample_receipt(HookImmediateAdmissionStateV1::Accepted, false);
    let rollback = HookFeedbackRollbackSwitchV1 {
        configuration_revision: 1,
        route: HookFeedbackDeliveryRouteV1::HookV2,
    };
    let deadline = HookSynchronousDeadlineV1::after_elapsed(0);

    let recorded = deliver_hook_feedback(
        &envelope,
        &admission,
        rollback,
        Some(receipt.clone()),
        deadline,
        &DaemonDeliveryReceiptPort::new(project.path()),
    )
    .await
    .unwrap();
    assert_eq!(
        recorded.outcome,
        Some(HookFeedbackDeliveryOutcomeV1::Delivered)
    );

    let committed = deliver_hook_feedback(
        &envelope,
        &admission,
        rollback,
        Some(commit.clone()),
        deadline,
        &DaemonContextScoutFeedbackPort::new(project.path()),
    )
    .await
    .unwrap();
    assert_eq!(
        committed.outcome,
        Some(HookFeedbackDeliveryOutcomeV1::Duplicate)
    );

    let delivered = deliver_hook_feedback(
        &envelope,
        &admission,
        rollback,
        Some(notice.clone()),
        deadline,
        &DaemonFeedbackNoticeDeliveryPort::new(project.path()),
    )
    .await
    .unwrap();
    assert_eq!(
        delivered.outcome,
        Some(HookFeedbackDeliveryOutcomeV1::Delivered)
    );
    assert_eq!(delivered.feedback.as_ref(), Some(&notice));

    let calls = guard.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].0.as_deref(), Some(project.path()));
    assert_eq!(calls[0].1["action"], "hook_v2_delivery_receipt");
    assert_eq!(
        serde_json::from_value::<crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1>(
            calls[0].1["receipt"].clone()
        )
        .unwrap(),
        receipt
    );
    assert_eq!(calls[1].1["action"], "hook_v2_feedback");
    assert_eq!(
        serde_json::from_value::<crate::agents::context_scout_v2::ContextScoutFeedbackV1>(
            calls[1].1["feedback"].clone()
        )
        .unwrap(),
        commit.feedback
    );
    assert_eq!(calls[2].1["action"], "hook_v2_feedback_notice_delivery");
    assert_eq!(
        serde_json::from_value::<crate::application::advisory::Pr13AdvisoryHookLookupNoticeV1>(
            calls[2].1["feedback_notice"].clone()
        )
        .unwrap(),
        notice
    );
}

#[tokio::test]
async fn scout_receipt_and_feedback_helpers_delegate_to_daemon_ports() {
    let project = tempfile::tempdir().unwrap();
    let receipt = crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1 {
        receipt_id: [21; 16],
        envelope_id: [22; 16],
        delivered_at: UtcMicros(23),
        outcome: crate::agents::context_scout_v2::ContextScoutOutcomeV1::Displayed,
    };
    let feedback = crate::agents::context_scout_v2::ContextScoutFeedbackV1 {
        receipt_id: receipt.receipt_id,
        kind: crate::agents::context_scout_v2::ContextScoutFeedbackKindV1::ExplicitlyAccepted,
    };
    let guard = crate::hooks::TestDaemonHookActionGuard::install([
        serde_json::json!({ "status": "stored" }),
        serde_json::json!({ "status": "duplicate" }),
    ]);

    assert!(record_context_scout_delivery(project.path(), &receipt).await);
    assert!(commit_context_scout_feedback(project.path(), &receipt, feedback).await);

    let calls = guard.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].1["action"], "hook_v2_delivery_receipt");
    assert_eq!(calls[1].1["action"], "hook_v2_feedback");
}

#[test]
fn opencode_event_uses_nested_properties_identity() {
    let material = native_material(
        r#"{
                "id": "event-17",
                "properties": {
                    "sessionID": "session-23",
                    "file": "/project/src/lib.rs"
                }
            }"#,
        tracedecay_hooks::HookEventFamily::SavedEdit,
        UtcMicros(41),
    )
    .unwrap();

    assert_eq!(material.event_id, hash16(b"event-17"));
    assert_eq!(material.protected_session_id, hash32(b"session-23"));
    assert_eq!(material.file_id, Some(hash16(b"event-17")));
}

fn opencode_lsp_fixture_event() -> (serde_json::Value, String) {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/packaged_host_events/opencode/baseline.json"
    ))
    .unwrap();
    let event = fixture["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["identity"] == "lsp_updated")
        .unwrap()["request"]
        .clone();
    let event_json = serde_json::to_string(&event).unwrap();
    (event, event_json)
}

#[tokio::test]
async fn opencode_lsp_updated_uses_project_scoped_daemon_action() {
    let project = tempfile::tempdir().unwrap();
    let (event, event_json) = opencode_lsp_fixture_event();
    let guard = crate::hooks::TestDaemonHookActionGuard::install([serde_json::json!({
        "action": "opencode_lsp_updated",
        "status": "accepted",
    })]);

    let dispatch = dispatch_opencode_lsp_updated(&event_json, project.path(), None).await;

    assert!(matches!(
        dispatch,
        HookV2Dispatch::Handled {
            guidance: None,
            disposition: HookTransportDispositionV1::Accepted,
        }
    ));
    let calls = guard.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0.as_deref(), Some(project.path()));
    assert_eq!(calls[0].1["action"], "opencode_lsp_updated");
    assert_eq!(calls[0].1["event"], event);
}

#[tokio::test]
async fn opencode_lsp_updated_rejects_non_accepted_daemon_status() {
    let project = tempfile::tempdir().unwrap();
    let (_event, event_json) = opencode_lsp_fixture_event();
    let _guard = crate::hooks::TestDaemonHookActionGuard::install([serde_json::json!({
        "action": "opencode_lsp_updated",
        "status": "rejected",
    })]);

    let dispatch = dispatch_opencode_lsp_updated(&event_json, project.path(), None).await;
    assert!(matches!(
        dispatch,
        HookV2Dispatch::Unavailable(HookTransportDispositionV1::CatchupRequired)
    ));
}

#[tokio::test]
async fn delivery_receipt_withheld_when_ineligible_or_foreign_envelope() {
    let project = tempfile::tempdir().unwrap();
    let notice = sample_notice();
    let mut envelope = sample_envelope(&notice);
    envelope.event_id = [9; 16];
    let envelope_id = [11; 16];
    let receipt = crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1 {
        receipt_id: context_scout_delivery_receipt_id(envelope.event_id, envelope_id),
        envelope_id,
        delivered_at: UtcMicros(23),
        outcome: crate::agents::context_scout_v2::ContextScoutOutcomeV1::Attempted,
    };
    let foreign = crate::agents::context_scout_v2::ContextScoutDeliveryReceiptV1 {
        receipt_id: [3; 16],
        ..receipt.clone()
    };
    let port = DaemonDeliveryReceiptPort::new(project.path());
    let rollback = HookFeedbackRollbackSwitchV1 {
        configuration_revision: 1,
        route: HookFeedbackDeliveryRouteV1::HookV2,
    };
    let deadline = HookSynchronousDeadlineV1::after_elapsed(0);
    let guard = crate::hooks::TestDaemonHookActionGuard::install([serde_json::json!({
        "status": "stored"
    })]);

    let after_deadline = deliver_hook_feedback(
        &envelope,
        &sample_receipt(HookImmediateAdmissionStateV1::Accepted, true),
        rollback,
        Some(receipt.clone()),
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
        Some(foreign),
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
        Some(receipt),
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
fn opencode_tool_event_uses_nested_input_and_output_identity() {
    let material = native_material(
        r#"{
                "input": {
                    "tool": "apply_patch",
                    "sessionID": "session-29",
                    "callID": "call-31"
                },
                "output": {
                    "metadata": {
                        "files": [{"filePath": "/project/src/main.rs"}]
                    }
                }
            }"#,
        tracedecay_hooks::HookEventFamily::SavedEdit,
        UtcMicros(43),
    )
    .unwrap();

    assert_eq!(material.event_id, hash16(b"call-31"));
    assert_eq!(material.protected_session_id, hash32(b"session-29"));
    assert_eq!(material.effect_receipt_id, Some(hash16(b"call-31")));
    assert_eq!(material.file_id, Some(hash16(b"call-31")));
}

#[test]
fn native_path_tool_and_payload_aliases_cannot_change_native_identity() {
    let first = native_material(
        r#"{
                "input": {
                    "tool": "apply_patch",
                    "sessionID": "session-29",
                    "callID": "call-31",
                    "args": {"patchText": "first payload"}
                },
                "output": {
                    "metadata": {
                        "files": [{"filePath": "/project/first.rs"}]
                    }
                }
            }"#,
        tracedecay_hooks::HookEventFamily::SavedEdit,
        UtcMicros(43),
    )
    .unwrap();
    let aliases_changed = native_material(
        r#"{
                "input": {
                    "tool": "write",
                    "sessionID": "session-29",
                    "callID": "call-31",
                    "args": {"patchText": "unrelated payload"}
                },
                "output": {
                    "metadata": {
                        "files": [{"filePath": "/elsewhere/alias.rs"}]
                    }
                }
            }"#,
        tracedecay_hooks::HookEventFamily::SavedEdit,
        UtcMicros(43),
    )
    .unwrap();
    let different_native_event = native_material(
        r#"{
                "input": {
                    "tool": "write",
                    "sessionID": "session-29",
                    "callID": "call-32"
                }
            }"#,
        tracedecay_hooks::HookEventFamily::SavedEdit,
        UtcMicros(43),
    )
    .unwrap();

    assert_eq!(aliases_changed.event_id, first.event_id);
    assert_eq!(aliases_changed.file_id, first.file_id);
    assert_ne!(different_native_event.event_id, first.event_id);
    assert_ne!(different_native_event.file_id, first.file_id);
}

#[test]
fn kimi_rendered_hook_fixture_queues_only_native_session_and_call_identity() {
    let fixture =
        include_str!("../../../tests/fixtures/packaged_host_events/kimi/post-tool-use-edit.json")
            .replace("<SESSION_ID>", "session.kimi.native")
            .replace("<TOOL_CALL_ID>", "call.kimi.native");
    let fields = serde_json::from_str::<NativeIdentityFields>(&fixture).unwrap();

    let lifecycle = native_context_scout_lifecycle(HookHostV1::KimiCode, &fields).unwrap();

    assert_eq!(lifecycle.session_id.as_str(), "session.kimi.native");
    assert_eq!(lifecycle.call_id.as_str(), "call.kimi.native");
}

#[test]
fn hermes_real_tool_fixture_uses_terminal_receipt_identity() {
    let fixture =
        include_str!("../../../tests/fixtures/packaged_host_events/hermes/saved-edit.json");
    let material = native_material(
        fixture,
        tracedecay_hooks::HookEventFamily::ToolLifecycle,
        UtcMicros(43),
    )
    .unwrap();

    assert_eq!(material.event_id, hash16(b"<TOOL_CALL_ID>"));
    assert_eq!(material.protected_session_id, hash32(b"<SESSION_ID>"));
    assert_eq!(material.tool_id, Some(hash16(b"<TOOL_CALL_ID>")));
    assert_eq!(material.effect_receipt_id, Some(hash16(b"<TOOL_CALL_ID>")));
    assert_eq!(material.file_id, None);
}

#[test]
fn hermes_adapter_fixture_preserves_native_terminal_identity() {
    let fixture =
        include_str!("../../../tests/fixtures/packaged_host_events/hermes/terminal-receipt.json");
    let material = native_material(
        fixture,
        tracedecay_hooks::HookEventFamily::ToolLifecycle,
        UtcMicros(47),
    )
    .unwrap();

    assert_eq!(material.event_id, hash16(b"<TOOL_CALL_ID>"));
    assert_eq!(material.protected_session_id, hash32(b"<SESSION_ID>"));
    assert_eq!(material.tool_id, Some(hash16(b"<TOOL_CALL_ID>")));
    assert_eq!(material.effect_receipt_id, Some(hash16(b"<TOOL_CALL_ID>")));
    assert_eq!(material.file_id, None);
}

#[test]
fn opencode_rendered_plugin_queues_only_tool_after_lifecycle_identity() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/packaged_host_events/opencode/baseline.json"
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
    let lifecycle = native_context_scout_lifecycle(HookHostV1::OpenCode, &fields).unwrap();
    assert_eq!(lifecycle.session_id.as_str(), "session.opencode.native");
    assert_eq!(lifecycle.call_id.as_str(), "call.opencode.native");

    let file_edit = fixture["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["identity"] == "saved_edit")
        .unwrap()["request"]
        .to_string();
    let fields = serde_json::from_str::<NativeIdentityFields>(&file_edit).unwrap();
    assert!(native_context_scout_lifecycle(HookHostV1::OpenCode, &fields).is_none());
}

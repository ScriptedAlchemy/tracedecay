use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};

fn native_request(marker: u8) -> HookOrchestrationRequestV1 {
    let host = tracedecay_domain::NativeHostIdentityV1::KimiCode;
    let binding = HookScopeBindingV1 {
        host,
        project_id: [marker; 16],
        repository_id: [marker + 1; 16],
        worktree_id: [marker + 2; 16],
        worktree_epoch: 1,
        binding_token: [marker; 32],
        capabilities: vec![tracedecay_hooks::HookCapabilityV1 {
            family: tracedecay_hooks::HookEventFamily::SavedEdit,
            support: tracedecay_hooks::stock_event_support(
                host,
                tracedecay_hooks::HookEventFamily::SavedEdit,
            ),
        }],
    };
    let envelope = tracedecay_hooks::decode_native_hook_event(
        host,
        include_bytes!(
            "../../../../tracedecay-hooks/fixtures/host_events/kimi/post-tool-use-edit.json"
        ),
    )
    .unwrap()
    .into_envelope(
        &binding,
        tracedecay_hooks::NativeEnvelopeMaterialV1 {
            event_id: [marker + 3; 16],
            protected_session_id: [marker; 32],
            observed_at: UtcMicros(1),
            tool_id: None,
            effect_receipt_id: Some([marker + 4; 16]),
            file_id: Some([marker + 5; 16]),
            changed_range_count: 1,
        },
    )
    .unwrap();
    let lifecycle = ContextScoutLifecycleAddressV1 {
        profile_id: tracedecay_domain::UserProfileId::new("profile.scout-request").unwrap(),
        provider_id: tracedecay_domain::ProviderId::new("kimi").unwrap(),
        project_id: ProjectId::new("project.scout-request").unwrap(),
        worktree_id: tracedecay_domain::WorktreeId::new("worktree.scout-request").unwrap(),
        session_id: tracedecay_domain::SessionId::new("session.scout-request").unwrap(),
        thread_id: tracedecay_domain::ThreadId::new("thread.scout-request").unwrap(),
        turn_id: tracedecay_domain::TurnId::new("turn.scout-request").unwrap(),
        agent_id: tracedecay_domain::AgentInstanceId::new("agent.scout-request").unwrap(),
        logical_message_id: tracedecay_domain::MessageId::new("message.scout-request").unwrap(),
    };
    HookOrchestrationRequestV1::from_envelope(envelope, &binding, Some(lifecycle), 1, false)
        .unwrap()
}

fn deadline() -> MonotonicDeadline {
    MonotonicDeadline::at(std::time::Instant::now() + std::time::Duration::from_secs(5))
}

#[tokio::test]
async fn explicit_request_supersedes_saved_edit_and_duplicate_requests_join() {
    let request = native_request(171);
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let runtime = BoundedHookOrchestratorV1::new(1, {
        let release = Arc::clone(&release);
        let calls = Arc::clone(&calls);
        move |request, cancellation| {
            let release = Arc::clone(&release);
            let started = started.clone();
            calls.fetch_add(1, Ordering::SeqCst);
            async move {
                started.send(request.trigger).unwrap();
                if request.trigger == HookOrchestrationTriggerV1::SavedEdit {
                    cancellation.cancelled().await;
                    HookOrchestrationWorkOutcomeV1::RetryableFailure
                } else {
                    release.notified().await;
                    HookOrchestrationWorkOutcomeV1::Completed
                }
            }
        }
    })
    .unwrap();
    assert!(register_hook_orchestration_runtime(
        [171; 16], [173; 16], &runtime
    ));
    assert_eq!(
        runtime.admit(request.clone()),
        HookOrchestrationAdmissionV1::Enqueued
    );
    assert_eq!(
        starts.recv().await,
        Some(HookOrchestrationTriggerV1::SavedEdit)
    );
    let run = || {
        run_registered_context_scout_request(
            request.hook.clone(),
            request.lifecycle.clone().unwrap(),
            1,
            canonical_sha256(&"explicit-request").unwrap(),
            deadline(),
            CancellationToken::new(),
        )
    };
    let (first, retry, ()) = tokio::join!(run(), run(), async {
        assert_eq!(
            starts.recv().await,
            Some(HookOrchestrationTriggerV1::Explicit)
        );
        release.notify_one();
    });
    assert!(first.is_ok());
    assert!(retry.is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    unregister_hook_orchestration_runtime([171; 16], [173; 16], &runtime);
    assert!(runtime.shutdown().await);
}

#[tokio::test]
async fn explicit_request_failure_cancellation_and_deadline_are_not_completion() {
    let request = native_request(181);
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let runtime = BoundedHookOrchestratorV1::new(1, move |_, cancellation| {
        let started = started.clone();
        async move {
            started.send(()).unwrap();
            cancellation.cancelled().await;
            HookOrchestrationWorkOutcomeV1::RetryableFailure
        }
    })
    .unwrap();
    assert!(register_hook_orchestration_runtime(
        [181; 16], [183; 16], &runtime
    ));
    let cancellation = CancellationToken::new();
    let (result, ()) = tokio::join!(
        run_registered_context_scout_request(
            request.hook.clone(),
            request.lifecycle.clone().unwrap(),
            1,
            canonical_sha256(&"cancel-request").unwrap(),
            deadline(),
            cancellation.clone(),
        ),
        async {
            starts.recv().await.unwrap();
            cancellation.cancel();
        }
    );
    assert!(matches!(result, Err(ApplicationProblem::Cancelled { .. })));
    assert!(matches!(
        run_registered_context_scout_request(
            request.hook.clone(),
            request.lifecycle.clone().unwrap(),
            1,
            canonical_sha256(&"expired-request").unwrap(),
            MonotonicDeadline::at(std::time::Instant::now()),
            CancellationToken::new(),
        )
        .await,
        Err(ApplicationProblem::TimedOut { .. })
    ));
    unregister_hook_orchestration_runtime([181; 16], [183; 16], &runtime);
    assert!(runtime.shutdown().await);

    let failed = BoundedHookOrchestratorV1::new(1, |_, _| async {
        HookOrchestrationWorkOutcomeV1::RetryableFailure
    })
    .unwrap();
    assert!(register_hook_orchestration_runtime(
        [181; 16], [183; 16], &failed
    ));
    assert!(matches!(
        run_registered_context_scout_request(
            request.hook,
            request.lifecycle.unwrap(),
            1,
            canonical_sha256(&"failed-request").unwrap(),
            deadline(),
            CancellationToken::new(),
        )
        .await,
        Err(ApplicationProblem::Unavailable { .. })
    ));
    unregister_hook_orchestration_runtime([181; 16], [183; 16], &failed);
    assert!(failed.shutdown().await);
}

#[tokio::test]
async fn explicit_request_preserves_a_newer_native_event() {
    let older = native_request(191);
    let mut newer = older.clone();
    let mut envelope = newer.hook.envelope().clone();
    envelope.event_id = [197; 16];
    let host = envelope.producer;
    let binding = HookScopeBindingV1 {
        host,
        project_id: envelope.project_id,
        repository_id: envelope.repository_id,
        worktree_id: envelope.worktree_id,
        worktree_epoch: envelope.worktree_epoch,
        binding_token: envelope.binding_token,
        capabilities: vec![tracedecay_hooks::HookCapabilityV1 {
            family: tracedecay_hooks::HookEventFamily::SavedEdit,
            support: tracedecay_hooks::stock_event_support(
                host,
                tracedecay_hooks::HookEventFamily::SavedEdit,
            ),
        }],
    };
    newer.hook = AdmittedContextScoutHookV1::new(envelope, &binding).unwrap();
    let (started, start) = tokio::sync::oneshot::channel();
    let started = Arc::new(StdMutex::new(Some(started)));
    let release = Arc::new(tokio::sync::Notify::new());
    let runtime = BoundedHookOrchestratorV1::new(1, {
        let release = Arc::clone(&release);
        move |_, cancellation| {
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            async move {
                started
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap()
                    .send(cancellation.clone())
                    .unwrap();
                release.notified().await;
                HookOrchestrationWorkOutcomeV1::Completed
            }
        }
    })
    .unwrap();
    assert!(register_hook_orchestration_runtime(
        [191; 16], [193; 16], &runtime
    ));
    assert_eq!(runtime.admit(newer), HookOrchestrationAdmissionV1::Enqueued);
    let native_cancellation = start.await.unwrap();
    assert!(matches!(
        run_registered_context_scout_request(
            older.hook,
            older.lifecycle.unwrap(),
            1,
            canonical_sha256(&"older-request").unwrap(),
            deadline(),
            CancellationToken::new(),
        )
        .await,
        Err(ApplicationProblem::Unavailable { .. })
    ));
    assert!(!native_cancellation.is_cancelled());
    release.notify_one();
    unregister_hook_orchestration_runtime([191; 16], [193; 16], &runtime);
    assert!(runtime.shutdown().await);
}

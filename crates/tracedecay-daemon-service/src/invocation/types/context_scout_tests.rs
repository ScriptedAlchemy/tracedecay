use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tracedecay_hooks::{HookEventFamily, HookEventSupportV1};
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

/// A cycle's terminal is the host's cue to send the session's next boundary,
/// so the cycle must release its permit before it reports that terminal.
#[tokio::test]
async fn settled_cycle_admits_the_sessions_next_boundary_from_its_terminal() {
    let mut first = native_request(221);
    let mut envelope = first.hook.envelope().clone();
    envelope.event_id = [229; 16];
    let binding = HookScopeBindingV1 {
        host: envelope.producer,
        project_id: envelope.project_id,
        repository_id: envelope.repository_id,
        worktree_id: envelope.worktree_id,
        worktree_epoch: envelope.worktree_epoch,
        binding_token: envelope.binding_token,
        capabilities: vec![tracedecay_hooks::HookCapabilityV1 {
            family: tracedecay_hooks::HookEventFamily::SavedEdit,
            support: tracedecay_hooks::stock_event_support(
                envelope.producer,
                tracedecay_hooks::HookEventFamily::SavedEdit,
            ),
        }],
    };
    let next =
        HookOrchestrationRequestV1::from_envelope(envelope, &binding, None, 1, false).unwrap();
    let runtime = BoundedHookOrchestratorV1::new(1, |_, _| async {
        HookOrchestrationWorkOutcomeV1::Completed
    })
    .unwrap();
    let (admitted, admission) = tokio::sync::oneshot::channel();
    let pending = StdMutex::new(Some((next, admitted)));
    let owner = Arc::downgrade(&runtime);
    first.completion = Some(Arc::new(move || {
        if let Some((next, admitted)) = pending.lock().unwrap().take()
            && let Some(runtime) = owner.upgrade()
        {
            let _ = admitted.send(runtime.admit(next));
        }
    }));

    assert_eq!(runtime.admit(first), HookOrchestrationAdmissionV1::Enqueued);
    assert_eq!(
        admission.await.unwrap(),
        HookOrchestrationAdmissionV1::Enqueued,
        "a settled cycle must not backpressure the boundary its terminal invites"
    );
    assert!(runtime.shutdown().await);
}

/// A boundary of a session with no Context Scout lifecycle runs once and
/// settles. It reruns only when a later hook of that session carries a
/// lifecycle, and a newer boundary of the session replaces it.
#[tokio::test]
async fn lifecycle_less_boundary_settles_and_resumes_on_its_sessions_lifecycle() {
    let mut boundary = native_request(201);
    let lifecycle = boundary.lifecycle.take().unwrap();
    let envelope = boundary.hook.envelope().clone();
    let binding = HookScopeBindingV1 {
        host: envelope.producer,
        project_id: envelope.project_id,
        repository_id: envelope.repository_id,
        worktree_id: envelope.worktree_id,
        worktree_epoch: envelope.worktree_epoch,
        binding_token: envelope.binding_token,
        capabilities: vec![tracedecay_hooks::HookCapabilityV1 {
            family: tracedecay_hooks::HookEventFamily::SavedEdit,
            support: tracedecay_hooks::stock_event_support(
                envelope.producer,
                tracedecay_hooks::HookEventFamily::SavedEdit,
            ),
        }],
    };
    let (ran, mut runs) = tokio::sync::mpsc::unbounded_channel();
    let runtime = BoundedHookOrchestratorV1::new(1, move |request, _| {
        let ran = ran.clone();
        async move {
            let has_lifecycle = request.lifecycle.is_some();
            ran.send((request.hook.envelope().event_id, has_lifecycle))
                .unwrap();
            if has_lifecycle {
                HookOrchestrationWorkOutcomeV1::Completed
            } else {
                HookOrchestrationWorkOutcomeV1::AwaitingLifecycle
            }
        }
    })
    .unwrap();
    assert!(register_hook_orchestration_runtime(
        envelope.project_id,
        envelope.worktree_id,
        &runtime
    ));
    let saved_edit = |event_id: u8| {
        let mut saved_edit = envelope.clone();
        saved_edit.event_id = [event_id; 16];
        let (completed, completion) = tokio::sync::oneshot::channel();
        let completed = StdMutex::new(Some(completed));
        assert_eq!(
            admit_registered_hook_orchestration(
                saved_edit,
                binding.clone(),
                None,
                1,
                false,
                Some(Arc::new(move || {
                    if let Some(completed) = completed.lock().unwrap().take() {
                        let _ = completed.send(());
                    }
                })),
            ),
            HookOrchestrationAdmissionV1::Enqueued
        );
        completion
    };
    let session_hook = |event_id: u8, lifecycle: Option<ContextScoutLifecycleAddressV1>| {
        let mut session_start = envelope.clone();
        session_start.event_id = [event_id; 16];
        session_start.event = HookEventV2::SessionBoundary {
            boundary: HookBoundaryV1::Start,
        };
        assert_eq!(
            admit_registered_hook_orchestration(
                session_start,
                binding.clone(),
                lifecycle,
                1,
                false,
                None
            ),
            HookOrchestrationAdmissionV1::UnsupportedTrigger
        );
    };

    let first = saved_edit(210);
    assert_eq!(runs.recv().await, Some(([210; 16], false)));
    first
        .await
        .expect("a boundary awaiting a lifecycle settles its owed work");

    session_hook(211, None);
    session_hook(212, Some(lifecycle.clone()));
    assert_eq!(
        runs.recv().await,
        Some(([210; 16], true)),
        "only the session's hook that carries a lifecycle resumes the parked boundary"
    );
    // The resumed cycle reports no terminal. Until it settles, the session's
    // next boundary supersedes it and inherits its lifecycle.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while runtime.active_tasks() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the resumed cycle settles");
    session_hook(213, Some(lifecycle.clone()));
    let second = saved_edit(214);
    assert_eq!(
        runs.recv().await,
        Some(([214; 16], false)),
        "a resumed boundary is no longer parked"
    );
    second.await.unwrap();

    let third = saved_edit(215);
    assert_eq!(runs.recv().await, Some(([215; 16], false)));
    third.await.unwrap();
    session_hook(216, Some(lifecycle));
    assert_eq!(
        runs.recv().await,
        Some(([215; 16], true)),
        "the newer boundary replaced the older parked one"
    );

    unregister_hook_orchestration_runtime(envelope.project_id, envelope.worktree_id, &runtime);
    assert!(runtime.shutdown().await);
    assert!(runs.try_recv().is_err());
}

#[test]
fn admission_without_a_tokio_runtime_runs_the_cycle_natively() {
    use tracedecay_runtime_core::native_runtime;
    let (ran, mut runs) = tokio::sync::mpsc::unbounded_channel();
    let runtime = BoundedHookOrchestratorV1::new(1, move |_, _| {
        let ran = ran.clone();
        async move {
            ran.send(tokio::runtime::Handle::try_current().is_ok())
                .unwrap();
            HookOrchestrationWorkOutcomeV1::Completed
        }
    })
    .unwrap();
    native_runtime::block_on(async {
        assert!(tokio::runtime::Handle::try_current().is_err());
        assert_eq!(
            runtime.admit(native_request(191)),
            HookOrchestrationAdmissionV1::Enqueued
        );
        let entered_tokio = native_runtime::timeout(std::time::Duration::from_secs(5), runs.recv())
            .await
            .expect("the cycle runs without a Tokio runtime")
            .expect("the cycle reports its context");
        assert!(!entered_tokio, "the cycle must run on the native runtime");
        assert!(runtime.shutdown().await);
    })
    .expect("native runtime");
}

/// Prompt and completed-tool boundaries run the session intent pass in its
/// own lane. A newer boundary supersedes only older intent work of the
/// session, a stop boundary owes its feedback cycle and an intent pass, intent
/// work never holds the permit the feedback producer needs, and an intent
/// boundary that carries the session's lifecycle resumes a parked feedback
/// boundary exactly as any other lifecycle-bearing hook does.
#[tokio::test]
async fn intent_boundaries_run_in_their_own_lane_and_supersede_only_intent_work() {
    let saved = native_request(221);
    let lifecycle = saved.lifecycle.clone().unwrap();
    let envelope = saved.hook.envelope().clone();
    let binding = HookScopeBindingV1 {
        host: envelope.producer,
        project_id: envelope.project_id,
        repository_id: envelope.repository_id,
        worktree_id: envelope.worktree_id,
        worktree_epoch: envelope.worktree_epoch,
        binding_token: envelope.binding_token,
        capabilities: [
            HookEventFamily::SessionBoundary,
            HookEventFamily::PromptBoundary,
            HookEventFamily::ToolLifecycle,
            HookEventFamily::SavedEdit,
            HookEventFamily::TestLifecycle,
        ]
        .into_iter()
        .map(|family| tracedecay_hooks::HookCapabilityV1 {
            family,
            support: tracedecay_hooks::stock_event_support(envelope.producer, family),
        })
        .collect(),
    };
    let boundary = |event_id: u8, event: HookEventV2| {
        let mut boundary = envelope.clone();
        boundary.event_id = [event_id; 16];
        boundary.event = event;
        boundary
    };
    let tool = |phase| HookEventV2::ToolLifecycle {
        tool_id: [9; 16],
        phase,
        effect_receipt_id: None,
    };
    let stop = HookEventV2::SessionBoundary {
        boundary: HookBoundaryV1::TurnComplete,
    };
    // Kiro admits prompt boundaries natively today; every other host gains
    // them when it registers its prompt hook.
    let kiro = tracedecay_domain::NativeHostIdentityV1::Kiro;
    let kiro_binding = HookScopeBindingV1 {
        host: kiro,
        capabilities: vec![tracedecay_hooks::HookCapabilityV1 {
            family: HookEventFamily::PromptBoundary,
            support: HookEventSupportV1::Native,
        }],
        ..binding.clone()
    };
    let mut kiro_prompt = boundary(229, HookEventV2::PromptBoundary);
    kiro_prompt.producer = kiro;
    assert_eq!(
        HookOrchestrationRequestV1::from_envelope(kiro_prompt, &kiro_binding, None, 1, false)
            .map(|request| request.trigger),
        Some(HookOrchestrationTriggerV1::IntentBoundary)
    );
    for (event, expected) in [
        (
            tool(HookLifecyclePhaseV1::Completed),
            Some(HookOrchestrationTriggerV1::IntentBoundary),
        ),
        (
            tool(HookLifecyclePhaseV1::Failed),
            Some(HookOrchestrationTriggerV1::IntentBoundary),
        ),
        (tool(HookLifecyclePhaseV1::Started), None),
        (tool(HookLifecyclePhaseV1::Cancelled), None),
        (
            HookEventV2::SessionBoundary {
                boundary: HookBoundaryV1::Start,
            },
            None,
        ),
        (stop.clone(), Some(HookOrchestrationTriggerV1::Stop)),
    ] {
        assert_eq!(
            HookOrchestrationRequestV1::from_envelope(
                boundary(230, event.clone()),
                &binding,
                None,
                1,
                false
            )
            .map(|request| request.trigger),
            expected,
            "{event:?}"
        );
    }
    let stop_request = HookOrchestrationRequestV1::from_envelope(
        boundary(230, stop.clone()),
        &binding,
        None,
        1,
        false,
    )
    .unwrap();
    let companion = stop_request
        .intent_companion()
        .expect("stop owes an intent pass");
    assert_eq!(
        companion.trigger,
        HookOrchestrationTriggerV1::IntentBoundary
    );
    assert!(companion.completion.is_none());
    assert!(saved.intent_companion().is_none());
    assert!(companion.intent_companion().is_none());

    let (events, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let runtime = BoundedHookOrchestratorV1::new(1, {
        let release = Arc::clone(&release);
        move |request: HookOrchestrationRequestV1, cancellation: CancellationToken| {
            let events = events.clone();
            let release = Arc::clone(&release);
            async move {
                let event = (
                    request.hook.envelope().event_id[0],
                    request.trigger,
                    request.lifecycle.is_some(),
                );
                events.send((event, "started")).unwrap();
                match request.trigger {
                    HookOrchestrationTriggerV1::IntentBoundary => {
                        tokio::select! {
                            () = cancellation.cancelled() => {
                                events.send((event, "superseded")).unwrap();
                                HookOrchestrationWorkOutcomeV1::RetryableFailure
                            }
                            () = release.notified() => HookOrchestrationWorkOutcomeV1::Completed,
                        }
                    }
                    _ if request.lifecycle.is_none() => {
                        HookOrchestrationWorkOutcomeV1::AwaitingLifecycle
                    }
                    _ => HookOrchestrationWorkOutcomeV1::Completed,
                }
            }
        }
    })
    .unwrap();
    assert!(register_hook_orchestration_runtime(
        envelope.project_id,
        envelope.worktree_id,
        &runtime
    ));
    let intent = HookOrchestrationTriggerV1::IntentBoundary;
    // Work runs on the native runtime; wait for exactly `remaining` running
    // cycles so no admission races a permit that is still being released.
    let settle_to = |remaining: usize| {
        for _ in 0..5_000 {
            if runtime.active_tasks() == remaining {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("orchestrator never settled to {remaining} running cycles");
    };
    let in_session = |event_id: u8, event: HookEventV2, session: u8| {
        let mut envelope = boundary(event_id, event);
        envelope.protected_session_id = [session; 32];
        envelope
    };

    // A saved edit without a lifecycle settles and parks in the feedback lane.
    let (parked, parked_completion) = tokio::sync::oneshot::channel();
    let parked = StdMutex::new(Some(parked));
    assert_eq!(
        admit_registered_hook_orchestration(
            in_session(231, saved.hook.envelope().event.clone(), 1),
            binding.clone(),
            None,
            1,
            false,
            Some(Arc::new(move || {
                if let Some(parked) = parked.lock().unwrap().take() {
                    let _ = parked.send(());
                }
            })),
        ),
        HookOrchestrationAdmissionV1::Enqueued
    );
    assert_eq!(
        observed.recv().await,
        Some((
            (231, HookOrchestrationTriggerV1::SavedEdit, false),
            "started"
        ))
    );
    parked_completion.await.unwrap();
    settle_to(0);

    // A completed tool call bearing the session's lifecycle runs its intent
    // pass and, like any lifecycle-bearing hook, resumes the parked boundary.
    assert_eq!(
        admit_registered_hook_orchestration(
            in_session(232, tool(HookLifecyclePhaseV1::Completed), 1),
            binding.clone(),
            Some(lifecycle.clone()),
            1,
            false,
            None,
        ),
        HookOrchestrationAdmissionV1::Enqueued
    );
    let started = [
        observed.recv().await.unwrap(),
        observed.recv().await.unwrap(),
    ];
    for expected in [
        (
            (231, HookOrchestrationTriggerV1::SavedEdit, true),
            "started",
        ),
        ((232, intent, true), "started"),
    ] {
        assert!(started.contains(&expected), "{expected:?} in {started:?}");
    }
    settle_to(1);

    // With that intent pass still running, another session's saved edit is
    // admitted at once: intent work holds only intent permits.
    assert_eq!(
        admit_registered_hook_orchestration(
            in_session(233, saved.hook.envelope().event.clone(), 2),
            binding.clone(),
            Some(lifecycle.clone()),
            1,
            false,
            None,
        ),
        HookOrchestrationAdmissionV1::Enqueued
    );
    assert_eq!(
        observed.recv().await,
        Some((
            (233, HookOrchestrationTriggerV1::SavedEdit, true),
            "started"
        ))
    );
    settle_to(1);
    // The intent lane is bounded on its own: another session's intent pass
    // is backpressured while the first one runs.
    assert_eq!(
        admit_registered_hook_orchestration(
            in_session(236, tool(HookLifecyclePhaseV1::Failed), 2),
            binding.clone(),
            Some(lifecycle.clone()),
            1,
            false,
            None,
        ),
        HookOrchestrationAdmissionV1::Backpressured
    );

    // A failed tool call of the first session supersedes its running
    // intent pass and inherits the session's admitted lifecycle.
    assert_eq!(
        admit_registered_hook_orchestration(
            in_session(234, tool(HookLifecyclePhaseV1::Failed), 1),
            binding.clone(),
            None,
            1,
            false,
            None,
        ),
        HookOrchestrationAdmissionV1::Enqueued
    );
    let transitions = [
        observed.recv().await.unwrap(),
        observed.recv().await.unwrap(),
    ];
    for expected in [
        ((232, intent, true), "superseded"),
        ((234, intent, true), "started"),
    ] {
        assert!(
            transitions.contains(&expected),
            "{expected:?} in {transitions:?}"
        );
    }

    // A stop boundary reports its owed feedback admission and also runs an
    // intent pass that supersedes the tool boundary's.
    let (stopped, stop_completion) = tokio::sync::oneshot::channel();
    let stopped = StdMutex::new(Some(stopped));
    assert_eq!(
        admit_registered_hook_orchestration(
            in_session(235, stop, 1),
            binding.clone(),
            Some(lifecycle),
            1,
            false,
            Some(Arc::new(move || {
                if let Some(stopped) = stopped.lock().unwrap().take() {
                    let _ = stopped.send(());
                }
            })),
        ),
        HookOrchestrationAdmissionV1::Enqueued
    );
    let transitions = [
        observed.recv().await.unwrap(),
        observed.recv().await.unwrap(),
        observed.recv().await.unwrap(),
    ];
    for expected in [
        ((234, intent, true), "superseded"),
        ((235, HookOrchestrationTriggerV1::Stop, true), "started"),
        ((235, intent, true), "started"),
    ] {
        assert!(
            transitions.contains(&expected),
            "{expected:?} in {transitions:?}"
        );
    }
    stop_completion
        .await
        .expect("the stop boundary's feedback completion fires");
    release.notify_one();
    settle_to(0);

    unregister_hook_orchestration_runtime(envelope.project_id, envelope.worktree_id, &runtime);
    assert!(runtime.shutdown().await);
}

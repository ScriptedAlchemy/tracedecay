use super::{
    Arc, BoundedHookOrchestratorV1, ContextScoutLifecycleAddressV1, HookOrchestrationAdmissionV1,
    HookOrchestrationRequestV1, HookOrchestrationSettlementV1, HookOrchestrationTriggerV1,
    HookOrchestrationWorkOutcomeV1, MAX_COALESCED_HOOK_COMPLETIONS, ProjectId, UtcMicros,
    admit_registered_hook_orchestration, register_hook_orchestration_runtime,
    unregister_hook_orchestration_runtime,
};
use tracedecay_hooks::{HookEventEnvelopeV2, HookEventV2, HookScopeBindingV1};

use tracedecay_hooks::HookBoundaryV1;

/// The orchestration request for the fixture saved-edit hook.
fn saved_edit_request(
    lifecycle: Option<ContextScoutLifecycleAddressV1>,
) -> HookOrchestrationRequestV1 {
    HookOrchestrationRequestV1::from_envelope(
        hook_envelope(HookEventV2::SavedEdit {
            file_id: [7; 16],
            changed_range_count: 1,
        }),
        &hook_binding(),
        lifecycle,
        1,
        false,
    )
    .unwrap()
}

/// Hook work that never completes and records when it is dropped.
struct PendingWork {
    dropped: Arc<std::sync::atomic::AtomicBool>,
}

impl std::future::Future for PendingWork {
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::task::Poll::Pending
    }
}

impl Drop for PendingWork {
    fn drop(&mut self) {
        self.dropped
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

#[test]
fn hook_orchestration_admits_only_saved_edit_stop_and_explicit() {
    let saved = saved_edit_request(Some(hook_lifecycle()));
    assert_eq!(saved.trigger, HookOrchestrationTriggerV1::SavedEdit);

    let stop = HookOrchestrationRequestV1::from_envelope(
        hook_envelope(HookEventV2::SessionBoundary {
            boundary: HookBoundaryV1::TurnComplete,
        }),
        &hook_binding(),
        Some(hook_lifecycle()),
        1,
        false,
    )
    .unwrap();
    assert_eq!(stop.trigger, HookOrchestrationTriggerV1::Stop);

    let without_scout_lifecycle = saved_edit_request(None);
    assert_eq!(
        without_scout_lifecycle.trigger,
        HookOrchestrationTriggerV1::SavedEdit
    );
    assert!(without_scout_lifecycle.lifecycle.is_none());

    assert!(
        HookOrchestrationRequestV1::from_envelope(
            hook_envelope(HookEventV2::TestLifecycle {
                test_run_id: [8; 16],
                test_count: 1,
                phase: tracedecay_hooks::HookLifecyclePhaseV1::Completed,
                receipt_id: Some([9; 16]),
            }),
            &hook_binding(),
            Some(hook_lifecycle()),
            1,
            false,
        )
        .is_none()
    );
    assert_eq!(
        HookOrchestrationRequestV1::from_envelope(
            hook_envelope(HookEventV2::SessionBoundary {
                boundary: HookBoundaryV1::Start,
            }),
            &hook_binding(),
            Some(hook_lifecycle()),
            1,
            true,
        )
        .unwrap()
        .trigger,
        HookOrchestrationTriggerV1::Explicit
    );
}

#[tokio::test]
async fn hook_orchestration_backpressures_without_waiting() {
    let release = Arc::new(tokio::sync::Notify::new());
    let work_release = Arc::clone(&release);
    let work = move |_, _| {
        let release = Arc::clone(&work_release);
        async move { release.notified().await }
    };
    let runtime = BoundedHookOrchestratorV1::new(1, work).unwrap();
    let completions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_completions = Arc::clone(&completions);
    let completed = Arc::new(tokio::sync::Notify::new());
    let completion_notification = Arc::clone(&completed);
    let mut request = saved_edit_request(None);
    request.completion = Some(Arc::new(move |_| {
        observed_completions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        completion_notification.notify_one();
    }));
    // A distinct hook event at a distinct work address is real new work, so it
    // must contend for the single permit rather than join or supersede the
    // admitted boundary. Address identity is the envelope's project, repository,
    // worktree, and protected session, not the event id or scout lifecycle.
    let mut other_envelope = hook_envelope(HookEventV2::SavedEdit {
        file_id: [8; 16],
        changed_range_count: 1,
    });
    other_envelope.event_id = [2; 16];
    other_envelope.protected_session_id = [9; 32];
    let other_request = HookOrchestrationRequestV1::from_envelope(
        other_envelope,
        &hook_binding(),
        Some(hook_lifecycle()),
        1,
        false,
    )
    .unwrap();

    assert_eq!(
        runtime.admit(request),
        HookOrchestrationAdmissionV1::Enqueued
    );
    assert_eq!(
        runtime.admit(other_request),
        HookOrchestrationAdmissionV1::Backpressured
    );
    assert_eq!(completions.load(std::sync::atomic::Ordering::Relaxed), 0);
    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(1), completed.notified())
        .await
        .expect("producer work completion");
    assert_eq!(
        completions.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "only completed producer work may clear the durable outbox"
    );
}

thread_local! {
    /// Set while this thread is inside a hook admission call.
    static ADMITTING_HOOK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[tokio::test]
async fn hook_orchestration_runs_feedback_work_without_scout_lifecycle() {
    let ran_synchronously = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let work_ran_synchronously = Arc::clone(&ran_synchronously);
    let ran = Arc::new(tokio::sync::Notify::new());
    let work_ran = Arc::clone(&ran);
    let runtime = BoundedHookOrchestratorV1::new(1, move |_, _| {
        let ran = Arc::clone(&work_ran);
        let ran_synchronously = Arc::clone(&work_ran_synchronously);
        async move {
            // The work runs on a native worker and may start before the
            // admitting thread resumes, so only a run on the admitting
            // thread inside the call counts as synchronous.
            if ADMITTING_HOOK.get() {
                ran_synchronously.store(true, std::sync::atomic::Ordering::Release);
            }
            ran.notify_one();
        }
    })
    .unwrap();
    assert!(register_hook_orchestration_runtime(
        [3; 16], [5; 16], &runtime
    ));

    ADMITTING_HOOK.set(true);
    let admission = admit_registered_hook_orchestration(
        hook_envelope(HookEventV2::SavedEdit {
            file_id: [7; 16],
            changed_range_count: 1,
        }),
        hook_binding(),
        None,
        1,
        false,
        None,
        None,
    );
    ADMITTING_HOOK.set(false);
    assert_eq!(admission, HookOrchestrationAdmissionV1::Enqueued);
    ran.notified().await;
    assert!(
        !ran_synchronously.load(std::sync::atomic::Ordering::Acquire),
        "hook admission must return before daemon-owned provider or model work begins"
    );
    unregister_hook_orchestration_runtime([3; 16], [5; 16], &runtime);
}

#[tokio::test]
async fn hook_orchestration_registry_refuses_a_foreign_live_incumbent() {
    let incumbent = BoundedHookOrchestratorV1::new(1, |_, _| async {}).unwrap();
    let successor = BoundedHookOrchestratorV1::new(1, |_, _| async {}).unwrap();
    assert!(register_hook_orchestration_runtime(
        [13; 16], [15; 16], &incumbent
    ));
    assert!(
        !register_hook_orchestration_runtime([13; 16], [15; 16], &successor),
        "a live incumbent must keep its hook locator pair"
    );
    // A foreign runtime never unregisters the incumbent.
    unregister_hook_orchestration_runtime([13; 16], [15; 16], &successor);
    assert!(register_hook_orchestration_runtime(
        [13; 16], [15; 16], &incumbent
    ));
    unregister_hook_orchestration_runtime([13; 16], [15; 16], &incumbent);
    assert!(register_hook_orchestration_runtime(
        [13; 16], [15; 16], &successor
    ));
    unregister_hook_orchestration_runtime([13; 16], [15; 16], &successor);
}

#[tokio::test]
async fn hook_orchestration_coalesces_duplicate_work_and_completes_every_admission() {
    let release = Arc::new(tokio::sync::Notify::new());
    let work_release = Arc::clone(&release);
    let work_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_work_calls = Arc::clone(&work_calls);
    let work_started = Arc::new(tokio::sync::Notify::new());
    let observed_work_started = Arc::clone(&work_started);
    let runtime = BoundedHookOrchestratorV1::new(1, move |_, _| {
        let release = Arc::clone(&work_release);
        let work_calls = Arc::clone(&observed_work_calls);
        let work_started = Arc::clone(&observed_work_started);
        async move {
            work_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            work_started.notify_one();
            release.notified().await;
        }
    })
    .unwrap();
    let completions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let completed = Arc::new(tokio::sync::Notify::new());
    let mut request = saved_edit_request(Some(hook_lifecycle()));
    let first_completions = Arc::clone(&completions);
    let first_completed = Arc::clone(&completed);
    request.completion = Some(Arc::new(move |_| {
        first_completions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        first_completed.notify_one();
    }));
    let mut duplicate = request.clone();
    let duplicate_completions = Arc::clone(&completions);
    let duplicate_completed = Arc::clone(&completed);
    duplicate.completion = Some(Arc::new(move |_| {
        duplicate_completions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        duplicate_completed.notify_one();
    }));

    assert_eq!(
        runtime.admit(request),
        HookOrchestrationAdmissionV1::Enqueued
    );
    work_started.notified().await;
    assert_eq!(
        runtime.admit(duplicate),
        HookOrchestrationAdmissionV1::Enqueued,
        "an exact duplicate must join the admitted work instead of consuming capacity"
    );
    assert_eq!(work_calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while completions.load(std::sync::atomic::Ordering::Relaxed) != 2 {
            completed.notified().await;
        }
    })
    .await
    .expect("both joined admissions complete");
    assert_eq!(work_calls.load(std::sync::atomic::Ordering::Relaxed), 1);
}

#[tokio::test]
async fn hook_orchestration_supersedes_same_address_without_replay_delay() {
    struct WorkGuard {
        dropped: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Drop for WorkGuard {
        fn drop(&mut self) {
            self.dropped
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }

    let first_started = Arc::new(tokio::sync::Notify::new());
    let observed_first_started = Arc::clone(&first_started);
    let first_dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_first_drop = Arc::clone(&first_dropped);
    let runtime = BoundedHookOrchestratorV1::new(1, move |request, cancellation| {
        let first_started = Arc::clone(&observed_first_started);
        let first_dropped = Arc::clone(&observed_first_drop);
        async move {
            if request.hook.envelope().event_id == [1; 16] {
                let _guard = WorkGuard {
                    dropped: first_dropped,
                };
                first_started.notify_one();
                cancellation.cancelled().await;
            }
        }
    })
    .unwrap();
    let first_completions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_first_completions = Arc::clone(&first_completions);
    let first_completed = Arc::new(tokio::sync::Notify::new());
    let observed_first_completed = Arc::clone(&first_completed);
    let mut first = saved_edit_request(Some(hook_lifecycle()));
    first.completion = Some(Arc::new(move |_| {
        observed_first_completions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        observed_first_completed.notify_one();
    }));
    assert_eq!(runtime.admit(first), HookOrchestrationAdmissionV1::Enqueued);
    first_started.notified().await;

    let second_completions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_second_completions = Arc::clone(&second_completions);
    let second_completed = Arc::new(tokio::sync::Notify::new());
    let observed_second_completed = Arc::clone(&second_completed);
    let mut second_envelope = hook_envelope(HookEventV2::SessionBoundary {
        boundary: HookBoundaryV1::TurnComplete,
    });
    second_envelope.event_id = [2; 16];
    let mut second = HookOrchestrationRequestV1::from_envelope(
        second_envelope,
        &hook_binding(),
        Some(ContextScoutLifecycleAddressV1 {
            turn_id: tracedecay_domain::TurnId::new("turn.advisory-hook.next").unwrap(),
            logical_message_id: tracedecay_domain::MessageId::new("message.advisory-hook.next")
                .unwrap(),
            ..hook_lifecycle()
        }),
        1,
        false,
    )
    .unwrap();
    second.completion = Some(Arc::new(move |_| {
        observed_second_completions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        observed_second_completed.notify_one();
    }));
    assert_eq!(
        runtime.admit(second),
        HookOrchestrationAdmissionV1::Enqueued,
        "a newer edit/stop event at the same address must replace queued work"
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        tokio::join!(first_completed.notified(), second_completed.notified());
    })
    .await
    .expect("superseding event runs as soon as the cancelled worker releases capacity");
    assert!(first_dropped.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(
        first_completions.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the successful successor acknowledges coalesced obsolete work"
    );
    assert_eq!(
        second_completions.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}

#[tokio::test]
async fn no_identity_successor_retains_exact_lifecycle_without_overwriting_explicit_identity() {
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let work_observed = Arc::clone(&observed);
    let first_started = Arc::new(tokio::sync::Notify::new());
    let work_first_started = Arc::clone(&first_started);
    let second_started = Arc::new(tokio::sync::Notify::new());
    let work_second_started = Arc::clone(&second_started);
    let third_started = Arc::new(tokio::sync::Notify::new());
    let work_third_started = Arc::clone(&third_started);
    let runtime = BoundedHookOrchestratorV1::new(1, move |request, cancellation| {
        let observed = Arc::clone(&work_observed);
        let first_started = Arc::clone(&work_first_started);
        let second_started = Arc::clone(&work_second_started);
        let third_started = Arc::clone(&work_third_started);
        async move {
            let event_id = request.hook.envelope().event_id;
            observed.lock().unwrap().push((event_id, request.lifecycle));
            if event_id == [1; 16] {
                first_started.notify_one();
            } else if event_id == [2; 16] {
                second_started.notify_one();
            } else if event_id == [3; 16] {
                third_started.notify_one();
            } else {
                unreachable!("test event identity");
            }
            if event_id != [3; 16] {
                cancellation.cancelled().await;
            }
        }
    })
    .unwrap();

    let original_lifecycle = hook_lifecycle();
    let first = saved_edit_request(Some(original_lifecycle.clone()));
    assert_eq!(runtime.admit(first), HookOrchestrationAdmissionV1::Enqueued);
    first_started.notified().await;

    let mut second_envelope = hook_envelope(HookEventV2::SavedEdit {
        file_id: [8; 16],
        changed_range_count: 1,
    });
    second_envelope.event_id = [2; 16];
    let second =
        HookOrchestrationRequestV1::from_envelope(second_envelope, &hook_binding(), None, 1, false)
            .unwrap();
    assert_eq!(
        runtime.admit(second),
        HookOrchestrationAdmissionV1::Enqueued
    );
    second_started.notified().await;

    let explicit_lifecycle = ContextScoutLifecycleAddressV1 {
        turn_id: tracedecay_domain::TurnId::new("turn.advisory-hook.explicit").unwrap(),
        logical_message_id: tracedecay_domain::MessageId::new("message.advisory-hook.explicit")
            .unwrap(),
        ..hook_lifecycle()
    };
    let mut third_envelope = hook_envelope(HookEventV2::SessionBoundary {
        boundary: HookBoundaryV1::TurnComplete,
    });
    third_envelope.event_id = [3; 16];
    let third = HookOrchestrationRequestV1::from_envelope(
        third_envelope,
        &hook_binding(),
        Some(explicit_lifecycle.clone()),
        1,
        false,
    )
    .unwrap();
    assert_eq!(runtime.admit(third), HookOrchestrationAdmissionV1::Enqueued);
    third_started.notified().await;

    let observed = observed.lock().unwrap();
    assert_eq!(observed[1], ([2; 16], Some(original_lifecycle)));
    assert_eq!(observed[2], ([3; 16], Some(explicit_lifecycle)));
}

/// The durable-replay half of the retryable contract: after a cycle fails
/// retryably and settles, the spool consumer re-admits the exact same
/// envelope. The orchestrator must run a fresh cycle for it, not treat the
/// settled failure as still in flight, and only the genuinely successful
/// cycle may fire the acknowledgement that clears the pending hook work.
#[tokio::test]
async fn replayed_admission_after_retryable_failure_completes_a_fresh_cycle() {
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let work_attempts = Arc::clone(&attempts);
    let first_attempted = Arc::new(tokio::sync::Notify::new());
    let work_first_attempted = Arc::clone(&first_attempted);
    let runtime = BoundedHookOrchestratorV1::new(1, move |_, _| {
        let attempts = Arc::clone(&work_attempts);
        let first_attempted = Arc::clone(&work_first_attempted);
        async move {
            let attempt = attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if attempt == 0 {
                first_attempted.notify_one();
                HookOrchestrationWorkOutcomeV1::RetryableFailure
            } else {
                HookOrchestrationWorkOutcomeV1::Completed
            }
        }
    })
    .unwrap();
    let mut envelope = hook_envelope(HookEventV2::SavedEdit {
        file_id: [7; 16],
        changed_range_count: 1,
    });
    envelope.project_id = [23; 16];
    envelope.worktree_id = [25; 16];
    let mut binding = hook_binding();
    binding.project_id = [23; 16];
    binding.worktree_id = [25; 16];
    assert!(register_hook_orchestration_runtime(
        [23; 16], [25; 16], &runtime
    ));
    let acknowledged = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let first_acknowledged = Arc::clone(&acknowledged);
    assert_eq!(
        admit_registered_hook_orchestration(
            envelope.clone(),
            binding.clone(),
            Some(hook_lifecycle()),
            1,
            false,
            None,
            Some(Arc::new(move |_| {
                first_acknowledged.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })),
        ),
        HookOrchestrationAdmissionV1::Enqueued
    );
    first_attempted.notified().await;
    while runtime.active_tasks() != 0 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        acknowledged.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "the failed cycle must not acknowledge the durable admission"
    );

    // One replay of the settled failure. Replaying again while that cycle is
    // still running would only join it, and past the coalescing bound a join
    // is backpressured by design.
    let replay_acknowledged = Arc::clone(&acknowledged);
    let replay_settled = Arc::new(tokio::sync::Notify::new());
    let replay_terminal = Arc::clone(&replay_settled);
    assert_eq!(
        admit_registered_hook_orchestration(
            envelope.clone(),
            binding.clone(),
            Some(hook_lifecycle()),
            1,
            false,
            None,
            Some(Arc::new(move |_| {
                replay_acknowledged.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                replay_terminal.notify_one();
            })),
        ),
        HookOrchestrationAdmissionV1::Enqueued,
        "the settled failure must free its slot for the replay"
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), replay_settled.notified())
        .await
        .expect("a replayed admission must complete a fresh cycle after a retryable failure");
    assert_eq!(acknowledged.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(
        attempts.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "acknowledgement requires a fresh successful cycle, not the settled failure"
    );
    unregister_hook_orchestration_runtime([23; 16], [25; 16], &runtime);
}

#[tokio::test]
async fn superseded_blocking_work_is_joined_before_its_receipt_terminal() {
    let (blocking_started, blocking_started_receiver) = tokio::sync::oneshot::channel();
    let blocking_started = Arc::new(std::sync::Mutex::new(Some(blocking_started)));
    let blocking_gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let blocking_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let successor_release = Arc::new(tokio::sync::Notify::new());
    let work_started = Arc::clone(&blocking_started);
    let work_gate = Arc::clone(&blocking_gate);
    let work_finished = Arc::clone(&blocking_finished);
    let work_successor_release = Arc::clone(&successor_release);
    let runtime = BoundedHookOrchestratorV1::new(1, move |request, _cancellation| {
        let started = Arc::clone(&work_started);
        let gate = Arc::clone(&work_gate);
        let finished = Arc::clone(&work_finished);
        let successor_release = Arc::clone(&work_successor_release);
        async move {
            if request.hook.envelope().event_id == [1; 16] {
                let task = tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
                    if let Some(started) = started
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take()
                    {
                        let _ = started.send(());
                    }
                    let (released, changed) = &*gate;
                    let mut released = released
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    while !*released {
                        released = changed
                            .wait(released)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                    finished.store(true, std::sync::atomic::Ordering::Release);
                });
                let _ = task.await;
            } else {
                successor_release.notified().await;
            }
        }
    })
    .unwrap();
    let first_terminal = Arc::new(tokio::sync::Notify::new());
    let observed_first_terminal = Arc::clone(&first_terminal);
    let nested_finished_at_terminal = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_nested_finished = Arc::clone(&nested_finished_at_terminal);
    let completion_finished = Arc::clone(&blocking_finished);
    let mut first = saved_edit_request(Some(hook_lifecycle()));
    first.completion = Some(Arc::new(move |_| {
        observed_nested_finished.store(
            completion_finished.load(std::sync::atomic::Ordering::Acquire),
            std::sync::atomic::Ordering::Release,
        );
        observed_first_terminal.notify_one();
    }));
    assert_eq!(runtime.admit(first), HookOrchestrationAdmissionV1::Enqueued);
    blocking_started_receiver
        .await
        .expect("blocking provider work started");

    let mut successor_envelope = hook_envelope(HookEventV2::SessionBoundary {
        boundary: HookBoundaryV1::TurnComplete,
    });
    successor_envelope.event_id = [2; 16];
    let successor = HookOrchestrationRequestV1::from_envelope(
        successor_envelope,
        &hook_binding(),
        Some(hook_lifecycle()),
        1,
        false,
    )
    .unwrap();
    assert_eq!(
        runtime.admit(successor),
        HookOrchestrationAdmissionV1::Enqueued
    );
    let (released, changed) = &*blocking_gate;
    *released
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    changed.notify_all();
    // The successor holds capacity across this wait, so a terminal that waits
    // on it never arrives. The bound only turns that into a failure.
    let terminal = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        first_terminal.notified(),
    )
    .await;
    successor_release.notify_one();
    assert!(
        terminal.is_ok(),
        "the superseded operation must emit its own receipt terminal"
    );
    assert!(
        nested_finished_at_terminal.load(std::sync::atomic::Ordering::Acquire),
        "the receipt terminal must follow settlement of nested blocking work"
    );
}

#[tokio::test]
async fn superseded_queued_work_emits_its_own_receipt_terminal() {
    let first_started = Arc::new(tokio::sync::Notify::new());
    let work_started = Arc::clone(&first_started);
    let first_release = Arc::new(tokio::sync::Notify::new());
    let work_release = Arc::clone(&first_release);
    let runtime = BoundedHookOrchestratorV1::new(1, move |request, cancellation| {
        let started = Arc::clone(&work_started);
        let release = Arc::clone(&work_release);
        async move {
            if request.hook.envelope().event_id == [1; 16] {
                started.notify_one();
                cancellation.cancelled().await;
                release.notified().await;
            }
        }
    })
    .unwrap();
    let request = |event_id, completion| {
        let mut envelope = hook_envelope(HookEventV2::SavedEdit {
            file_id: [7; 16],
            changed_range_count: 1,
        });
        envelope.event_id = event_id;
        let mut request = HookOrchestrationRequestV1::from_envelope(
            envelope,
            &hook_binding(),
            Some(hook_lifecycle()),
            1,
            false,
        )
        .unwrap();
        request.completion = completion;
        request
    };
    assert_eq!(
        runtime.admit(request([1; 16], None)),
        HookOrchestrationAdmissionV1::Enqueued
    );
    first_started.notified().await;

    let queued_terminal = Arc::new(tokio::sync::Notify::new());
    let observed_queued_terminal = Arc::clone(&queued_terminal);
    assert_eq!(
        runtime.admit(request(
            [2; 16],
            Some(Arc::new(move |_| observed_queued_terminal.notify_one())),
        )),
        HookOrchestrationAdmissionV1::Enqueued
    );
    assert_eq!(
        runtime.admit(request([3; 16], None)),
        HookOrchestrationAdmissionV1::Enqueued
    );
    // The first operation holds capacity across this wait, so a terminal that
    // waits for capacity never arrives. The bound only turns that into a failure.
    let terminal = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        queued_terminal.notified(),
    )
    .await;
    first_release.notify_one();
    assert!(
        terminal.is_ok(),
        "superseded work that never acquired capacity must settle its own receipt"
    );
}

#[tokio::test]
async fn hook_orchestration_bounds_coalesced_completion_waiters() {
    let release = Arc::new(tokio::sync::Notify::new());
    let work_release = Arc::clone(&release);
    let runtime = BoundedHookOrchestratorV1::new(1, move |_, _| {
        let release = Arc::clone(&work_release);
        async move {
            release.notified().await;
        }
    })
    .unwrap();
    let completion = Arc::new(|_| {}) as Arc<dyn Fn(HookOrchestrationSettlementV1) + Send + Sync>;
    let request = |completion| {
        let mut request = saved_edit_request(Some(hook_lifecycle()));
        request.completion = Some(completion);
        request
    };
    let first = request(Arc::clone(&completion));
    assert_eq!(runtime.admit(first), HookOrchestrationAdmissionV1::Enqueued);
    for _ in 1..MAX_COALESCED_HOOK_COMPLETIONS {
        let duplicate = request(Arc::clone(&completion));
        assert_eq!(
            runtime.admit(duplicate),
            HookOrchestrationAdmissionV1::Enqueued
        );
    }
    let overflow = request(completion);
    assert_eq!(
        runtime.admit(overflow),
        HookOrchestrationAdmissionV1::Backpressured
    );
    release.notify_one();
}

#[tokio::test]
async fn dropping_hook_orchestrator_cancels_daemon_owned_work_without_false_completion() {
    let work_dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_work_drop = Arc::clone(&work_dropped);
    let work_started = Arc::new(tokio::sync::Notify::new());
    let observed_work_started = Arc::clone(&work_started);
    let runtime = BoundedHookOrchestratorV1::new(1, move |_, _| {
        observed_work_started.notify_one();
        PendingWork {
            dropped: Arc::clone(&observed_work_drop),
        }
    })
    .unwrap();
    let completions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_completions = Arc::clone(&completions);
    let mut request = saved_edit_request(Some(hook_lifecycle()));
    request.completion = Some(Arc::new(move |_| {
        observed_completions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }));
    assert_eq!(
        runtime.admit(request),
        HookOrchestrationAdmissionV1::Enqueued
    );
    work_started.notified().await;
    drop(runtime);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !work_dropped.load(std::sync::atomic::Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retiring the retained owner cancels its worker");
    assert_eq!(
        completions.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "cancelled work is not reported as completed"
    );
}

#[tokio::test]
async fn hook_orchestrator_shutdown_fences_and_joins_pending_work() {
    let work_dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_work_drop = Arc::clone(&work_dropped);
    let work_started = Arc::new(tokio::sync::Notify::new());
    let observed_work_started = Arc::clone(&work_started);
    let runtime = BoundedHookOrchestratorV1::new(1, move |_, _| {
        observed_work_started.notify_one();
        PendingWork {
            dropped: Arc::clone(&observed_work_drop),
        }
    })
    .unwrap();
    let request = saved_edit_request(Some(hook_lifecycle()));
    assert_eq!(
        runtime.admit(request.clone()),
        HookOrchestrationAdmissionV1::Enqueued
    );
    work_started.notified().await;

    assert!(
        runtime.shutdown().await,
        "pending work must cancel and join"
    );
    assert!(work_dropped.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(runtime.active_tasks(), 0);
    assert_eq!(
        runtime.admit(request),
        HookOrchestrationAdmissionV1::Unavailable,
        "shutdown must fence later hook-cycle admission"
    );
}

#[tokio::test]
async fn hook_orchestrator_shutdown_reports_a_failed_worker() {
    struct PanickingWork;

    impl std::future::Future for PanickingWork {
        type Output = ();

        fn poll(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            panic!("hook worker failure probe");
        }
    }

    let runtime = BoundedHookOrchestratorV1::new(1, move |_, _| PanickingWork).unwrap();
    let request = saved_edit_request(Some(hook_lifecycle()));
    assert_eq!(
        runtime.admit(request),
        HookOrchestrationAdmissionV1::Enqueued
    );
    while runtime.active_tasks() != 0 {
        tokio::task::yield_now().await;
    }

    assert!(
        !runtime.shutdown().await,
        "a panicked worker must remain a typed unclean shutdown"
    );
}

fn hook_envelope(event: HookEventV2) -> HookEventEnvelopeV2 {
    HookEventEnvelopeV2 {
        schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
        event_id: [1; 16],
        producer: tracedecay_domain::NativeHostIdentityV1::Codex,
        protected_session_id: [2; 32],
        project_id: [3; 16],
        repository_id: [4; 16],
        worktree_id: [5; 16],
        worktree_epoch: 1,
        binding_token: [6; 32],
        ordering: tracedecay_hooks::HookOrderingV1::Unknown,
        observed_at: UtcMicros(1),
        event,
    }
}

fn hook_binding() -> HookScopeBindingV1 {
    HookScopeBindingV1 {
        host: tracedecay_domain::NativeHostIdentityV1::Codex,
        project_id: [3; 16],
        repository_id: [4; 16],
        worktree_id: [5; 16],
        worktree_epoch: 1,
        binding_token: [6; 32],
        capabilities: [
            tracedecay_hooks::HookEventFamily::SessionBoundary,
            tracedecay_hooks::HookEventFamily::PromptBoundary,
            tracedecay_hooks::HookEventFamily::ToolLifecycle,
            tracedecay_hooks::HookEventFamily::SavedEdit,
            tracedecay_hooks::HookEventFamily::TestLifecycle,
        ]
        .into_iter()
        .map(|family| tracedecay_hooks::HookCapabilityV1 {
            family,
            support: tracedecay_hooks::stock_event_support(
                tracedecay_domain::NativeHostIdentityV1::Codex,
                family,
            ),
        })
        .collect(),
    }
}

fn hook_lifecycle() -> ContextScoutLifecycleAddressV1 {
    ContextScoutLifecycleAddressV1 {
        profile_id: tracedecay_domain::UserProfileId::new("profile.advisory-hook").unwrap(),
        provider_id: tracedecay_domain::ProviderId::new("codex").unwrap(),
        project_id: ProjectId::new("project.advisory-hook").unwrap(),
        worktree_id: tracedecay_domain::WorktreeId::new("worktree.advisory-hook").unwrap(),
        session_id: tracedecay_domain::SessionId::new("session.advisory-hook").unwrap(),
        thread_id: tracedecay_domain::ThreadId::new("thread.advisory-hook").unwrap(),
        turn_id: tracedecay_domain::TurnId::new("turn.advisory-hook").unwrap(),
        agent_id: tracedecay_domain::AgentInstanceId::new("agent.advisory-hook").unwrap(),
        logical_message_id: tracedecay_domain::MessageId::new("message.advisory-hook").unwrap(),
    }
}

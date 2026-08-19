//! `types` module test coverage (split from the former monolithic
//! `invocation::tests` module).

use super::*;

#[test]
fn hook_orchestration_admits_only_saved_edit_stop_and_explicit() {
    let saved = HookOrchestrationRequestV1::from_envelope(
        hook_envelope(HookEventV2::SavedEdit {
            file_id: [7; 16],
            changed_range_count: 1,
        }),
        &hook_binding(),
        Some(hook_lifecycle()),
        1,
        false,
    )
    .unwrap();
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

    let without_scout_lifecycle = HookOrchestrationRequestV1::from_envelope(
        hook_envelope(HookEventV2::SavedEdit {
            file_id: [7; 16],
            changed_range_count: 1,
        }),
        &hook_binding(),
        None,
        1,
        false,
    )
    .unwrap();
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
    let mut request = HookOrchestrationRequestV1::from_envelope(
        hook_envelope(HookEventV2::SavedEdit {
            file_id: [7; 16],
            changed_range_count: 1,
        }),
        &hook_binding(),
        None,
        1,
        false,
    )
    .unwrap();
    request.completion = Some(Arc::new(move || {
        observed_completions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        completion_notification.notify_one();
    }));
    // A hook from a *different native session* is real new work at a distinct
    // stable address, so it must contend for the single permit rather than
    // join or supersede the admitted boundary. (A newer boundary in the same
    // session supersedes instead; that path has its own coverage.)
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
            ran_synchronously.store(true, std::sync::atomic::Ordering::Release);
            ran.notify_one();
        }
    })
    .unwrap();
    assert!(register_hook_orchestration_runtime(
        [3; 16], [5; 16], &runtime
    ));

    assert_eq!(
        admit_registered_hook_orchestration(
            hook_envelope(HookEventV2::SavedEdit {
                file_id: [7; 16],
                changed_range_count: 1,
            }),
            hook_binding(),
            None,
            1,
            false,
            None,
        ),
        HookOrchestrationAdmissionV1::Enqueued
    );
    assert!(
        !ran_synchronously.load(std::sync::atomic::Ordering::Acquire),
        "hook admission must return before daemon-owned provider or model work begins"
    );
    ran.notified().await;
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
    let runtime = BoundedHookOrchestratorV1::new(1, move |_, _| {
        let release = Arc::clone(&work_release);
        let work_calls = Arc::clone(&observed_work_calls);
        async move {
            work_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            release.notified().await;
        }
    })
    .unwrap();
    let completions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let completed = Arc::new(tokio::sync::Notify::new());
    let mut request = HookOrchestrationRequestV1::from_envelope(
        hook_envelope(HookEventV2::SavedEdit {
            file_id: [7; 16],
            changed_range_count: 1,
        }),
        &hook_binding(),
        Some(hook_lifecycle()),
        1,
        false,
    )
    .unwrap();
    let first_completions = Arc::clone(&completions);
    let first_completed = Arc::clone(&completed);
    request.completion = Some(Arc::new(move || {
        first_completions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        first_completed.notify_one();
    }));
    let mut duplicate = request.clone();
    let duplicate_completions = Arc::clone(&completions);
    let duplicate_completed = Arc::clone(&completed);
    duplicate.completion = Some(Arc::new(move || {
        duplicate_completions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        duplicate_completed.notify_one();
    }));

    assert_eq!(
        runtime.admit(request),
        HookOrchestrationAdmissionV1::Enqueued
    );
    tokio::task::yield_now().await;
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
    let mut first = HookOrchestrationRequestV1::from_envelope(
        hook_envelope(HookEventV2::SavedEdit {
            file_id: [7; 16],
            changed_range_count: 1,
        }),
        &hook_binding(),
        Some(hook_lifecycle()),
        1,
        false,
    )
    .unwrap();
    first.completion = Some(Arc::new(move || {
        observed_first_completions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
    second.completion = Some(Arc::new(move || {
        observed_second_completions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        observed_second_completed.notify_one();
    }));
    assert_eq!(
        runtime.admit(second),
        HookOrchestrationAdmissionV1::Enqueued,
        "a newer edit/stop event at the same address must replace queued work"
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        second_completed.notified(),
    )
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
async fn retryable_hook_work_does_not_acknowledge_the_durable_admission() {
    let attempted = Arc::new(tokio::sync::Notify::new());
    let observed_attempted = Arc::clone(&attempted);
    let runtime = BoundedHookOrchestratorV1::new(1, move |_, _| {
        let attempted = Arc::clone(&observed_attempted);
        async move {
            attempted.notify_one();
            HookOrchestrationWorkOutcomeV1::RetryableFailure
        }
    })
    .unwrap();
    let completions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_completions = Arc::clone(&completions);
    let mut request = HookOrchestrationRequestV1::from_envelope(
        hook_envelope(HookEventV2::SavedEdit {
            file_id: [7; 16],
            changed_range_count: 1,
        }),
        &hook_binding(),
        Some(hook_lifecycle()),
        1,
        false,
    )
    .unwrap();
    request.completion = Some(Arc::new(move || {
        observed_completions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }));

    assert_eq!(
        runtime.admit(request),
        HookOrchestrationAdmissionV1::Enqueued
    );
    attempted.notified().await;
    tokio::task::yield_now().await;
    assert_eq!(
        completions.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "retryable producer failure must leave the durable hook pending for replay"
    );
}

/// The durable-replay half of the retryable contract: after a cycle fails
/// retryably and settles, the spool consumer re-admits the exact same
/// envelope. The orchestrator must run a fresh cycle for it — not treat the
/// settled failure as still in flight — and only the genuinely successful
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
            Some(Arc::new(move || {
                first_acknowledged.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })),
        ),
        HookOrchestrationAdmissionV1::Enqueued
    );
    first_attempted.notified().await;
    tokio::task::yield_now().await;
    assert_eq!(
        acknowledged.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "the failed cycle must not acknowledge the durable admission"
    );

    // The spool consumer replays the identical envelope until the producer
    // work genuinely terminates. A replay that races the settling failure
    // joins it and is dropped with it, so the consumer's next pass retries.
    let replay_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while acknowledged.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        assert!(
            std::time::Instant::now() < replay_deadline,
            "a replayed admission must complete a fresh cycle after a retryable failure"
        );
        let replay_acknowledged = Arc::clone(&acknowledged);
        assert_eq!(
            admit_registered_hook_orchestration(
                envelope.clone(),
                binding.clone(),
                Some(hook_lifecycle()),
                1,
                false,
                Some(Arc::new(move || {
                    replay_acknowledged.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                })),
            ),
            HookOrchestrationAdmissionV1::Enqueued
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        attempts.load(std::sync::atomic::Ordering::Relaxed) >= 2,
        "acknowledgement requires a fresh successful cycle, not the settled failure"
    );
    unregister_hook_orchestration_runtime([23; 16], [25; 16], &runtime);
}

/// Superseding a boundary whose provider work is wedged in nested blocking
/// work must settle the superseded admission's own receipt terminal by
/// dropping the cancelled work, never by polling it to completion: a wedged
/// provider cannot hold the durable receipt hostage. The gate stays closed
/// until after the terminal is observed, so the ordering is deterministic.
#[tokio::test]
async fn superseded_blocking_work_settles_its_receipt_without_awaiting_cancelled_work() {
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
                let task = tokio::task::spawn_blocking(move || {
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
    let mut first = HookOrchestrationRequestV1::from_envelope(
        hook_envelope(HookEventV2::SavedEdit {
            file_id: [7; 16],
            changed_range_count: 1,
        }),
        &hook_binding(),
        Some(hook_lifecycle()),
        1,
        false,
    )
    .unwrap();
    first.completion = Some(Arc::new(move || {
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
    // The gate is still closed: the terminal below can only come from the
    // superseded operation dropping its cancelled work.
    let terminal =
        tokio::time::timeout(std::time::Duration::from_secs(1), first_terminal.notified()).await;
    assert!(
        terminal.is_ok(),
        "the superseded operation must emit its own receipt terminal without awaiting cancelled work"
    );
    assert!(
        !blocking_finished.load(std::sync::atomic::Ordering::Acquire),
        "the gate is closed, so the nested blocking work cannot have finished"
    );

    // Release the wedged blocking work and the successor so shutdown joins
    // every thread this test started.
    let (released, changed) = &*blocking_gate;
    *released
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    changed.notify_all();
    successor_release.notify_one();
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
            Some(Arc::new(move || observed_queued_terminal.notify_one())),
        )),
        HookOrchestrationAdmissionV1::Enqueued
    );
    assert_eq!(
        runtime.admit(request([3; 16], None)),
        HookOrchestrationAdmissionV1::Enqueued
    );
    let terminal = tokio::time::timeout(
        std::time::Duration::from_millis(250),
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
    let completion = Arc::new(|| {}) as Arc<dyn Fn() + Send + Sync>;
    let request = |completion| {
        let mut request = HookOrchestrationRequestV1::from_envelope(
            hook_envelope(HookEventV2::SavedEdit {
                file_id: [7; 16],
                changed_range_count: 1,
            }),
            &hook_binding(),
            Some(hook_lifecycle()),
            1,
            false,
        )
        .unwrap();
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

    let work_dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_work_drop = Arc::clone(&work_dropped);
    let runtime = BoundedHookOrchestratorV1::new(1, move |_, _| PendingWork {
        dropped: Arc::clone(&observed_work_drop),
    })
    .unwrap();
    let completions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_completions = Arc::clone(&completions);
    let mut request = HookOrchestrationRequestV1::from_envelope(
        hook_envelope(HookEventV2::SavedEdit {
            file_id: [7; 16],
            changed_range_count: 1,
        }),
        &hook_binding(),
        Some(hook_lifecycle()),
        1,
        false,
    )
    .unwrap();
    request.completion = Some(Arc::new(move || {
        observed_completions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }));
    assert_eq!(
        runtime.admit(request),
        HookOrchestrationAdmissionV1::Enqueued
    );
    tokio::task::yield_now().await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feedback_admission_conflicts_construct_zero_losing_producers() {
    #[derive(Clone, Copy, Debug)]
    enum ConflictSlot {
        CallableCode,
        Feedback,
        FeedbackCycleInput,
    }

    let _pin = crate::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project root");
    let project_id = ProjectId::new("project.feedback.atomic-publication").expect("project id");
    let host = crate::host_admission::HostAdmissionTestRuntimeV1::project(
        crate::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered project runtime");
    let graph = host
        .initialize_project_graph_for_test(
            project.path(),
            crate::tracedecay::TraceDecayOpenOptions::default(),
        )
        .await
        .expect("initialized project graph");
    let database = graph.dashboard_database_guard().as_ref().clone();
    let scope = ResolvedScope::new(
        project_id.clone(),
        tracedecay_domain::RepositoryId::new("repository.feedback.atomic-publication")
            .expect("repository id"),
        tracedecay_domain::WorktreeId::new("worktree.feedback.atomic-publication")
            .expect("worktree id"),
        None,
    )
    .expect("resolved scope");
    let access = ProjectSourceAccessSnapshot {
        scope: scope.clone(),
        requester: ActorId::new("actor.feedback.atomic-publication").expect("actor"),
        binding: tracedecay_domain::configuration::ScopeSourceBinding::new(
            tracedecay_domain::SourceBindingId::new("binding.feedback.atomic-publication")
                .expect("binding"),
            tracedecay_domain::configuration::SourceKindV1::Cursor,
            tracedecay_domain::LocatorDigest::new(format!("sha256:{}", "a".repeat(64)))
                .expect("locator"),
            tracedecay_domain::configuration::AuthorityRef::Project(project_id.clone()),
        )
        .expect("source binding"),
        configuration_revision: ConfigurationRevisionId::new(
            "revision.feedback.atomic-publication",
        )
        .expect("configuration revision"),
        configuration_digest: canonical_sha256(&"feedback-atomic-configuration")
            .expect("configuration digest"),
        configuration_provenance_digest: canonical_sha256(
            &"feedback-atomic-configuration-provenance",
        )
        .expect("configuration provenance"),
        effective_capabilities: std::collections::BTreeSet::default(),
        grant_expires_at: UtcMicros(i64::MAX),
    };
    let incumbent_runtime = Arc::new(
        open_feedback_runtime(
            database.clone(),
            project.path(),
            scope.clone(),
            access.clone(),
        )
        .await
        .expect("incumbent feedback runtime"),
    );
    let publications = incumbent_runtime.publication_store();
    let incumbent_boot = publications
        .observation_read_model()
        .await
        .expect("incumbent observation model")
        .watermark
        .producer_boot_id
        .expect("incumbent producer boot");
    for conflict in [
        ConflictSlot::CallableCode,
        ConflictSlot::Feedback,
        ConflictSlot::FeedbackCycleInput,
    ] {
        let service = DaemonInvocationService::default();
        match conflict {
            ConflictSlot::CallableCode => {
                service
                    .project_runtimes
                    .publish(
                        project.path().to_path_buf(),
                        RegisteredCallableCodeRuntime {
                            authorization: DaemonCallableCodeAuthorizationSource::production(
                                project.path().to_path_buf(),
                                scope.clone(),
                                Arc::clone(graph.configuration_runtime()),
                            ),
                            scope: scope.clone(),
                        },
                    )
                    .await
                    .unwrap();
            }
            ConflictSlot::Feedback => {
                service
                    .project_runtimes
                    .publish(
                        project.path().to_path_buf(),
                        RegisteredFeedbackRuntime {
                            project_id: project_id.clone(),
                            runtime: Arc::clone(&incumbent_runtime),
                        },
                    )
                    .await
                    .unwrap();
            }
            ConflictSlot::FeedbackCycleInput => {
                let unavailable = Arc::new(UnavailableFeedbackCycleRuntimeV1::new(
                    project_id.clone(),
                    incumbent_runtime.source_observation_port(),
                ));
                service
                    .project_runtimes
                    .publish(
                        project.path().to_path_buf(),
                        Arc::new(SwitchableFeedbackCycleRuntimeV1::new(unavailable)),
                    )
                    .await
                    .unwrap();
            }
        }

        let registrar = DaemonFeedbackRuntimeRegistrar::new(&service);
        let result = registrar
            .open_and_register(
                database.clone(),
                project.path().to_path_buf(),
                scope.clone(),
                access.clone(),
                Arc::clone(graph.configuration_runtime()),
            )
            .await;
        assert!(
            matches!(
                result,
                Err(DaemonFeedbackRuntimeRegistrationError::AlreadyRegistered)
            ),
            "{conflict:?} must reject the whole feedback publication"
        );
        assert_eq!(
            registrar.producer_constructions.load(Ordering::SeqCst),
            0,
            "{conflict:?} must reject before constructing any producer"
        );
        let watermark = publications
            .observation_read_model()
            .await
            .expect("observation model after rejected publication")
            .watermark;
        assert_eq!(
            watermark.producer_boot_id,
            Some(incumbent_boot.clone()),
            "{conflict:?} must preserve the incumbent durable producer boot"
        );
        assert_eq!(
            service
                .project_runtimes
                .holds::<RegisteredCallableCodeRuntime>(project.path())
                .await,
            matches!(conflict, ConflictSlot::CallableCode)
        );
        assert_eq!(
            service
                .project_runtimes
                .holds::<RegisteredFeedbackRuntime>(project.path())
                .await,
            matches!(conflict, ConflictSlot::Feedback)
        );
        assert_eq!(
            service
                .project_runtimes
                .holds::<Arc<SwitchableFeedbackCycleRuntimeV1>>(project.path())
                .await,
            matches!(conflict, ConflictSlot::FeedbackCycleInput)
        );
    }

    drop(publications);
    drop(incumbent_runtime);
    let service = DaemonInvocationService::default();
    service
        .project_runtimes
        .publish(
            project.path().to_path_buf(),
            Arc::new(()) as Arc<dyn Any + Send + Sync>,
        )
        .await
        .unwrap();
    let (publication_ready, publication_is_ready) = tokio::sync::oneshot::channel();
    let (continue_publication, publication_may_continue) = tokio::sync::oneshot::channel();
    let (commit_starting, commit_is_starting) = tokio::sync::oneshot::channel();
    let gate = Arc::new(DaemonFeedbackPublicationTestGate::new(
        publication_ready,
        publication_may_continue,
    ));
    service
        .project_runtimes
        .arm_commit_starting(commit_starting);
    let registrar = DaemonFeedbackRuntimeRegistrar::new(&service).with_publication_gate(gate);
    let publisher_registrar = registrar.clone();
    let publisher_database = database.clone();
    let publisher_root = project.path().to_path_buf();
    let publisher_scope = scope.clone();
    let publisher_access = access.clone();
    let publisher_configuration = Arc::clone(graph.configuration_runtime());
    let publisher = tokio::spawn(async move {
        publisher_registrar
            .open_and_register(
                publisher_database,
                publisher_root,
                publisher_scope,
                publisher_access,
                publisher_configuration,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), publication_is_ready)
        .await
        .expect("feedback publication must reach its commit gate")
        .expect("publication-ready sender");

    let observer_registry = service.project_runtimes.clone();
    let observer_root = project.path().to_path_buf();
    let (observer_holding, observer_is_holding) = std::sync::mpsc::channel();
    let (release_observer, observer_may_release) = std::sync::mpsc::channel();
    let observer = tokio::spawn(async move {
        let mut samples = vec![
            observer_registry
                .feedback_publication_state(&observer_root)
                .await,
        ];
        observer_registry
            .read::<Arc<dyn Any + Send + Sync>, _, _>(&observer_root, move |_| {
                observer_holding
                    .send(())
                    .expect("observer-holding receiver");
                observer_may_release
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .expect("release-observer sender");
            })
            .await
            .expect("observer marker");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let sample = observer_registry
                    .feedback_publication_state(&observer_root)
                    .await;
                samples.push(sample);
                if sample == (true, true, true) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("observer must reach the complete feedback publication");
        samples
    });
    tokio::task::spawn_blocking(move || {
        observer_is_holding
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("observer must hold the registry lock");
    })
    .await
    .expect("observer-holding task");
    continue_publication
        .send(())
        .expect("continue-publication receiver");
    tokio::time::timeout(std::time::Duration::from_secs(2), commit_is_starting)
        .await
        .expect("feedback publication must start committing")
        .expect("commit-starting sender");
    release_observer
        .send(())
        .expect("release-observer receiver");

    tokio::time::timeout(std::time::Duration::from_secs(2), publisher)
        .await
        .expect("feedback publisher must not deadlock at commit")
        .expect("feedback publisher task")
        .expect("feedback publication");
    assert_eq!(
        registrar.producer_constructions.load(Ordering::SeqCst),
        1,
        "the successful registrar path must construct exactly one producer"
    );
    let samples = tokio::time::timeout(std::time::Duration::from_secs(2), observer)
        .await
        .expect("feedback observer must finish after publication")
        .expect("feedback observer task");
    assert_eq!(samples.last(), Some(&(true, true, true)));
    assert!(
        samples
            .iter()
            .all(|sample| matches!(sample, (false, false, false) | (true, true, true))),
        "the real registrar exposed a partial feedback runtime: {samples:?}"
    );
}

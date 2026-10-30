use tracedecay_domain::UtcMicros;
use tracedecay_sessions::admission::HostAdmissionStatus;

use super::super::ingest::complete_ingest_admission;
use super::super::test_support::*;
use super::super::*;
use super::*;

mod lifecycle_policy;
mod tool_dispatch;

#[test]
fn daemon_admission_is_idempotent_per_identity_and_conflicts_on_different_bytes() {
    let data_root = tempfile::tempdir().unwrap();
    let now = UtcMicros(1_000);

    let first =
        record_hook_v2_admission(data_root.path(), &admission_test_envelope(9, 7), now).unwrap();
    let duplicate =
        record_hook_v2_admission(data_root.path(), &admission_test_envelope(9, 7), now).unwrap();
    let conflict =
        record_hook_v2_admission(data_root.path(), &admission_test_envelope(9, 8), now).unwrap();
    let second =
        record_hook_v2_admission(data_root.path(), &admission_test_envelope(10, 7), now).unwrap();
    assert_eq!(
        first.decision,
        tracedecay_hooks::HookAdmissionDecisionV1::Admitted
    );
    assert_eq!(
        duplicate.decision,
        tracedecay_hooks::HookAdmissionDecisionV1::ExactDuplicate
    );
    assert_eq!(duplicate.order, first.order);
    assert_eq!(
        conflict.decision,
        tracedecay_hooks::HookAdmissionDecisionV1::Conflict
    );
    assert_eq!(conflict.order, first.order);
    assert_eq!(
        second.decision,
        tracedecay_hooks::HookAdmissionDecisionV1::Admitted
    );
    assert_eq!(second.order, first.order + 1);
    assert!(
        hook_v2_admission_ledger_root(
            data_root.path(),
            tracedecay_domain::NativeHostIdentityV1::ClaudeCode
        )
        .join("admissions.v2.log")
        .is_file()
    );
}

fn record_hook_v2_admission(
    data_root: &std::path::Path,
    envelope: &tracedecay_hooks::HookEventEnvelopeV2,
    now: UtcMicros,
) -> Option<tracedecay_hooks::HookAdmissionLedgerReceiptV1> {
    let staged = stage_hook_v2_admission(data_root, envelope, None, None, Some(0), now)?;
    staged.commit.wait().ok()?;
    Some(staged.receipt)
}

fn pending_work(data_root: &std::path::Path) -> Vec<tracedecay_hooks::HookEventEnvelopeV2> {
    hook_v2_pending_work_envelopes(
        data_root,
        tracedecay_domain::NativeHostIdentityV1::ClaudeCode,
        UtcMicros(1_000),
    )
    .unwrap()
}

fn restart_ledger(data_root: &std::path::Path) {
    forget_hook_v2_admission_ledger_for_test(
        data_root,
        tracedecay_domain::NativeHostIdentityV1::ClaudeCode,
    );
}

#[test]
fn producer_work_commits_with_its_admission_and_redrives_until_completed() {
    let data_root = tempfile::tempdir().unwrap();
    let now = UtcMicros(1_000);
    let envelope = admission_test_envelope(31, 7);
    let staged = stage_hook_v2_admission(
        data_root.path(),
        &envelope,
        Some(&envelope),
        None,
        Some(0),
        now,
    )
    .expect("ledger available");
    staged.commit.wait().unwrap();
    assert_eq!(
        staged.receipt.decision,
        tracedecay_hooks::HookAdmissionDecisionV1::Admitted
    );
    assert!(staged.receipt.work_settlement.is_none());

    restart_ledger(data_root.path());
    assert_eq!(pending_work(data_root.path()), vec![envelope.clone()]);
    let duplicate = record_hook_v2_admission(data_root.path(), &envelope, now).unwrap();
    assert_eq!(
        duplicate.decision,
        tracedecay_hooks::HookAdmissionDecisionV1::ExactDuplicate
    );
    assert!(duplicate.work_settlement.is_none());

    assert!(settle_hook_v2_pending_work(
        data_root.path(),
        &envelope,
        tracedecay_hooks::HookAdmissionWorkSettlementV1::Completed
    ));
    assert_eq!(pending_work(data_root.path()).len(), 0);
    restart_ledger(data_root.path());
    assert_eq!(pending_work(data_root.path()).len(), 0);
    assert!(
        record_hook_v2_admission(data_root.path(), &envelope, now)
            .unwrap()
            .work_settlement
            .is_some(),
        "producer-work completion must survive daemon restart"
    );
}

fn envelope_is_prompt(event: &tracedecay_hooks::HookEventV2) -> bool {
    matches!(event, tracedecay_hooks::HookEventV2::PromptBoundary)
}

/// Prompt and completed-tool boundaries retain their own intent work until
/// the registered orchestrator settles it; a started call is not a boundary.
#[tokio::test]
async fn prompt_and_tool_admissions_settle_their_durable_intent_work() {
    let data_root = tempfile::tempdir().unwrap();
    let binding = admission_test_binding(7);
    // Kiro admits prompt boundaries natively today; Claude gains them when
    // it registers its prompt hook.
    let kiro = tracedecay_domain::NativeHostIdentityV1::Kiro;
    let kiro_binding = tracedecay_hooks::HookScopeBindingV1 {
        host: kiro,
        capabilities: vec![tracedecay_hooks::HookCapabilityV1 {
            family: tracedecay_hooks::HookEventFamily::PromptBoundary,
            support: tracedecay_hooks::stock_event_support(
                kiro,
                tracedecay_hooks::HookEventFamily::PromptBoundary,
            ),
        }],
        ..binding.clone()
    };
    let (ran, mut runs) = tokio::sync::mpsc::unbounded_channel();
    let runtime = tracedecay_daemon_service::BoundedHookOrchestratorV1::new(
        1,
        move |request: tracedecay_daemon_service::HookOrchestrationRequestV1, _| {
            let ran = ran.clone();
            async move {
                ran.send((
                    request.hook.envelope().event_id[0],
                    request.trigger,
                    request.lifecycle.is_some(),
                ))
                .unwrap();
            }
        },
    )
    .unwrap();
    assert!(
        tracedecay_daemon_service::register_hook_orchestration_runtime([1; 16], [3; 16], &runtime)
    );
    let tool = |phase| tracedecay_hooks::HookEventV2::ToolLifecycle {
        tool_id: [9; 16],
        phase,
        effect_receipt_id: None,
    };
    for (event_id, event) in [
        (41, tracedecay_hooks::HookEventV2::PromptBoundary),
        (42, tool(tracedecay_hooks::HookLifecyclePhaseV1::Completed)),
        (43, tool(tracedecay_hooks::HookLifecyclePhaseV1::Failed)),
    ] {
        let mut envelope = admission_test_envelope(event_id, 7);
        let prompt = envelope_is_prompt(&event);
        envelope.event = event;
        if prompt {
            envelope.producer = kiro;
        }
        assert!(hook_v2_requires_producer_work(&envelope));
        let staged = stage_hook_v2_admission(
            data_root.path(),
            &envelope,
            Some(&envelope),
            None,
            Some(0),
            UtcMicros(1_000),
        )
        .expect("ledger available");
        staged.commit.wait().unwrap();
        let completion = hook_v2_work_completion(data_root.path(), &envelope);
        assert_eq!(
            tracedecay_daemon_service::admit_registered_hook_orchestration(
                envelope,
                if prompt {
                    kiro_binding.clone()
                } else {
                    binding.clone()
                },
                None,
                1,
                false,
                Some(staged.receipt.order),
                Some(completion),
            ),
            tracedecay_contracts::HookOrchestrationAdmissionV1::Enqueued
        );
        assert_eq!(
            runs.recv().await,
            Some((
                event_id,
                tracedecay_daemon_service::HookOrchestrationTriggerV1::IntentBoundary,
                false
            ))
        );
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while runtime.active_tasks() != 0
                || !hook_v2_pending_work_envelopes(
                    data_root.path(),
                    if prompt { kiro } else { binding.host },
                    UtcMicros(1_000),
                )
                .unwrap()
                .is_empty()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    assert_eq!(pending_work(data_root.path()), Vec::new());
    let mut started = admission_test_envelope(44, 7);
    started.event = tool(tracedecay_hooks::HookLifecyclePhaseV1::Started);
    assert_eq!(
        tracedecay_daemon_service::admit_registered_hook_orchestration(
            started,
            binding.clone(),
            None,
            1,
            false,
            None,
            None,
        ),
        tracedecay_contracts::HookOrchestrationAdmissionV1::UnsupportedTrigger
    );
    tracedecay_daemon_service::unregister_hook_orchestration_runtime([1; 16], [3; 16], &runtime);
    assert!(runtime.shutdown().await);
}

/// A native re-delivery carries the same event at a later `observed_at`. It
/// must converge on the first admission's pending work, not be refused as a
/// second record the drain could never settle.
#[test]
fn redelivered_producer_event_with_pending_work_is_an_exact_duplicate() {
    let data_root = tempfile::tempdir().unwrap();
    let envelope = admission_test_envelope(33, 7);
    let mut redelivered = envelope.clone();
    redelivered.observed_at = UtcMicros(envelope.observed_at.0 + 5_000);
    let first = stage_hook_v2_admission(
        data_root.path(),
        &envelope,
        Some(&envelope),
        None,
        Some(0),
        UtcMicros(1_000),
    )
    .unwrap();
    first.commit.wait().unwrap();

    let again = stage_hook_v2_admission(
        data_root.path(),
        &redelivered,
        Some(&redelivered),
        None,
        Some(0),
        UtcMicros(2_000),
    )
    .expect("a redelivery is not backpressure");
    again.commit.wait().unwrap();

    assert_eq!(
        again.receipt.decision,
        tracedecay_hooks::HookAdmissionDecisionV1::ExactDuplicate
    );
    assert_eq!(pending_work(data_root.path()), vec![envelope.clone()]);
    assert!(settle_hook_v2_pending_work(
        data_root.path(),
        &redelivered,
        tracedecay_hooks::HookAdmissionWorkSettlementV1::Completed,
    ));
    assert_eq!(pending_work(data_root.path()).len(), 0);
}

/// A long-lived daemon admits for more project/host pairs over its lifetime
/// than it holds ledgers open at once.
#[test]
fn admission_ledgers_cycle_past_the_open_bound_without_restart() {
    let now = hook_now();
    let envelope = admission_test_envelope(35, 7);
    let roots = (0..MAX_OPEN_HOOK_V2_ADMISSION_LEDGERS + 8)
        .map(|_| tempfile::tempdir().unwrap())
        .collect::<Vec<_>>();
    for (pair, root) in roots.iter().enumerate() {
        let staged =
            stage_hook_v2_admission(root.path(), &envelope, Some(&envelope), None, Some(0), now)
                .unwrap_or_else(|| panic!("project/host pair {pair} must admit without restart"));
        staged.commit.wait().unwrap();
        assert_eq!(
            staged.receipt.decision,
            tracedecay_hooks::HookAdmissionDecisionV1::Admitted
        );
    }

    let first = roots[0].path();
    assert_eq!(
        record_hook_v2_admission(first, &envelope, now)
            .unwrap()
            .decision,
        tracedecay_hooks::HookAdmissionDecisionV1::ExactDuplicate
    );
    assert_eq!(
        record_hook_v2_admission(first, &admission_test_envelope(35, 8), now)
            .unwrap()
            .decision,
        tracedecay_hooks::HookAdmissionDecisionV1::Conflict
    );
    restart_ledger(first);
    assert_eq!(pending_work(first), vec![envelope.clone()]);
    restart_ledger(first);
    assert!(
        settle_hook_v2_pending_work(
            first,
            &envelope,
            tracedecay_hooks::HookAdmissionWorkSettlementV1::Completed
        ),
        "a released ledger reopens to record completion"
    );
    assert_eq!(pending_work(first).len(), 0);
}

/// Only a ledger whose commits have all settled is released for another pair,
/// and it reopens with its records and owed work intact.
#[test]
fn a_ledger_with_a_commit_in_flight_keeps_its_writer_fence() {
    let host = tracedecay_domain::NativeHostIdentityV1::ClaudeCode;
    let now = UtcMicros(1_000);
    let envelope = admission_test_envelope(36, 7);
    let ledgers = HookV2AdmissionLedgers::new(2);
    let [a, b, c, d] = std::array::from_fn(|_| tempfile::tempdir().unwrap());
    let stage = |root: &std::path::Path, envelope: &tracedecay_hooks::HookEventEnvelopeV2| {
        ledgers
            .with_ledger(
                hook_v2_admission_ledger_root(root, host),
                host,
                now,
                |ledger| ledger.stage_admission(envelope, Some(envelope), None, Some(0), now),
            )
            .map(|staged| staged.unwrap())
    };
    let lock_is_free = |root: &std::path::Path| match tracedecay_hooks::HookAdmissionLedgerV1::open(
        hook_v2_admission_ledger_root(root, host),
        host,
        tracedecay_hooks::HookAdmissionLedgerLimitsV1::stock(),
        now,
    ) {
        Ok(_) => true,
        Err(tracedecay_hooks::HookAdmissionLedgerError::Busy) => false,
        Err(error) => panic!("unexpected ledger open failure: {error}"),
    };

    let a_in_flight = stage(a.path(), &envelope).unwrap();
    stage(b.path(), &envelope).unwrap().commit.wait().unwrap();
    let c_in_flight = stage(c.path(), &envelope).unwrap();
    assert!(!lock_is_free(a.path()), "an unsettled ledger stays open");
    assert!(lock_is_free(b.path()), "the settled ledger was released");

    assert!(matches!(
        stage(d.path(), &envelope),
        Err(HookV2AdmissionLedgerUnavailable::AtCapacity)
    ));

    a_in_flight.commit.wait().unwrap();
    stage(d.path(), &envelope).unwrap().commit.wait().unwrap();
    assert!(
        lock_is_free(a.path()),
        "a settled ledger is released on demand"
    );
    let duplicate = stage(a.path(), &envelope).unwrap();
    duplicate.commit.wait().unwrap();
    assert_eq!(
        duplicate.receipt.decision,
        tracedecay_hooks::HookAdmissionDecisionV1::ExactDuplicate
    );
    assert!(duplicate.receipt.work_settlement.is_none());
    let conflict = stage(a.path(), &admission_test_envelope(36, 8)).unwrap();
    conflict.commit.wait().unwrap();
    assert_eq!(
        conflict.receipt.decision,
        tracedecay_hooks::HookAdmissionDecisionV1::Conflict
    );
    c_in_flight.commit.wait().unwrap();
}

#[test]
fn bounded_snapshot_deferral_is_typed_retryable_backpressure() {
    let deferred = complete_ingest_admission(
        HostAdmissionOutcome::accepted_for_replay(),
        true,
        false,
        true,
    );
    assert_eq!(deferred.status, HostAdmissionStatus::Backpressured);
    assert!(deferred.retryable);
    assert_eq!(deferred.reason_code, Some("ingest_pass_backpressured"));

    let completed = complete_ingest_admission(
        HostAdmissionOutcome::accepted_for_replay(),
        true,
        false,
        false,
    );
    assert_eq!(completed.status, HostAdmissionStatus::Committed);
}

#[test]
fn hook_v2_binding_capability_rejection_requires_authoritative_catchup() {
    let mut envelope = hook_v2_envelope_for_test();
    envelope.event = tracedecay_hooks::HookEventV2::PromptBoundary;

    assert!(matches!(
        classify_hook_v2_binding(
            &envelope,
            tracedecay_hooks::HookConfigurationReadOutcomeV1::Bound(hook_v2_snapshot()),
        ),
        HookV2BindingAdmission::CatchupRequired
    ));
}

#[test]
fn hook_v2_missing_configuration_remains_transiently_unavailable() {
    assert!(matches!(
        classify_hook_v2_binding(
            &hook_v2_envelope_for_test(),
            tracedecay_hooks::HookConfigurationReadOutcomeV1::Missing,
        ),
        HookV2BindingAdmission::Unavailable
    ));
}

#[test]
fn github_stack_wakeup_is_cursor_desktop_only_and_first_admission_only() {
    assert!(cursor_stack_wakeup_allowed(
        true,
        tracedecay_domain::NativeHostIdentityV1::CursorDesktop,
    ));
    assert!(!cursor_stack_wakeup_allowed(
        false,
        tracedecay_domain::NativeHostIdentityV1::CursorDesktop,
    ));
    for host in [
        tracedecay_domain::NativeHostIdentityV1::Codex,
        tracedecay_domain::NativeHostIdentityV1::ClaudeCode,
        tracedecay_domain::NativeHostIdentityV1::CursorCloud,
    ] {
        assert!(!cursor_stack_wakeup_allowed(true, host), "{host:?}");
    }
}

/// A private profile root under `dir`, as profile identity requires.
fn private_profile_root(dir: &std::path::Path) -> std::path::PathBuf {
    let profile_root = dir.join(".tracedecay");
    tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(&profile_root)
        .expect("private profile root");
    profile_root
}

#[test]
fn profile_scoped_native_admission_is_idempotent_in_the_authenticated_profile() {
    let profile = tempfile::tempdir().unwrap();
    let profile_root = private_profile_root(profile.path());
    let identity =
        tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root).unwrap();
    let decoded = tracedecay_hooks::decode_native_hook_event(
        tracedecay_domain::NativeHostIdentityV1::ClaudeCode,
        br#"{"hook_event_name":"SessionStart"}"#,
    )
    .unwrap();
    let admission = serde_json::json!(tracedecay_hooks::ProfileScopedNativeHookAdmissionV1 {
        decoded,
        material: tracedecay_hooks::NativeEnvelopeMaterialV1 {
            event_id: [7; 16],
            protected_session_id: [8; 32],
            observed_at: UtcMicros(1_000),
            tool_id: None,
            effect_receipt_id: None,
            file_id: None,
            changed_range_count: 0,
        },
    });

    let first = hook_v2_profile_admit(admission.clone(), &profile_root, &identity).unwrap();
    let duplicate = hook_v2_profile_admit(admission, &profile_root, &identity).unwrap();
    assert_eq!(
        first,
        HookV2ProfileAdmissionResultV1::Accepted {
            disposition: HookRuntimeDispositionV1::Accepted,
        }
    );
    assert_eq!(
        duplicate,
        HookV2ProfileAdmissionResultV1::ExactDuplicate {
            disposition: HookRuntimeDispositionV1::Accepted,
        }
    );
    assert!(
        profile_root
            .join("hook-v2-profile-admissions")
            .join("claude")
            .join("admissions.v2.log")
            .is_file()
    );
}

/// Profile-scoped admissions arrive on concurrent daemon requests. Each must
/// be admitted exactly once through the daemon-owned ledger; none may be
/// answered `unavailable` because a sibling request held the writer lock.
#[test]
fn concurrent_profile_scoped_admissions_are_all_recorded() {
    const WRITERS: u8 = 16;
    let profile = tempfile::tempdir().unwrap();
    let profile_root = private_profile_root(profile.path());
    let identity =
        tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root).unwrap();
    let admission = |event: u8| {
        serde_json::json!(tracedecay_hooks::ProfileScopedNativeHookAdmissionV1 {
            decoded: tracedecay_hooks::decode_native_hook_event(
                tracedecay_domain::NativeHostIdentityV1::ClaudeCode,
                br#"{"hook_event_name":"SessionStart"}"#,
            )
            .unwrap(),
            material: tracedecay_hooks::NativeEnvelopeMaterialV1 {
                event_id: [event; 16],
                protected_session_id: [8; 32],
                observed_at: UtcMicros(1_000),
                tool_id: None,
                effect_receipt_id: None,
                file_id: None,
                changed_range_count: 0,
            },
        })
    };
    let admit_all = || {
        let barrier = std::sync::Barrier::new(usize::from(WRITERS));
        std::thread::scope(|scope| {
            let workers: Vec<_> = (1..=WRITERS)
                .map(|event| {
                    let (barrier, identity, profile_root) = (&barrier, &identity, &profile_root);
                    let args = admission(event);
                    scope.spawn(move || {
                        barrier.wait();
                        serde_json::to_value(
                            hook_v2_profile_admit(args, profile_root, identity).unwrap(),
                        )
                        .unwrap()["status"]
                            .as_str()
                            .unwrap()
                            .to_owned()
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        })
    };

    assert_eq!(admit_all(), vec!["accepted"; usize::from(WRITERS)]);
    assert_eq!(
        admit_all(),
        vec!["exact_duplicate"; usize::from(WRITERS)],
        "every concurrent admission was durably recorded exactly once"
    );
}

#[test]
fn an_admission_ledger_root_that_is_not_a_directory_is_unavailable_not_empty() {
    let data_root = tempfile::tempdir().unwrap();
    let host = tracedecay_domain::NativeHostIdentityV1::ClaudeCode;
    let ledger_root = hook_v2_admission_ledger_root(data_root.path(), host);
    std::fs::create_dir_all(ledger_root.parent().unwrap()).unwrap();
    std::fs::write(&ledger_root, b"not a ledger").unwrap();

    let pending = hook_v2_pending_work_envelopes(data_root.path(), host, UtcMicros(1_000));
    assert!(
        matches!(
            pending,
            Err(HookV2AdmissionLedgerUnavailable::Ledger(
                tracedecay_hooks::HookAdmissionLedgerError::UnsafePath
            ))
        ),
        "an unusable admission ledger is a typed failure, not nothing owed: {pending:?}"
    );
}

/// A Claude tool call mints its lifecycle only for the intent pass. With the
/// pass disabled, the default, it writes nothing inside the hook's budget;
/// Kimi binds its lifecycle for every Scout claim, so the gate leaves it be.
#[tokio::test]
async fn claude_tool_calls_mint_a_lifecycle_only_while_the_intent_pass_is_enabled() {
    use tracedecay_daemon_service::context_scout_lifecycle::{
        AuthorityRegistrationV1, lookup_registered_context_scout_lifecycle,
        register_context_scout_lifecycle_authority,
    };
    use tracedecay_domain::{NativeHostIdentityV1, ObservationSourceRangeV1, ProjectId, SessionId};

    let temporary = tempfile::tempdir().unwrap();
    let project_id = ProjectId::new("project.claude-lifecycle-gate").unwrap();
    let runtime =
        tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1::project(
            temporary.path().join("profile"),
            temporary.path().join("project"),
            project_id.clone(),
        )
        .await
        .unwrap();
    let sessions = runtime
        .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Project)
        .unwrap();
    let (hook_project_id, hook_worktree_id) = ([81; 16], [82; 16]);
    assert_eq!(
        register_context_scout_lifecycle_authority(
            hook_project_id,
            hook_worktree_id,
            project_id,
            tracedecay_domain::WorktreeId::new("worktree.claude-lifecycle-gate").unwrap(),
            &sessions,
        ),
        AuthorityRegistrationV1::Registered
    );
    let background_cpu = runtime.background_cpu();
    let mount = HookV2LifecycleMountV1 {
        lifecycle: None,
        project_sessions: &sessions,
        background_cpu: Some(&background_cpu),
    };
    let minted = |session: &'static str| async move {
        lookup_registered_context_scout_lifecycle(
            hook_project_id,
            hook_worktree_id,
            &SessionId::new(session.to_owned()).unwrap(),
        )
        .await
        .is_some()
    };
    let tool_call = |host: NativeHostIdentityV1, session: &'static str, intent_enabled: bool| {
        let mount = &mount;
        async move {
            let lifecycle = tracedecay_agent_hosts::hooks::NativeContextScoutLifecycleV1::new(
                session,
                "call.gate",
                [1; 16],
            )
            .unwrap();
            admit_gated_native_lifecycle(
                mount,
                ProviderId::new(host.hook_key()).unwrap(),
                &lifecycle,
                ObservationSourceRangeV1::new(1, 2).unwrap(),
                host,
                intent_enabled,
            )
            .await
        }
    };

    assert_eq!(
        tool_call(
            NativeHostIdentityV1::ClaudeCode,
            "session.claude.off",
            false
        )
        .await,
        NativeLifecycleAdmissionV1::Ready,
    );
    assert!(
        !minted("session.claude.off").await,
        "a Claude tool call with the intent pass disabled writes no lifecycle"
    );
    assert_eq!(
        tool_call(NativeHostIdentityV1::ClaudeCode, "session.claude.on", true).await,
        NativeLifecycleAdmissionV1::Ready
    );
    assert!(
        minted("session.claude.on").await,
        "a Claude tool call with the intent pass enabled mints its lifecycle"
    );
    assert_eq!(
        tool_call(NativeHostIdentityV1::KimiCode, "session.kimi.off", false).await,
        NativeLifecycleAdmissionV1::Ready
    );
    assert!(
        minted("session.kimi.off").await,
        "Kimi keeps minting its lifecycle whatever the intent pass says"
    );
}

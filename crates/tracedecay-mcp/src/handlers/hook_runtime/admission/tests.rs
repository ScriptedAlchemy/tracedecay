use tracedecay_domain::UtcMicros;
use tracedecay_sessions::admission::HostAdmissionStatus;

use super::super::ingest::complete_ingest_admission;
use super::super::test_support::*;
use super::super::*;
use super::*;

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
    let staged = stage_hook_v2_admission(data_root, envelope, None, now)?;
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
    let staged = stage_hook_v2_admission(data_root.path(), &envelope, Some(&envelope), now)
        .expect("ledger available");
    staged.commit.wait().unwrap();
    assert_eq!(
        staged.receipt.decision,
        tracedecay_hooks::HookAdmissionDecisionV1::Admitted
    );
    assert!(!staged.receipt.work_completed);

    restart_ledger(data_root.path());
    assert_eq!(pending_work(data_root.path()), vec![envelope.clone()]);
    let duplicate = record_hook_v2_admission(data_root.path(), &envelope, now).unwrap();
    assert_eq!(
        duplicate.decision,
        tracedecay_hooks::HookAdmissionDecisionV1::ExactDuplicate
    );
    assert!(!duplicate.work_completed);

    assert!(complete_hook_v2_pending_work(data_root.path(), &envelope));
    assert!(pending_work(data_root.path()).is_empty());
    restart_ledger(data_root.path());
    assert!(pending_work(data_root.path()).is_empty());
    assert!(
        record_hook_v2_admission(data_root.path(), &envelope, now)
            .unwrap()
            .work_completed,
        "producer-work completion must survive daemon restart"
    );
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
        UtcMicros(1_000),
    )
    .unwrap();
    first.commit.wait().unwrap();

    let again = stage_hook_v2_admission(
        data_root.path(),
        &redelivered,
        Some(&redelivered),
        UtcMicros(2_000),
    )
    .expect("a redelivery is not backpressure");
    again.commit.wait().unwrap();

    assert_eq!(
        again.receipt.decision,
        tracedecay_hooks::HookAdmissionDecisionV1::ExactDuplicate
    );
    assert_eq!(pending_work(data_root.path()), vec![envelope.clone()]);
    assert!(complete_hook_v2_pending_work(
        data_root.path(),
        &redelivered
    ));
    assert!(pending_work(data_root.path()).is_empty());
}

#[test]
fn pre_ledger_pending_work_is_refused_for_reset_not_adopted() {
    let data_root = tempfile::tempdir().unwrap();
    let now = UtcMicros(1_000);
    let host = tracedecay_domain::NativeHostIdentityV1::ClaudeCode;
    let provider = admission_test_envelope(34, 7);
    let pre_ledger_root = data_root.path().join("hook-v2-pending-work").join("claude");
    {
        let (mut spool, _) = tracedecay_hooks::HookSpoolV1::open(
            &pre_ledger_root,
            tracedecay_hooks::HookSpoolConfigV1::stock(host),
            now,
        )
        .unwrap();
        spool
            .append(provider.clone(), &admission_test_binding(7), now)
            .unwrap();
        spool.commit().unwrap();
    }

    let pending = hook_v2_pending_work_envelopes(data_root.path(), host, now);
    assert!(
        matches!(
            pending,
            Err(HookV2AdmissionLedgerUnavailable::Ledger(
                tracedecay_hooks::HookAdmissionLedgerError::ResetRequired
            ))
        ),
        "pre-ledger pending work is a reset refusal, not an empty drain: {pending:?}"
    );
    assert!(record_hook_v2_admission(data_root.path(), &provider, now).is_none());
    assert!(pre_ledger_root.is_dir());
    assert_eq!(
        tracedecay_hooks::hook_admission_reset_required_roots(data_root.path()),
        vec![pre_ledger_root.clone()]
    );

    std::fs::remove_dir_all(&pre_ledger_root).unwrap();
    let admitted = record_hook_v2_admission(data_root.path(), &provider, now).unwrap();
    assert_eq!(
        admitted.decision,
        tracedecay_hooks::HookAdmissionDecisionV1::Admitted
    );
    assert_eq!(
        tracedecay_hooks::hook_admission_reset_required_roots(data_root.path()),
        Vec::<std::path::PathBuf>::new()
    );
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

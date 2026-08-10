use tracedecay_domain::UtcMicros;
use tracedecay_usecases::host_admission::HostAdmissionStatus;

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
        hook_v2_admission_ledger_root(data_root.path(), tracedecay_hooks::HookHostV1::ClaudeCode)
            .join("admissions.v1.bin")
            .is_file()
    );
}

#[test]
fn completion_persists_before_pending_ack_failure_and_cleanup_retries() {
    let data_root = tempfile::tempdir().unwrap();
    let now = UtcMicros(1_000);
    let envelope = admission_test_envelope(31, 7);
    let binding = admission_test_binding(7);
    let first = record_hook_v2_admission(data_root.path(), &envelope, now).unwrap();
    assert!(!first.work_completed);
    let unavailable =
        retain_hook_v2_pending_work(data_root.path(), &envelope, &envelope, &binding, now)
            .expect("durable pending work");
    drop(unavailable);
    assert_eq!(
        hook_v2_pending_work_envelopes(
            data_root.path(),
            tracedecay_hooks::HookHostV1::ClaudeCode,
            now,
        ),
        std::slice::from_ref(&envelope)
    );
    let pending_sequence = {
        let (mut spool, _) = tracedecay_hooks::HookSpoolV1::open(
            hook_v2_pending_work_root(data_root.path(), tracedecay_hooks::HookHostV1::ClaudeCode),
            tracedecay_hooks::HookSpoolConfigV1::stock(tracedecay_hooks::HookHostV1::ClaudeCode),
            now,
        )
        .unwrap();
        let batch = spool.claim_replay_batches(now, 1).unwrap().remove(0);
        let sequence = batch.records[0].sequence;
        spool.release_replay_claim(batch.claim_id).unwrap();
        sequence
    };

    assert!(
        !complete_hook_v2_pending_work(data_root.path(), &envelope, pending_sequence + 1, now),
        "invalid acknowledgement must retain pending work"
    );

    let duplicate = record_hook_v2_admission(data_root.path(), &envelope, now).unwrap();
    assert_eq!(
        duplicate.decision,
        tracedecay_hooks::HookAdmissionDecisionV1::ExactDuplicate
    );
    assert!(
        duplicate.work_completed,
        "completed producer work must stay fenced when pending acknowledgement fails"
    );
    assert_eq!(
        hook_v2_pending_work_envelopes(
            data_root.path(),
            tracedecay_hooks::HookHostV1::ClaudeCode,
            now,
        ),
        std::slice::from_ref(&envelope)
    );

    assert!(complete_hook_v2_pending_work(
        data_root.path(),
        &envelope,
        pending_sequence,
        now,
    ));
    assert!(
        hook_v2_pending_work_envelopes(
            data_root.path(),
            tracedecay_hooks::HookHostV1::ClaudeCode,
            now,
        )
        .is_empty()
    );
    assert!(
        record_hook_v2_admission(data_root.path(), &envelope, now)
            .unwrap()
            .work_completed
    );
    forget_hook_v2_admission_ledger_for_test(
        data_root.path(),
        tracedecay_hooks::HookHostV1::ClaudeCode,
    );
    assert!(
        record_hook_v2_admission(data_root.path(), &envelope, now)
            .unwrap()
            .work_completed,
        "producer-work completion must survive daemon restart"
    );
}

#[test]
fn completed_restart_duplicate_cleans_pending_without_work_redrive() {
    let data_root = tempfile::tempdir().unwrap();
    let now = UtcMicros(1_000);
    let envelope = admission_test_envelope(32, 7);
    let binding = admission_test_binding(7);
    record_hook_v2_admission(data_root.path(), &envelope, now).unwrap();
    let completion =
        retain_hook_v2_pending_work(data_root.path(), &envelope, &envelope, &binding, now)
            .expect("durable pending work");
    {
        let key = (
            data_root.path().to_path_buf(),
            tracedecay_hooks::HookHostV1::ClaudeCode.hook_key(),
        );
        let mut ledgers = hook_v2_admission_ledgers().lock().unwrap();
        assert!(
            ledgers
                .get_mut(&key)
                .unwrap()
                .mark_work_completed(&envelope)
                .unwrap()
        );
    }
    drop(completion);
    forget_hook_v2_admission_ledger_for_test(
        data_root.path(),
        tracedecay_hooks::HookHostV1::ClaudeCode,
    );

    let duplicate = record_hook_v2_admission(data_root.path(), &envelope, now).unwrap();
    assert_eq!(
        duplicate.decision,
        tracedecay_hooks::HookAdmissionDecisionV1::ExactDuplicate
    );
    assert!(duplicate.work_completed);
    let cleanup =
        retain_hook_v2_pending_work(data_root.path(), &envelope, &envelope, &binding, now)
            .expect("completed duplicate pending cleanup");
    cleanup();

    assert!(
        hook_v2_pending_work_envelopes(
            data_root.path(),
            tracedecay_hooks::HookHostV1::ClaudeCode,
            now,
        )
        .is_empty(),
        "completed duplicate must clear pending transport state without rerunning producer work"
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
fn hook_v2_binding_epoch_mismatch_requires_authoritative_catchup() {
    let mut envelope = hook_v2_envelope_for_test();
    envelope.worktree_epoch += 1;

    assert!(matches!(
        classify_hook_v2_binding(
            &envelope,
            tracedecay_hooks::HookConfigurationReadOutcomeV1::Bound(hook_v2_snapshot()),
        ),
        HookV2BindingAdmission::CatchupRequired
    ));
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
        tracedecay_hooks::HookHostV1::CursorDesktop,
    ));
    assert!(!cursor_stack_wakeup_allowed(
        false,
        tracedecay_hooks::HookHostV1::CursorDesktop,
    ));
    for host in [
        tracedecay_hooks::HookHostV1::Codex,
        tracedecay_hooks::HookHostV1::ClaudeCode,
        tracedecay_hooks::HookHostV1::CursorCloud,
    ] {
        assert!(!cursor_stack_wakeup_allowed(true, host), "{host:?}");
    }
}

#[test]
fn hook_v2_catchup_response_propagates_transport_disposition() {
    let response = hook_v2_catchup_response("hook_v2_admit");
    assert_eq!(response["status"], "rejected");
    assert_eq!(response["disposition"], "catchup_required");
}

#[test]
fn profile_scoped_native_admission_is_idempotent_in_the_authenticated_profile() {
    let _profile = crate::config::PinnedUserDataDir::new();
    let profile_root = crate::storage::default_profile_root().unwrap();
    let identity = crate::daemon::profile_identity::load_or_create(&profile_root).unwrap();
    let decoded = tracedecay_hooks::decode_native_hook_event(
        tracedecay_hooks::HookHostV1::ClaudeCode,
        br#"{"hook_event_name":"SessionStart"}"#,
    )
    .unwrap();
    let args = serde_json::json!({
        "admission": tracedecay_hooks::ProfileScopedNativeHookAdmissionV1 {
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
        },
    });

    let first =
        hook_v2_profile_admit(&args, "hook_v2_profile_admit", &profile_root, &identity).unwrap();
    let duplicate =
        hook_v2_profile_admit(&args, "hook_v2_profile_admit", &profile_root, &identity).unwrap();
    assert_eq!(first["status"], "accepted");
    assert_eq!(first["disposition"], "accepted");
    assert_eq!(duplicate["status"], "exact_duplicate");
    assert_eq!(duplicate["disposition"], "accepted");
    assert!(
        profile_root
            .join("hook-v2-profile-admissions")
            .join("claude")
            .join("admissions.v1.bin")
            .is_file()
    );
}

use std::sync::Mutex as StdMutex;

use super::super::*;
use crate::application::host_admission::HostAdmissionScope;
use tracedecay_domain::{ObservationSourceRangeV1, ProjectId, ProviderId, SessionId, UtcMicros};

use super::super::test_support::*;
use super::*;

static RETAINED_CLAIM_TEST_LOCK: StdMutex<()> = StdMutex::new(());

#[test]
fn exact_retained_claim_lookup_commits_beyond_thirty_two_entries() {
    let _guard = RETAINED_CLAIM_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let project_id = [201; 16];
    for id in 1..=40 {
        assert!(
            retain_hook_v2_delivery_claim(project_id, retained_claim(id), UtcMicros(1)).is_ok()
        );
    }
    for id in 1..=40 {
        assert_eq!(
            lookup_hook_v2_delivery_claim(project_id, [id; 16])
                .expect("exact retained claim")
                .entry
                .envelope
                .envelope_id,
            [id; 16]
        );
        remove_hook_v2_delivery_claim(project_id, [id; 16]);
    }
}

#[test]
fn retained_claims_backpressure_at_a_deterministic_bound() {
    let _guard = RETAINED_CLAIM_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for index in 0..MAX_RETAINED_HOOK_V2_DELIVERY_CLAIMS as u16 {
        let mut project_id = [202; 16];
        project_id[0] = (index >> 8) as u8;
        assert!(
            retain_hook_v2_delivery_claim(project_id, retained_claim(index as u8), UtcMicros(1),)
                .is_ok()
        );
    }
    assert!(retain_hook_v2_delivery_claim([203; 16], retained_claim(1), UtcMicros(1)).is_err());
    for index in 0..MAX_RETAINED_HOOK_V2_DELIVERY_CLAIMS as u16 {
        let mut project_id = [202; 16];
        project_id[0] = (index >> 8) as u8;
        remove_hook_v2_delivery_claim(project_id, [index as u8; 16]);
    }
}

#[test]
fn receipt_outcomes_release_claims_and_only_retry_unavailable() {
    use crate::agents::context_scout_v2::ContextScoutDurableStoreOutcomeV1;

    let _guard = RETAINED_CLAIM_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let project_id = [204; 16];
    for (id, outcome, retryable) in [
        (1, ContextScoutDurableStoreOutcomeV1::Stored, false),
        (2, ContextScoutDurableStoreOutcomeV1::Duplicate, false),
        (3, ContextScoutDurableStoreOutcomeV1::Superseded, false),
        (4, ContextScoutDurableStoreOutcomeV1::Unavailable, true),
    ] {
        assert!(
            retain_hook_v2_delivery_claim(project_id, retained_claim(id), UtcMicros(1)).is_ok()
        );
        assert_eq!(
            release_hook_v2_delivery_claim(project_id, [id; 16], outcome),
            retryable
        );
        assert!(lookup_hook_v2_delivery_claim(project_id, [id; 16]).is_none());
    }
}

#[test]
fn scout_read_actions_are_closed_and_read_only() {
    for action in [
        "hook_v2_scout_recent",
        "hook_v2_scout_explain",
        "hook_v2_scout_capability",
        "hook_v2_scout_budget",
    ] {
        assert!(ContextScoutReadSurfaceV1::from_action(action).is_some());
    }
    assert!(ContextScoutReadSurfaceV1::from_action("hook_v2_scout_apply").is_none());
}

#[test]
fn hook_v2_scout_prepare_accepts_no_caller_candidates() {
    let response = orchestration_response(
        "hook_v2_scout_prepare",
        crate::daemon::Pr13HookOrchestrationAdmissionV1::Unavailable,
    );
    assert_eq!(response["status"], "unavailable");
    assert_eq!(response["reason"], "orchestration_unavailable");
    assert!(!response.to_string().contains("candidate"));
    assert!(!response.to_string().contains("control"));
}

#[test]
fn hook_v2_native_session_requires_exact_protected_locator() {
    let session_id = "native-session-1";
    let mut envelope = hook_v2_envelope_for_test();
    envelope.protected_session_id =
        crate::hooks::hook_v2_protected_session_id_for_native(session_id);
    assert_eq!(
        hook_v2_native_session_id(&json!({ "native_session_id": session_id }), &envelope)
            .as_ref()
            .map(SessionId::as_str),
        Some(session_id)
    );

    envelope.protected_session_id = [9; 32];
    assert!(
        hook_v2_native_session_id(&json!({ "native_session_id": session_id }), &envelope).is_none()
    );
}

#[tokio::test]
async fn kimi_and_opencode_queued_lifecycle_delivery_prepares_scout_lookup() {
    let temporary = tempfile::tempdir().unwrap();
    let project_id = ProjectId::new("project.native-hook-scout").unwrap();
    let runtime = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
        temporary.path().join("profile"),
        temporary.path().join("project"),
        project_id.clone(),
    )
    .await
    .unwrap();
    let sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();
    let worktree_id = tracedecay_domain::WorktreeId::new("worktree.native-hook-scout").unwrap();
    let hook_project_id = [71; 16];
    let hook_worktree_id = [72; 16];
    assert!(
        crate::daemon::context_scout_lifecycle::register_context_scout_lifecycle_authority(
            hook_project_id,
            hook_worktree_id,
            project_id,
            worktree_id,
            &sessions,
        )
        .is_bound()
    );

    for (provider, session, first_call, latest_call) in [
        (
            "kimi",
            "session.kimi.native",
            "call.kimi.first",
            "call.kimi.latest",
        ),
        (
            "opencode",
            "session.opencode.native",
            "call.opencode.first",
            "call.opencode.latest",
        ),
    ] {
        for (order, call) in [first_call, latest_call].into_iter().enumerate() {
            let identity = crate::hooks::NativeContextScoutLifecycleV1::new(session, call).unwrap();
            let range = ObservationSourceRangeV1::new(
                u64::try_from(order).unwrap() + 1,
                u64::try_from(order).unwrap() + 2,
            )
            .unwrap();
            assert!(
                admit_native_context_scout_lifecycle(
                    &sessions,
                    ProviderId::new(provider).unwrap(),
                    &identity,
                    range,
                )
                .await
            );
            assert!(
                admit_native_context_scout_lifecycle(
                    &sessions,
                    ProviderId::new(provider).unwrap(),
                    &identity,
                    range,
                )
                .await
            );
        }
        let lifecycle =
            crate::daemon::context_scout_lifecycle::lookup_registered_context_scout_lifecycle(
                hook_project_id,
                hook_worktree_id,
                &SessionId::new(session.to_owned()).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(lifecycle.provider_id.as_str(), provider);
        assert_eq!(lifecycle.thread_id.as_str(), session);
        assert_eq!(lifecycle.agent_id.as_str(), session);
        assert_eq!(lifecycle.turn_id.as_str(), latest_call);
        assert_eq!(lifecycle.logical_message_id.as_str(), latest_call);
    }
}

use std::sync::Mutex as StdMutex;

use tracedecay_agent_hosts::agents::context_scout::ContextScoutDurableStoreOutcomeV1;
use tracedecay_daemon_service::context_scout_lifecycle::AuthorityRegistrationV1;
use tracedecay_domain::{ObservationSourceRangeV1, ProjectId, ProviderId, SessionId, UtcMicros};
use tracedecay_sessions::admission::HostAdmissionScope;

use super::super::envelope::hook_v2_native_session_id;
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
fn exact_event_claim_lookup_is_unique_and_unexpired() {
    let _guard = RETAINED_CLAIM_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let project_id = [201; 16];
    let now = UtcMicros(999);
    let claim = retained_claim(41);
    let event_id = claim.lease.lease_id;

    assert!(lookup_hook_v2_delivery_claim_for_event(project_id, event_id, now).is_none());
    assert!(retain_hook_v2_delivery_claim(project_id, claim.clone(), now).is_ok());
    assert_eq!(
        lookup_hook_v2_delivery_claim_for_event(project_id, event_id, now),
        Some(claim.clone())
    );

    let mut ambiguous = retained_claim(42);
    ambiguous.lease.lease_id = event_id;
    assert!(retain_hook_v2_delivery_claim(project_id, ambiguous.clone(), now).is_ok());
    assert!(lookup_hook_v2_delivery_claim_for_event(project_id, event_id, now).is_none());
    remove_hook_v2_delivery_claim(project_id, ambiguous.entry.envelope.envelope_id);
    assert!(
        lookup_hook_v2_delivery_claim_for_event(project_id, event_id, claim.lease.expires_at)
            .is_none()
    );
    remove_hook_v2_delivery_claim(project_id, claim.entry.envelope.envelope_id);
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
    let overflow = retained_claim(1);
    assert_eq!(
        retain_hook_v2_delivery_claim([203; 16], overflow.clone(), UtcMicros(1)),
        Err(Box::new(overflow.clone()))
    );
    assert_eq!(
        lookup_hook_v2_delivery_claim([203; 16], overflow.entry.envelope.envelope_id),
        None
    );
    let mut stored_project = [202; 16];
    stored_project[0] = 0;
    assert_eq!(
        lookup_hook_v2_delivery_claim(stored_project, [0; 16]),
        Some(retained_claim(0))
    );
    for index in 0..MAX_RETAINED_HOOK_V2_DELIVERY_CLAIMS as u16 {
        let mut project_id = [202; 16];
        project_id[0] = (index >> 8) as u8;
        remove_hook_v2_delivery_claim(project_id, [index as u8; 16]);
    }
}

#[test]
fn receipt_outcomes_release_claims_and_only_retry_unavailable() {
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
fn hook_v2_native_session_requires_exact_protected_locator() {
    let session_id = "native-session-1";
    let mut envelope = hook_v2_envelope_for_test();
    envelope.protected_session_id =
        tracedecay_agent_hosts::hooks::protected_native_session_id(session_id);
    assert_eq!(
        hook_v2_native_session_id(Some(session_id), &envelope)
            .as_ref()
            .map(SessionId::as_str),
        Some(session_id)
    );

    envelope.protected_session_id = [9; 32];
    assert!(hook_v2_native_session_id(Some(session_id), &envelope).is_none());
}

#[tokio::test]
async fn kimi_and_opencode_queued_lifecycle_delivery_prepares_scout_lookup() {
    let temporary = tempfile::tempdir().unwrap();
    let project_id = ProjectId::new("project.native-hook-scout").unwrap();
    let runtime =
        tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1::project(
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
    assert_eq!(
        tracedecay_daemon_service::context_scout_lifecycle::register_context_scout_lifecycle_authority(
            hook_project_id,
            hook_worktree_id,
            project_id,
            worktree_id,
            &sessions,
        ),
        AuthorityRegistrationV1::Registered
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
            let identity = tracedecay_agent_hosts::hooks::NativeContextScoutLifecycleV1::new(
                session, call, [1; 16],
            )
            .unwrap();
            let range = ObservationSourceRangeV1::new(
                u64::try_from(order).unwrap() + 1,
                u64::try_from(order).unwrap() + 2,
            )
            .unwrap();
            assert!(
                admit_native_context_scout_lifecycle(
                    &sessions,
                    Some(&runtime.background_cpu()),
                    ProviderId::new(provider).unwrap(),
                    &identity,
                    range,
                )
                .await
            );
            assert!(
                admit_native_context_scout_lifecycle(
                    &sessions,
                    Some(&runtime.background_cpu()),
                    ProviderId::new(provider).unwrap(),
                    &identity,
                    range,
                )
                .await
            );
        }
        let lifecycle =
            tracedecay_daemon_service::context_scout_lifecycle::lookup_registered_context_scout_lifecycle(
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

/// Admission order is per host: another session's events, and this session's
/// events that carry no lifecycle, leave gaps between one session's lifecycle
/// positions. A gap must still commit, and an older event arriving after a
/// newer one is already covered; neither may stay backpressured forever.
#[tokio::test]
async fn lifecycle_positions_with_gaps_or_late_arrivals_settle() {
    let temporary = tempfile::tempdir().unwrap();
    let project_id = ProjectId::new("project.native-hook-scout-gaps").unwrap();
    let runtime =
        tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1::project(
            temporary.path().join("profile"),
            temporary.path().join("project"),
            project_id.clone(),
        )
        .await
        .unwrap();
    let sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();
    let hook_project_id = [73; 16];
    let hook_worktree_id = [74; 16];
    assert_eq!(
        tracedecay_daemon_service::context_scout_lifecycle::register_context_scout_lifecycle_authority(
            hook_project_id,
            hook_worktree_id,
            project_id,
            tracedecay_domain::WorktreeId::new("worktree.native-hook-scout-gaps").unwrap(),
            &sessions,
        ),
        AuthorityRegistrationV1::Registered
    );
    let session = "session.kimi.gaps";
    let admit = |call: &'static str, start: u64| {
        let sessions = &sessions;
        let background_cpu = runtime.background_cpu();
        async move {
            admit_native_context_scout_lifecycle(
                sessions,
                Some(&background_cpu),
                ProviderId::new("kimi").unwrap(),
                &tracedecay_agent_hosts::hooks::NativeContextScoutLifecycleV1::new(
                    session, call, [1; 16],
                )
                .unwrap(),
                ObservationSourceRangeV1::new(start, start + 1).unwrap(),
            )
            .await
        }
    };

    assert!(admit("call.kimi.first", 1).await);
    assert!(admit("call.kimi.after-gap", 5).await, "a gap must commit");
    assert!(
        admit("call.kimi.after-gap", 5).await,
        "a retry stays admitted"
    );
    assert!(
        admit("call.kimi.late", 3).await,
        "an event older than the committed cursor is already covered"
    );

    let lifecycle =
        tracedecay_daemon_service::context_scout_lifecycle::lookup_registered_context_scout_lifecycle(
            hook_project_id,
            hook_worktree_id,
            &SessionId::new(session.to_owned()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(lifecycle.turn_id.as_str(), "call.kimi.after-gap");
}

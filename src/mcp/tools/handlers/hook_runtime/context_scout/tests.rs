use std::sync::Mutex as StdMutex;

use tracedecay_domain::UtcMicros;

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
        crate::daemon::HookOrchestrationAdmissionV1::Unavailable,
    );
    assert_eq!(response["status"], "unavailable");
    assert_eq!(response["reason"], "orchestration_unavailable");
    assert!(!response.to_string().contains("candidate"));
    assert!(!response.to_string().contains("control"));
}

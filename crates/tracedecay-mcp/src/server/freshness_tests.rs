use tracedecay_contracts::code_index_freshness::{
    CODE_INDEX_PUBLICATION_AUTHORITY_CORRUPT, CodeIndexConvergenceParkedV1,
};

use crate::server::StartupCatchUpMachineV1;
use crate::tool_error_response;

/// A dispatched catch-up must never read as settled before it runs. This is
/// the ordering hazard the old default-`true` completion flags carried: the
/// dispatch site had to pre-clear them in a separate store, and any waiter
/// that landed in between observed a false "ready".
#[tokio::test]
async fn a_claimed_dispatch_is_never_observed_as_settled() {
    let machine = StartupCatchUpMachineV1::default();
    // Undispatched machines are ready: nothing will ever run.
    assert!(machine.settled());

    assert!(machine.try_claim_dispatch());
    assert!(!machine.try_claim_dispatch());
    // Claiming dispatch is itself the transition into `Syncing`.
    assert!(!machine.settled());

    machine.settle();
    assert!(machine.settled());
}

/// Shutdown must leave the startup sync readable as settled.
#[tokio::test]
async fn a_cancelled_machine_reads_as_settled_and_refuses_further_phases() {
    let machine = StartupCatchUpMachineV1::default();
    assert!(machine.try_claim_dispatch());
    machine.mark_cancelled();
    assert!(machine.settled());

    // A late in-flight task settling after shutdown cannot resurrect it.
    machine.settle();
    assert!(machine.settled());
    assert!(!machine.try_claim_dispatch());
}

#[test]
fn project_route_error_messages_keep_retry_authority_when_clients_hide_error_data() {
    let error = tracedecay_domain::errors::TraceDecayError::ProjectRoute {
        reason_code: "code-graph-unavailable".to_owned(),
        retryable: true,
        detail: "the verified code graph is not ready for the exact project root".to_owned(),
        typed_detail: None,
    };

    let response = tool_error_response(serde_json::json!(9), "tracedecay_test_map", &error);
    let rpc_error = response.error.expect("JSON-RPC error");

    assert_eq!(
        rpc_error.message,
        "tool project route failed: reason_code=code-graph-unavailable retryable=true: the verified code graph is not ready for the exact project root"
    );
    let data = rpc_error.data.expect("structured project-route data");
    assert_eq!(data["reason_code"], "code-graph-unavailable");
    assert_eq!(data["retryable"], true);
}

#[test]
fn publication_corruption_keeps_park_facts_typed_at_the_rpc_boundary() {
    let error = CodeIndexConvergenceParkedV1 {
        reason: "source mode is not owner-private".to_owned(),
        blocked_reason: None,
        remediation: "restore mode 0600".to_owned(),
        parked_at_micros: 1,
        observed_passes: 2,
        retries_on_wake: true,
    }
    .publication_authority_corrupt_error();
    let response = tool_error_response(serde_json::json!(10), "tracedecay_sync", &error);
    let data = response
        .error
        .expect("JSON-RPC error")
        .data
        .expect("structured route data");

    assert_eq!(
        data["reason_code"],
        CODE_INDEX_PUBLICATION_AUTHORITY_CORRUPT
    );
    assert_eq!(
        data["detail"],
        serde_json::json!({
            "kind": "parked",
            "cause": "source mode is not owner-private",
            "remedy": "restore mode 0600",
            "retries_on_wake": true,
        })
    );
}

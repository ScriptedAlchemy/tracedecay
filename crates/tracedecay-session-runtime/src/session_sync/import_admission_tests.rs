use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_contracts::session_sync::{
    SessionSyncCommandV1, SessionSyncControlV1, SessionSyncOutcomeV1, SessionSyncRequestV1,
    SessionSyncScopeV1, SessionSyncServicePort, SessionTranscriptImportV1,
};
use tracedecay_contracts::{
    CancellationSignal, Deadline, IdempotencyKey, OperationTermination, RequestId, now_micros,
};
use tracedecay_domain::{ProjectId, UtcMicros};
use tracedecay_global_db::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_runtime_core::config::ProfileRoot;
use tracedecay_sessions::serving::{
    SessionProjectionServingState, SessionProjectionServingStatusPort, SessionProjectionStaleReason,
};

use crate::session_sync::{DaemonSessionSyncConfig, DaemonSessionSyncService};
use crate::session_temporal_refresh_scheduler::SessionTemporalRefreshWake;
use crate::session_temporal_refresh_scheduler::history::SessionHistoricalIngestOutcome;
use crate::session_temporal_refresh_scheduler::wake::SessionTemporalRefreshWakeState;

#[derive(Clone, Copy)]
enum CatchUp {
    Pending,
    Current,
    Blocked,
}

fn catch_up_state(kind: CatchUp) -> Arc<SessionTemporalRefreshWakeState> {
    let state = Arc::new(SessionTemporalRefreshWakeState::default());
    state.mark_running();
    match kind {
        CatchUp::Pending => state.mark_history_pending(),
        CatchUp::Current => {
            state.record_history_outcome(SessionHistoricalIngestOutcome::Complete);
        }
        CatchUp::Blocked => {
            state.record_history_outcome(SessionHistoricalIngestOutcome::Blocked {
                reason_code: "invalid_observation_contract",
                made_progress: false,
            });
        }
    }
    state
}

fn bound_wake(state: &Arc<SessionTemporalRefreshWakeState>) -> SessionTemporalRefreshWake {
    let wake = SessionTemporalRefreshWake::unavailable();
    wake.bind(state);
    wake
}

async fn import_receipt(
    kind: CatchUp,
    label: &str,
) -> (
    tracedecay_contracts::session_sync::SessionSyncCompletionReceiptV1,
    Arc<SessionTemporalRefreshWakeState>,
) {
    let root = tempfile::tempdir().expect("import fixture directory");
    let project_root = root.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project directory");
    let project_id = ProjectId::new(format!("project.{label}")).expect("project id");
    let runtime =
        HostAdmissionTestRuntimeV1::project(root.path(), &project_root, project_id.clone())
            .await
            .expect("registered session stores");
    let project_sessions = runtime
        .registered_database_lease(HostAdmissionScope::Project)
        .expect("project sessions");
    let profile_sessions = runtime
        .registered_database_lease(HostAdmissionScope::Profile)
        .expect("profile sessions");
    let brain_id = project_sessions.binding().shard_id.brain_id.clone();
    let profile_id = project_sessions.binding().shard_id.profile_id.clone();
    let project_state = catch_up_state(kind);
    let user_state = catch_up_state(kind);
    let service = DaemonSessionSyncService::default();
    service
        .register_project(DaemonSessionSyncConfig {
            brain_id,
            profile_id: profile_id.clone(),
            project_id: project_id.clone(),
            profile_root: root.path().to_path_buf(),
            project_root: project_root.clone(),
            transcript_source_profile: ProfileRoot::new(root.path().to_path_buf()),
            project_sessions,
            user_sessions: profile_sessions.clone(),
            registry: profile_sessions,
            background_cpu: Arc::new(ProcessBackgroundCpuV1::new(NonZeroUsize::MIN)),
            startup_import: false,
            project_refresh: bound_wake(&project_state),
            user_refresh: bound_wake(&user_state),
        })
        .await
        .expect("session sync project");
    let scope = SessionSyncScopeV1::new(project_id, profile_id);
    let request = SessionSyncRequestV1::new(
        RequestId::new(format!("session-sync.{label}")).expect("operation id"),
        IdempotencyKey::new(format!("session-sync.{label}")).expect("idempotency key"),
        scope.clone(),
        Deadline::new(UtcMicros(now_micros().0.saturating_add(60_000_000))).expect("deadline"),
        CancellationSignal::active(format!("session-sync.{label}")).expect("cancellation"),
        SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
    );
    let accepted = SessionSyncServicePort::execute(&service, request).await;
    let SessionSyncOutcomeV1::Accepted(admission) = accepted else {
        panic!("import was not admitted: {accepted:?}");
    };
    let started = tokio::time::Instant::now();
    let control = SessionSyncControlV1::new(scope, admission.idempotency_key);
    // Well under the 60s request deadline the old waiter would have consumed,
    // with room for a loaded runner: this bounds hand-off latency, not CPU.
    let hand_off_bound = Duration::from_secs(10);
    loop {
        match SessionSyncServicePort::status(&service, control.clone()).await {
            SessionSyncOutcomeV1::Complete(receipt) => {
                assert!(
                    started.elapsed() < hand_off_bound,
                    "import consumed its observation bound instead of returning the settled catch-up"
                );
                return (receipt, project_state);
            }
            SessionSyncOutcomeV1::Accepted(_) | SessionSyncOutcomeV1::Joined(_) => {
                assert!(
                    started.elapsed() < hand_off_bound,
                    "import stayed pending while catch-up state was already known"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            other => panic!("import ended without a coverage receipt: {other:?}"),
        }
    }
}

fn remaining_work(
    receipt: &tracedecay_contracts::session_sync::SessionSyncCompletionReceiptV1,
) -> u64 {
    receipt
        .coverage
        .iter()
        .map(|entry| entry.coverage.remaining_work())
        .fold(0, u64::saturating_add)
}

#[tokio::test]
async fn import_reports_deferred_progress_while_historical_catch_up_is_still_pending() {
    let (receipt, _) = import_receipt(CatchUp::Pending, "import-pending").await;

    assert_eq!(receipt.termination, OperationTermination::Partial);
    assert!(receipt.failure_codes.is_empty());
    assert_eq!(remaining_work(&receipt), 2);
}

#[tokio::test]
async fn import_after_current_catch_up_defers_until_the_scheduled_pass_runs() {
    let (receipt, state) = import_receipt(CatchUp::Current, "import-current").await;

    // History was current before the request, but the pass it scheduled has
    // not run: sources written since the last pass are not admitted yet.
    assert_eq!(receipt.termination, OperationTermination::Partial);
    assert!(receipt.failure_codes.is_empty());
    assert_eq!(remaining_work(&receipt), 2);
    assert_eq!(
        bound_wake(&state).serving_status().state,
        SessionProjectionServingState::Stale {
            reason: SessionProjectionStaleReason::HistoricalConvergence,
        }
    );
    assert!(state.take_historical_dirty());
}

#[tokio::test]
async fn import_keeps_a_blocked_catch_up_as_a_failure() {
    let (receipt, state) = import_receipt(CatchUp::Blocked, "import-blocked").await;
    assert!(!state.take_historical_dirty());

    assert_eq!(receipt.termination, OperationTermination::Failed);
    assert!(
        receipt
            .failure_codes
            .iter()
            .any(|code| code == "invalid_observation_contract")
    );
}

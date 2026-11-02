//! Explicit Git writes need the memory graph that project mounting starts in
//! the background. Other session commands do not depend on that activation.

use std::future::Future;
use std::time::{Duration, Instant};

use tracedecay_contracts::now_micros;
use tracedecay_contracts::session_sync::{
    SessionSyncAdmissionErrorV1, SessionSyncCommandV1, SessionSyncOutcomeV1, SessionSyncRequestV1,
};
use tracedecay_domain::UtcMicros;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::native_runtime::{self, TimeoutError};

/// Check the canonical service journal before a Git write waits for graph
/// activation. Status proves only that a key exists; execute revalidates the
/// source and replays the exact receipt. A missing key proceeds to settlement.
pub(super) async fn replay_existing_git_sync(
    request: &SessionSyncRequestV1,
    status: impl Future<Output = SessionSyncOutcomeV1> + Send,
    replay: impl Future<Output = SessionSyncOutcomeV1> + Send,
) -> Option<SessionSyncOutcomeV1> {
    if !matches!(
        request.command(),
        SessionSyncCommandV1::SynchronizeGit(options) if !options.dry_run()
    ) {
        return None;
    }
    match status.await {
        SessionSyncOutcomeV1::Unavailable {
            reason_code: "session_sync_operation_not_found",
        } => None,
        _ => Some(replay.await),
    }
}

/// Join the exact project's existing graph activation before admitting a Git
/// write. Dropping this wait does not cancel the registry-owned activation.
/// Settlement is not a successful graph probe: detached or retired authorities
/// still fail through the sync service's normal typed publication checks.
#[tracing::instrument(name = "mcp.admin.cli.git_graph_settlement", level = "trace", skip_all)]
pub(super) async fn settle_git_sync_graph(
    request: &SessionSyncRequestV1,
    settlement: impl Future<Output = Result<()>> + Send,
) -> Result<Option<SessionSyncOutcomeV1>> {
    if !matches!(
        request.command(),
        SessionSyncCommandV1::SynchronizeGit(options) if !options.dry_run()
    ) {
        return Ok(None);
    }
    let started = Instant::now();
    let observed_at = now_micros();
    if let Some(outcome) = admission_refusal(request, observed_at) {
        return Ok(Some(outcome));
    }
    let remaining = u64::try_from(
        i128::from(request.deadline().expires_at.0) - i128::from(observed_at.0),
    )
    .map_err(|_| TraceDecayError::Config {
        message: "session Git sync deadline is outside the supported range".to_owned(),
    })?;
    let deadline = started
        .checked_add(Duration::from_micros(remaining))
        .ok_or_else(|| TraceDecayError::Config {
            message: "session Git sync deadline exceeds the monotonic clock range".to_owned(),
        })?;
    let settled = tokio::select! {
        biased;
        () = request.cancellation().cancelled() => {
            return Ok(Some(SessionSyncOutcomeV1::Cancelled));
        }
        settled = native_runtime::timeout_at(deadline, settlement) => settled,
    };
    // Settlement and cancellation can become ready in the same poll. The
    // service must receive the original controls, never a renewed deadline.
    if let Some(outcome) = admission_refusal(request, now_micros()) {
        return Ok(Some(outcome));
    }
    match settled {
        Ok(result) => result.map(|()| None),
        Err(TimeoutError::Elapsed) => Ok(Some(SessionSyncOutcomeV1::DeadlineExceeded)),
        Err(TimeoutError::TimerUnavailable(error)) => Err(TraceDecayError::Config {
            message: format!("session Git sync graph settlement timer unavailable: {error}"),
        }),
    }
}

fn admission_refusal(
    request: &SessionSyncRequestV1,
    observed_at: UtcMicros,
) -> Option<SessionSyncOutcomeV1> {
    match request.admit_at(observed_at) {
        Ok(()) => None,
        Err(SessionSyncAdmissionErrorV1::Cancelled) => Some(SessionSyncOutcomeV1::Cancelled),
        Err(SessionSyncAdmissionErrorV1::DeadlineExceeded) => {
            Some(SessionSyncOutcomeV1::DeadlineExceeded)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::{Future, pending, poll_fn};
    use std::task::Poll;

    use tracedecay_contracts::session_sync::{
        SessionGitSyncV1, SessionSyncAdmissionReceiptV1, SessionSyncCommandV1,
        SessionSyncCompletionReceiptV1, SessionSyncOutcomeV1, SessionSyncRequestV1,
        SessionSyncScopeV1, SessionSyncSourceV1, SessionSyncStatsV1, SessionTranscriptImportV1,
    };
    use tracedecay_contracts::{
        CancellationSignal, Deadline, IdempotencyKey, OperationTermination, RequestId, now_micros,
    };
    use tracedecay_domain::errors::{Result, TraceDecayError};
    use tracedecay_domain::{ProjectId, UserProfileId, UtcMicros};
    use tracedecay_runtime_core::native_runtime;

    use super::{replay_existing_git_sync, settle_git_sync_graph};

    fn request(command: SessionSyncCommandV1, expires_at: UtcMicros) -> SessionSyncRequestV1 {
        match native_runtime::install(native_runtime::default_builder()) {
            Ok(_) | Err(native_runtime::InstallError::AlreadyStarted) => {}
            Err(error) => panic!("native runtime failed to start: {error}"),
        }
        assert!(native_runtime::handle().is_some());
        SessionSyncRequestV1::new(
            RequestId::new("request.git-graph").unwrap(),
            IdempotencyKey::new("git-graph").unwrap(),
            SessionSyncScopeV1::new(
                ProjectId::new("project.git-graph").unwrap(),
                UserProfileId::new("profile.git-graph").unwrap(),
            ),
            Deadline::new(expires_at).unwrap(),
            CancellationSignal::active("cancel.git-graph").unwrap(),
            command,
        )
    }

    fn write_request() -> SessionSyncRequestV1 {
        request(
            SessionSyncCommandV1::SynchronizeGit(SessionGitSyncV1::new(0, 1, false).unwrap()),
            UtcMicros(now_micros().0 + 30_000_000),
        )
    }

    #[tokio::test]
    async fn expired_or_cancelled_repeat_replays_the_completed_receipt() {
        for cancelled in [false, true] {
            let request = request(
                SessionSyncCommandV1::SynchronizeGit(
                    SessionGitSyncV1::new(0, 1, false).unwrap(),
                ),
                if cancelled {
                    UtcMicros(now_micros().0 + 30_000_000)
                } else {
                    now_micros()
                },
            );
            if cancelled {
                request.cancellation().cancel(now_micros());
            }
            let completion = SessionSyncOutcomeV1::Complete {
                source: SessionSyncSourceV1::SynchronizeGit,
                receipt: SessionSyncCompletionReceiptV1 {
                    admission: SessionSyncAdmissionReceiptV1 {
                        operation_id: request.operation_id().clone(),
                        idempotency_key: request.idempotency_key().clone(),
                        accepted_at: now_micros(),
                    },
                    coalesced_primary: None,
                    completed_at: now_micros(),
                    termination: OperationTermination::Completed,
                    stats: SessionSyncStatsV1::default(),
                    coverage: Vec::new(),
                    source_frontiers: Vec::new(),
                    failure_codes: Vec::new(),
                },
            };
            let status = completion.clone();
            let replay = completion.clone();
            assert_eq!(
                replay_existing_git_sync(
                    &request,
                    async move { status },
                    async move { replay },
                )
                .await,
                Some(completion),
                "a repeated Git write must preserve its receipt"
            );
        }
    }

    #[tokio::test]
    async fn fresh_git_sync_waits_only_when_the_journal_is_absent() {
        let request = write_request();
        assert_eq!(
            replay_existing_git_sync(
                &request,
                async {
                    SessionSyncOutcomeV1::Unavailable {
                        reason_code: "session_sync_operation_not_found",
                    }
                },
                async { panic!("an unjournaled Git sync must not replay") },
            )
            .await,
            None
        );
    }

    #[tokio::test]
    async fn imports_and_dry_runs_do_not_poll_graph_settlement() {
        for command in [
            SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
            SessionSyncCommandV1::SynchronizeGit(SessionGitSyncV1::new(0, 1, true).unwrap()),
        ] {
            let request = request(command, UtcMicros(now_micros().0 + 30_000_000));
            assert_eq!(
                settle_git_sync_graph(&request, async {
                    panic!("this command must not wait for the memory graph")
                })
                .await
                .unwrap(),
                None
            );
        }
    }

    #[tokio::test]
    async fn git_write_waits_for_settlement_without_changing_request_controls() {
        let request = write_request();
        let original_deadline = request.deadline().clone();
        let original_key = request.idempotency_key().clone();
        let (release, settled) = tokio::sync::oneshot::channel::<()>();
        let mut wait = Box::pin(settle_git_sync_graph(&request, async move {
            settled.await.unwrap();
            Ok(())
        }));
        assert!(
            poll_fn(|cx| Poll::Ready(wait.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        release.send(()).unwrap();
        assert_eq!(wait.await.unwrap(), None);
        assert_eq!(request.deadline(), &original_deadline);
        assert_eq!(request.idempotency_key(), &original_key);
    }

    #[tokio::test]
    async fn cancellation_drops_only_the_pending_settlement_wait() {
        let request = write_request();
        let (release, settled) = tokio::sync::oneshot::channel::<()>();
        let mut wait = Box::pin(settle_git_sync_graph(&request, async move {
            settled.await.unwrap();
            Ok(())
        }));
        assert!(
            poll_fn(|cx| Poll::Ready(wait.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        request.cancellation().cancel(now_micros());
        assert_eq!(wait.await.unwrap(), Some(SessionSyncOutcomeV1::Cancelled));
        assert!(release.is_closed());
    }

    #[tokio::test]
    async fn pre_admission_cancellation_and_expiry_do_not_poll_settlement() {
        let expired = request(
            SessionSyncCommandV1::SynchronizeGit(SessionGitSyncV1::new(0, 1, false).unwrap()),
            now_micros(),
        );
        let cancelled = write_request();
        cancelled.cancellation().cancel(now_micros());
        for (request, expected) in [
            (expired, SessionSyncOutcomeV1::DeadlineExceeded),
            (cancelled, SessionSyncOutcomeV1::Cancelled),
        ] {
            assert_eq!(
                settle_git_sync_graph(&request, async {
                    panic!("refused request must not poll graph settlement")
                })
                .await
                .unwrap(),
                Some(expected)
            );
        }
    }

    #[tokio::test]
    async fn pending_graph_cannot_extend_the_caller_deadline() {
        let request = request(
            SessionSyncCommandV1::SynchronizeGit(SessionGitSyncV1::new(0, 1, false).unwrap()),
            UtcMicros(now_micros().0 + 1_000_000),
        );
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            settle_git_sync_graph(&request, pending::<Result<()>>()),
        )
        .await
        .expect("the original request deadline must stop the wait")
        .unwrap();
        assert_eq!(outcome, Some(SessionSyncOutcomeV1::DeadlineExceeded));
    }

    #[tokio::test]
    async fn graph_settlement_error_is_not_reported_as_readiness() {
        let request = write_request();
        let error = settle_git_sync_graph(&request, async {
            Err(TraceDecayError::project_route(
                "project_runtime_replacing_sessions",
                true,
                "project is being retired",
            ))
        })
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("project_runtime_replacing_sessions")
        );
    }

    #[tokio::test]
    async fn cancellation_during_successful_settlement_still_refuses_admission() {
        let request = write_request();
        let cancellation = request.cancellation().clone();
        assert_eq!(
            settle_git_sync_graph(&request, async move {
                cancellation.cancel(now_micros());
                Ok(())
            })
            .await
            .unwrap(),
            Some(SessionSyncOutcomeV1::Cancelled)
        );
    }
}

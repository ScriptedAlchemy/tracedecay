use std::future::Future;
use std::time::Duration;

use serde_json::Value;

use crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake;
use crate::errors::{Result, TraceDecayError};

#[derive(Clone, Copy)]
pub(crate) enum LiveTranscriptRefreshRoute<'a> {
    Project(Option<&'a SessionTemporalRefreshWake>),
    Profile(Option<&'a SessionTemporalRefreshWake>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LiveTranscriptRefreshJoin {
    NotRequired,
    PublicationJoined,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RefreshWaitOutcome {
    Joined,
    Unpublished,
    Cancelled,
}

async fn wait_for_refresh_publication(
    publication: impl Future<Output = bool>,
    cancellation: &tracedecay_application::CancellationSignal,
) -> RefreshWaitOutcome {
    tokio::pin!(publication);
    tokio::select! {
        joined = &mut publication => {
            if joined {
                RefreshWaitOutcome::Joined
            } else {
                RefreshWaitOutcome::Unpublished
            }
        }
        () = crate::daemon_client::wait_for_cancellation(cancellation.clone()) => {
            RefreshWaitOutcome::Cancelled
        }
    }
}

pub(crate) fn live_transcript_refresh_required(tool_name: &str, arguments: &Value) -> bool {
    match tool_name {
        "tracedecay_hook_runtime" => {
            arguments.get("action").and_then(Value::as_str) == Some("ingest_transcript")
        }
        "tracedecay_lcm_preflight" => {
            arguments
                .get("transcript_projection")
                .and_then(Value::as_bool)
                == Some(true)
        }
        _ => false,
    }
}

fn refresh_failure(
    tool_name: &str,
    reason_code: &'static str,
    detail: &'static str,
) -> TraceDecayError {
    let completion = if tool_name == "tracedecay_hook_runtime" {
        "hook"
    } else {
        "LCM"
    };
    TraceDecayError::session_refresh(
        reason_code,
        true,
        format!("{detail} before {completion} completion"),
    )
}

fn remaining_request_budget(
    tool_name: &str,
    deadline: &tracedecay_application::Deadline,
) -> Result<Duration> {
    let remaining_micros = deadline
        .expires_at
        .0
        .saturating_sub(tracedecay_application::clock::now_micros().0);
    let remaining_micros = u64::try_from(remaining_micros).map_err(|_| {
        refresh_failure(
            tool_name,
            "temporal_refresh_deadline_exceeded",
            "session temporal refresh request deadline elapsed",
        )
    })?;
    if remaining_micros == 0 {
        return Err(refresh_failure(
            tool_name,
            "temporal_refresh_deadline_exceeded",
            "session temporal refresh request deadline elapsed",
        ));
    }
    Ok(Duration::from_micros(remaining_micros))
}

pub(crate) async fn join_required_live_transcript_refresh(
    tool_name: &str,
    arguments: &Value,
    route: LiveTranscriptRefreshRoute<'_>,
    deadline: &tracedecay_application::Deadline,
    cancellation: &tracedecay_application::CancellationSignal,
) -> Result<LiveTranscriptRefreshJoin> {
    if !live_transcript_refresh_required(tool_name, arguments) {
        return Ok(LiveTranscriptRefreshJoin::NotRequired);
    }
    join_live_transcript_refresh(tool_name, route, deadline, cancellation).await
}

pub(crate) async fn join_live_transcript_refresh(
    tool_name: &str,
    route: LiveTranscriptRefreshRoute<'_>,
    deadline: &tracedecay_application::Deadline,
    cancellation: &tracedecay_application::CancellationSignal,
) -> Result<LiveTranscriptRefreshJoin> {
    if cancellation.is_cancelled() {
        return Err(refresh_failure(
            tool_name,
            "temporal_refresh_cancelled",
            "session temporal refresh request was cancelled",
        ));
    }
    let wake = match route {
        LiveTranscriptRefreshRoute::Project(wake) | LiveTranscriptRefreshRoute::Profile(wake) => {
            wake
        }
    }
    .ok_or_else(|| {
        refresh_failure(
            tool_name,
            "temporal_refresh_unavailable",
            "session temporal refresh authority is unavailable",
        )
    })?;
    let remaining = remaining_request_budget(tool_name, deadline)?;
    let outcome =
        wait_for_refresh_publication(wake.wake_and_wait_until_idle(remaining), cancellation).await;
    if outcome == RefreshWaitOutcome::Cancelled {
        Err(refresh_failure(
            tool_name,
            "temporal_refresh_cancelled",
            "session temporal refresh request was cancelled",
        ))
    } else if outcome == RefreshWaitOutcome::Joined {
        Ok(LiveTranscriptRefreshJoin::PublicationJoined)
    } else if deadline.is_elapsed_at(tracedecay_application::clock::now_micros()) {
        Err(refresh_failure(
            tool_name,
            "temporal_refresh_deadline_exceeded",
            "session temporal refresh request deadline elapsed",
        ))
    } else {
        Err(refresh_failure(
            tool_name,
            "temporal_refresh_unavailable",
            "session temporal refresh did not publish",
        ))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake;

    fn request_control(
        remaining_micros: i64,
    ) -> (
        tracedecay_application::Deadline,
        tracedecay_application::CancellationSignal,
    ) {
        let now = tracedecay_application::clock::now_micros();
        (
            tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
                now.0.saturating_add(remaining_micros),
            ))
            .unwrap(),
            tracedecay_application::CancellationSignal::active("live-refresh-test").unwrap(),
        )
    }

    #[tokio::test]
    async fn completed_hook_ingest_fails_when_its_refresh_owner_is_unavailable() {
        let (deadline, cancellation) = request_control(1_000_000);
        let error = super::join_required_live_transcript_refresh(
            "tracedecay_hook_runtime",
            &json!({"action": "ingest_transcript"}),
            super::LiveTranscriptRefreshRoute::Project(Some(
                &SessionTemporalRefreshWake::unavailable(),
            )),
            &deadline,
            &cancellation,
        )
        .await
        .expect_err("completed ingest must not outlive an unavailable refresh");

        assert_eq!(
            error.session_refresh_context(),
            Some((
                "temporal_refresh_unavailable",
                true,
                "session temporal refresh did not publish before hook completion",
            ))
        );
        let data = crate::mcp::tools::structured_hook_error_data(&error)
            .expect("hook error must retain structured context");
        assert_eq!(data["status"], "unavailable");
    }

    #[tokio::test]
    async fn projectless_hook_uses_its_admitted_profile_route_when_scope_is_omitted() {
        let (deadline, cancellation) = request_control(1_000_000);
        let error = super::join_required_live_transcript_refresh(
            "tracedecay_hook_runtime",
            &json!({"action": "ingest_transcript"}),
            super::LiveTranscriptRefreshRoute::Profile(Some(
                &SessionTemporalRefreshWake::unavailable(),
            )),
            &deadline,
            &cancellation,
        )
        .await
        .expect_err("profile ingest must require the admitted profile refresh owner");

        assert_eq!(
            error.session_refresh_context().map(|context| context.0),
            Some("temporal_refresh_unavailable")
        );
    }

    #[tokio::test]
    async fn selected_project_join_accepts_only_the_selected_refresh_authority() {
        let (deadline, cancellation) = request_control(1_000_000);
        let selected_project_wake = SessionTemporalRefreshWake::unavailable();
        let error = super::join_required_live_transcript_refresh(
            "tracedecay_hook_runtime",
            &json!({"action": "ingest_transcript"}),
            super::LiveTranscriptRefreshRoute::Project(Some(&selected_project_wake)),
            &deadline,
            &cancellation,
        )
        .await
        .expect_err("selected project must require its own refresh owner");

        assert_eq!(
            error.session_refresh_context().map(|context| context.0),
            Some("temporal_refresh_unavailable")
        );
    }

    #[tokio::test]
    async fn lcm_refresh_failure_keeps_typed_retryable_session_context() {
        let (deadline, cancellation) = request_control(1_000_000);
        let error = super::join_required_live_transcript_refresh(
            "tracedecay_lcm_preflight",
            &json!({"storage_scope": "user", "transcript_projection": true}),
            super::LiveTranscriptRefreshRoute::Profile(Some(
                &SessionTemporalRefreshWake::unavailable(),
            )),
            &deadline,
            &cancellation,
        )
        .await
        .expect_err("LCM projection must fail when its refresh cannot publish");

        assert_eq!(
            error.session_refresh_context(),
            Some((
                "temporal_refresh_unavailable",
                true,
                "session temporal refresh did not publish before LCM completion",
            ))
        );
        assert!(error.hook_runtime_context().is_none());
    }

    #[tokio::test]
    async fn refresh_join_uses_only_the_remaining_request_deadline() {
        let (deadline, cancellation) = request_control(-1);
        let error = super::join_required_live_transcript_refresh(
            "tracedecay_hook_runtime",
            &json!({"action": "ingest_transcript"}),
            super::LiveTranscriptRefreshRoute::Project(Some(
                &SessionTemporalRefreshWake::unavailable(),
            )),
            &deadline,
            &cancellation,
        )
        .await
        .expect_err("an exhausted request cannot mint a refresh timeout");

        assert_eq!(
            error.session_refresh_context().map(|context| context.0),
            Some("temporal_refresh_deadline_exceeded")
        );
    }

    #[tokio::test]
    async fn refresh_join_observes_request_cancellation() {
        let (deadline, cancellation) = request_control(1_000_000);
        assert!(cancellation.cancel(tracedecay_application::clock::now_micros()));
        let error = super::join_required_live_transcript_refresh(
            "tracedecay_hook_runtime",
            &json!({"action": "ingest_transcript"}),
            super::LiveTranscriptRefreshRoute::Project(Some(
                &SessionTemporalRefreshWake::unavailable(),
            )),
            &deadline,
            &cancellation,
        )
        .await
        .expect_err("a cancelled request cannot join refresh");

        assert_eq!(
            error.session_refresh_context().map(|context| context.0),
            Some("temporal_refresh_cancelled")
        );
    }

    #[tokio::test]
    async fn refresh_join_stops_when_cancellation_arrives_during_the_wait() {
        let cancellation =
            tracedecay_application::CancellationSignal::active("live-refresh-inflight").unwrap();
        let cancel = cancellation.clone();
        let cancellation_task = tokio::spawn(async move {
            tokio::task::yield_now().await;
            assert!(cancel.cancel(tracedecay_application::clock::now_micros()));
        });

        let outcome =
            super::wait_for_refresh_publication(std::future::pending::<bool>(), &cancellation)
                .await;
        cancellation_task.await.unwrap();

        assert_eq!(outcome, super::RefreshWaitOutcome::Cancelled);
    }
}

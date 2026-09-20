use std::time::Duration;

use serde_json::Value;

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_sessions::serving::SessionRefreshWorkerPort;

const LIVE_TRANSCRIPT_REFRESH_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum LiveTranscriptRefreshScope {
    Project,
    User,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiveTranscriptRefreshJoin {
    NotRequired,
    PublicationJoined,
}

fn required_refresh_scope(
    tool_name: &str,
    arguments: &Value,
) -> Option<LiveTranscriptRefreshScope> {
    let required = match tool_name {
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
    };
    required.then(|| {
        if arguments.get("user_scope").and_then(Value::as_bool) == Some(true)
            || arguments.get("storage_scope").and_then(Value::as_str) == Some("user")
        {
            LiveTranscriptRefreshScope::User
        } else {
            LiveTranscriptRefreshScope::Project
        }
    })
}

fn refresh_unavailable(tool_name: &str) -> TraceDecayError {
    const DETAIL: &str = "session temporal refresh did not publish before hook completion";
    if tool_name == "tracedecay_hook_runtime" {
        TraceDecayError::hook_runtime_with_status(
            "temporal_refresh_unavailable",
            true,
            DETAIL,
            tracedecay_sessions::admission::HostAdmissionStatus::Unavailable.as_wire(),
        )
    } else {
        TraceDecayError::Config {
            message: DETAIL.to_owned(),
        }
    }
}

/// Joins the refresh owner of the store this call wrote.
///
/// `project_wake` and `user_wake` are the execution server's owners. A
/// selected project is that server, so its wake is the project owner, not a
/// reason to ignore it. A missing owner is unavailable. There is no fallback
/// onto the other scope or onto some other project's scheduler.
pub async fn join_required_live_transcript_refresh(
    tool_name: &str,
    arguments: &Value,
    project_wake: Option<&dyn SessionRefreshWorkerPort>,
    user_wake: Option<&dyn SessionRefreshWorkerPort>,
) -> Result<LiveTranscriptRefreshJoin> {
    let Some(scope) = required_refresh_scope(tool_name, arguments) else {
        return Ok(LiveTranscriptRefreshJoin::NotRequired);
    };
    let wake = match scope {
        LiveTranscriptRefreshScope::Project => project_wake,
        LiveTranscriptRefreshScope::User => user_wake,
    }
    .ok_or_else(|| refresh_unavailable(tool_name))?;
    if wake
        .wake_and_wait_until_idle(LIVE_TRANSCRIPT_REFRESH_DEADLINE)
        .await
    {
        Ok(LiveTranscriptRefreshJoin::PublicationJoined)
    } else {
        Err(refresh_unavailable(tool_name))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use serde_json::json;
    use tracedecay_contracts::{SessionTemporalRefreshWakeFuture, SessionTemporalRefreshWakePort};
    use tracedecay_sessions::serving::{
        SessionProjectionServingState, SessionProjectionServingStatus,
        SessionProjectionServingStatusPort,
    };

    use super::LiveTranscriptRefreshJoin;

    use tracedecay_contracts::UnavailableSessionTemporalRefreshWake;

    /// Refresh owner whose publication is the state after `wake_and_wait`.
    struct PublishingRefresh {
        published: AtomicBool,
    }

    impl PublishingRefresh {
        fn idle() -> Self {
            Self {
                published: AtomicBool::new(false),
            }
        }

        fn published(&self) -> bool {
            self.published.load(Ordering::Acquire)
        }
    }

    impl SessionTemporalRefreshWakePort for PublishingRefresh {
        fn wake(&self) -> bool {
            self.published.store(true, Ordering::Release);
            true
        }

        fn is_unavailable(&self) -> bool {
            false
        }

        fn wake_and_wait_until_idle(
            &self,
            _timeout: Duration,
        ) -> SessionTemporalRefreshWakeFuture<'_> {
            let published = self.wake();
            Box::pin(async move { published })
        }
    }

    impl SessionProjectionServingStatusPort for PublishingRefresh {
        fn serving_status(&self) -> SessionProjectionServingStatus {
            SessionProjectionServingStatus {
                state: SessionProjectionServingState::Current,
                last_progress_at_unix_micros: None,
                backlog: 0,
                blocker: None,
                retry_class: None,
            }
        }
    }

    #[tokio::test]
    async fn completed_hook_ingest_fails_when_its_refresh_owner_is_unavailable() {
        let error = super::join_required_live_transcript_refresh(
            "tracedecay_hook_runtime",
            &json!({"action": "ingest_transcript"}),
            Some(&UnavailableSessionTemporalRefreshWake),
            None,
        )
        .await
        .expect_err("completed ingest must not outlive an unavailable refresh");

        assert_eq!(
            error.hook_runtime_context(),
            Some((
                "temporal_refresh_unavailable",
                true,
                "session temporal refresh did not publish before hook completion",
            ))
        );
        let data = crate::structured_hook_error_data(&error)
            .expect("hook error must retain structured context");
        assert_eq!(data["status"], "unavailable");
    }

    #[tokio::test]
    async fn user_scope_never_falls_back_to_the_project_refresh_owner() {
        let project = PublishingRefresh::idle();
        let error = super::join_required_live_transcript_refresh(
            "tracedecay_hook_runtime",
            &json!({"action": "ingest_transcript", "user_scope": true}),
            Some(&project),
            None,
        )
        .await
        .expect_err("user ingest must require the user refresh owner");

        assert_eq!(
            error.hook_runtime_context().map(|context| context.0),
            Some("temporal_refresh_unavailable")
        );
        assert!(
            !project.published(),
            "user ingest must not publish through the project refresh owner"
        );
    }

    #[tokio::test]
    async fn project_ingest_does_not_publish_through_the_user_refresh_owner() {
        let user = PublishingRefresh::idle();
        let error = super::join_required_live_transcript_refresh(
            "tracedecay_hook_runtime",
            &json!({"action": "ingest_transcript"}),
            None,
            Some(&user),
        )
        .await
        .expect_err("project ingest must require the project refresh owner");

        assert_eq!(
            error.hook_runtime_context().map(|context| context.0),
            Some("temporal_refresh_unavailable")
        );
        assert!(
            !user.published(),
            "project ingest must not publish through the user refresh owner"
        );
    }

    #[tokio::test]
    async fn hook_ingest_joins_the_project_refresh_owner() {
        let project = PublishingRefresh::idle();
        let user = PublishingRefresh::idle();
        let joined = super::join_required_live_transcript_refresh(
            "tracedecay_hook_runtime",
            &json!({"action": "ingest_transcript"}),
            Some(&project),
            Some(&user),
        )
        .await
        .expect("project hook ingest must join its refresh owner");

        assert_eq!(joined, LiveTranscriptRefreshJoin::PublicationJoined);
        assert!(
            project.published(),
            "hook ingest must publish through the project refresh owner"
        );
        assert!(
            !user.published(),
            "project hook ingest must not also publish through the user owner"
        );
    }
}

use std::time::Duration;

use serde_json::{Map, Value};

use tracedecay_domain::SessionId;
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
    let required = tool_name == "tracedecay_lcm_preflight"
        && arguments
            .get("transcript_projection")
            .and_then(Value::as_bool)
            == Some(true);
    required.then(|| {
        if arguments.get("storage_scope").and_then(Value::as_str) == Some("user") {
            LiveTranscriptRefreshScope::User
        } else {
            LiveTranscriptRefreshScope::Project
        }
    })
}

const REFRESH_UNAVAILABLE: &str = "session temporal refresh did not publish before hook completion";

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
    join_scope(scope, None, project_wake, user_wake, || {
        TraceDecayError::Config {
            message: REFRESH_UNAVAILABLE.to_owned(),
        }
    })
    .await
}

/// Joins the refresh owner of the store a completed hook transcript ingest
/// wrote: the user store's for a `user_scope` ingest, the project's
/// otherwise.
pub async fn join_hook_ingest_refresh(
    user_scope: bool,
    session_id: Option<&SessionId>,
    project_wake: Option<&dyn SessionRefreshWorkerPort>,
    user_wake: Option<&dyn SessionRefreshWorkerPort>,
) -> Result<()> {
    let scope = if user_scope {
        LiveTranscriptRefreshScope::User
    } else {
        LiveTranscriptRefreshScope::Project
    };
    join_scope(scope, session_id, project_wake, user_wake, || {
        TraceDecayError::hook_runtime_with_status(
            "temporal_refresh_unavailable",
            true,
            REFRESH_UNAVAILABLE,
            tracedecay_sessions::admission::HostAdmissionStatus::Unavailable.as_wire(),
        )
    })
    .await
    .map(|_| ())
}

/// Hermes inline capture preserves this exact public session identity. Source
/// sweeps may write several sessions, and other providers can normalize their
/// native IDs, so those calls keep the aggregate publication contract.
/// Capture still owns request validation and admission before the join runs.
pub fn hook_ingest_refresh_session(arguments: &Map<String, Value>) -> Result<Option<SessionId>> {
    if arguments.get("action").and_then(Value::as_str) != Some("ingest_transcript")
        || arguments.get("provider").and_then(Value::as_str) != Some("hermes")
        || !arguments.get("messages").is_some_and(Value::is_array)
    {
        return Ok(None);
    }
    let session_id = arguments
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| TraceDecayError::Config {
            message: "Hermes turn callback requires a session_id".to_owned(),
        })?;
    SessionId::new(session_id)
        .map(Some)
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid Hermes turn session: {error}"),
        })
}

async fn join_scope(
    scope: LiveTranscriptRefreshScope,
    session_id: Option<&SessionId>,
    project_wake: Option<&dyn SessionRefreshWorkerPort>,
    user_wake: Option<&dyn SessionRefreshWorkerPort>,
    unavailable: impl Fn() -> TraceDecayError,
) -> Result<LiveTranscriptRefreshJoin> {
    let wake = match scope {
        LiveTranscriptRefreshScope::Project => project_wake,
        LiveTranscriptRefreshScope::User => user_wake,
    }
    .ok_or_else(&unavailable)?;
    let published = match session_id {
        Some(session_id) => {
            wake.wake_and_wait_for_session_projection(session_id, LIVE_TRANSCRIPT_REFRESH_DEADLINE)
                .await
        }
        None => {
            wake.wake_and_wait_for_projection(LIVE_TRANSCRIPT_REFRESH_DEADLINE)
                .await
        }
    };
    if published {
        Ok(LiveTranscriptRefreshJoin::PublicationJoined)
    } else {
        Err(unavailable())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use serde_json::json;
    use tracedecay_contracts::{SessionTemporalRefreshWakeFuture, SessionTemporalRefreshWakePort};
    use tracedecay_domain::SessionId;
    use tracedecay_sessions::serving::{
        SessionConvergenceStatus, SessionProjectionServingState, SessionProjectionServingStatus,
        SessionProjectionServingStatusPort,
    };

    use super::LiveTranscriptRefreshJoin;

    use tracedecay_contracts::UnavailableSessionTemporalRefreshWake;

    /// Refresh owner whose publication is the state after `wake_and_wait`.
    struct PublishingRefresh {
        published: AtomicBool,
        session: Mutex<Option<SessionId>>,
    }

    impl PublishingRefresh {
        fn idle() -> Self {
            Self {
                published: AtomicBool::new(false),
                session: Mutex::new(None),
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

        fn wake_and_wait_for_projection(
            &self,
            _timeout: Duration,
        ) -> SessionTemporalRefreshWakeFuture<'_> {
            let published = self.wake();
            Box::pin(async move { published })
        }

        fn wake_and_wait_for_session_projection<'a>(
            &'a self,
            session_id: &'a SessionId,
            _timeout: Duration,
        ) -> SessionTemporalRefreshWakeFuture<'a> {
            *self.session.lock().expect("record session target") = Some(session_id.clone());
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
                convergence: SessionConvergenceStatus::unavailable(),
            }
        }
    }

    #[tokio::test]
    async fn completed_hook_ingest_fails_when_its_refresh_owner_is_unavailable() {
        let error = super::join_hook_ingest_refresh(
            false,
            None,
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
        let error = super::join_hook_ingest_refresh(true, None, Some(&project), None)
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
        let error = super::join_hook_ingest_refresh(false, None, None, Some(&user))
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
        super::join_hook_ingest_refresh(false, None, Some(&project), Some(&user))
            .await
            .expect("project hook ingest must join its refresh owner");

        assert!(
            project.published(),
            "hook ingest must publish through the project refresh owner"
        );
        assert!(
            !user.published(),
            "project hook ingest must not also publish through the user owner"
        );
    }

    #[tokio::test]
    async fn hermes_inline_join_names_its_session_but_source_scans_remain_aggregate() {
        for (arguments, expected) in [
            (
                json!({"action": "ingest_transcript", "provider": "hermes", "session_id": "hermes-live", "messages": [{"role": "user", "content": "live turn"}]}),
                Some(SessionId::new("hermes-live").expect("session id")),
            ),
            (
                json!({"action": "ingest_transcript", "provider": "hermes", "messages": null}),
                None,
            ),
            (
                json!({"action": "ingest_transcript", "provider": "hermes"}),
                None,
            ),
            (
                json!({"action": "ingest_transcript", "provider": "codex", "session_id": "native-provider-id", "messages": []}),
                None,
            ),
        ] {
            let target = super::hook_ingest_refresh_session(arguments.as_object().unwrap())
                .expect("derive the canonical target without changing source-scan validation");
            assert_eq!(target, expected);
            let project = PublishingRefresh::idle();
            let user = PublishingRefresh::idle();
            super::join_hook_ingest_refresh(true, target.as_ref(), Some(&project), Some(&user))
                .await
                .expect("join the owning user store");
            assert!(user.published());
            assert_eq!(*user.session.lock().unwrap(), expected);
            assert!(!project.published());
        }
    }

    #[tokio::test]
    async fn projected_lcm_preflight_joins_its_user_refresh_owner() {
        let project = PublishingRefresh::idle();
        let user = PublishingRefresh::idle();
        let joined = super::join_required_live_transcript_refresh(
            "tracedecay_lcm_preflight",
            &json!({"transcript_projection": true, "storage_scope": "user"}),
            Some(&project),
            Some(&user),
        )
        .await
        .expect("projected preflight must join its refresh owner");

        assert_eq!(joined, LiveTranscriptRefreshJoin::PublicationJoined);
        assert!(user.published());
        assert!(!project.published());
    }
}

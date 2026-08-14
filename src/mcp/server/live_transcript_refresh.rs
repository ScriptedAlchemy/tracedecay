use std::time::Duration;

use serde_json::Value;

use crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake;
use crate::errors::{Result, TraceDecayError};

const LIVE_TRANSCRIPT_REFRESH_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum LiveTranscriptRefreshScope {
    Project,
    User,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LiveTranscriptRefreshJoin {
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
            tracedecay_usecases::host_admission::HostAdmissionStatus::Unavailable.as_wire(),
        )
    } else {
        TraceDecayError::Config {
            message: DETAIL.to_owned(),
        }
    }
}

pub(crate) async fn join_required_live_transcript_refresh(
    tool_name: &str,
    arguments: &Value,
    selected_project_owner: bool,
    project_wake: Option<&SessionTemporalRefreshWake>,
    user_wake: Option<&SessionTemporalRefreshWake>,
) -> Result<LiveTranscriptRefreshJoin> {
    let Some(scope) = required_refresh_scope(tool_name, arguments) else {
        return Ok(LiveTranscriptRefreshJoin::NotRequired);
    };
    let wake = match scope {
        LiveTranscriptRefreshScope::Project if !selected_project_owner => project_wake,
        LiveTranscriptRefreshScope::User => user_wake,
        LiveTranscriptRefreshScope::Project => None,
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
    use serde_json::json;

    use crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake;

    #[tokio::test]
    async fn completed_hook_ingest_fails_when_its_refresh_owner_is_unavailable() {
        let error = super::join_required_live_transcript_refresh(
            "tracedecay_hook_runtime",
            &json!({"action": "ingest_transcript"}),
            false,
            Some(&SessionTemporalRefreshWake::unavailable()),
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
        let data = crate::mcp::tools::structured_hook_error_data(&error)
            .expect("hook error must retain structured context");
        assert_eq!(data["status"], "unavailable");
    }

    #[tokio::test]
    async fn user_scope_never_falls_back_to_the_project_refresh_owner() {
        let error = super::join_required_live_transcript_refresh(
            "tracedecay_hook_runtime",
            &json!({"action": "ingest_transcript", "user_scope": true}),
            false,
            Some(&SessionTemporalRefreshWake::unavailable()),
            None,
        )
        .await
        .expect_err("user ingest must require the user refresh owner");

        assert_eq!(
            error.hook_runtime_context().map(|context| context.0),
            Some("temporal_refresh_unavailable")
        );
    }

    #[tokio::test]
    async fn selected_project_never_uses_the_active_projects_refresh_owner() {
        let active_project_wake = SessionTemporalRefreshWake::unavailable();
        let error = super::join_required_live_transcript_refresh(
            "tracedecay_hook_runtime",
            &json!({"action": "ingest_transcript"}),
            true,
            Some(&active_project_wake),
            None,
        )
        .await
        .expect_err("selected project must require its own refresh owner");

        assert_eq!(
            error.hook_runtime_context().map(|context| context.0),
            Some("temporal_refresh_unavailable")
        );
    }
}

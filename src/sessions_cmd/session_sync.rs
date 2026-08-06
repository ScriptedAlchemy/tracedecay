use std::path::Path;

use serde_json::{Value, json};

use super::{call_daemon_tool, resolve_cli_project_root};

/// Default lower bound for `git-sync`: 90 days before now.
const GIT_SYNC_DEFAULT_WINDOW_SECS: i64 = 90 * 24 * 60 * 60;

pub(super) async fn run_git_sync(
    project_id: Option<String>,
    project_path: Option<String>,
    since: Option<String>,
    limit_sessions: usize,
    dry_run: bool,
) -> tracedecay::errors::Result<()> {
    let project_root = resolve_cli_project_root(None, project_id, project_path).await?;
    let since_ts = resolve_git_sync_since(since.as_deref())?;
    let outcome = call_daemon_tool(
        &project_root,
        "tracedecay_admin_cli",
        json!({
            "action": "sessions_git_sync",
            "since": since_ts,
            "limit_sessions": limit_sessions,
            "dry_run": dry_run,
        }),
    )
    .await?;

    await_session_sync_completion(&project_root, "session git sync", outcome).await?;
    if dry_run {
        println!("git-sync (dry-run): no rows were written");
    }
    Ok(())
}

#[derive(Debug)]
pub(super) enum SessionSyncPollState {
    Pending {
        operation_id: String,
        idempotency_key: String,
    },
    Completed,
}

pub(super) fn session_sync_poll_state(
    label: &str,
    outcome: &serde_json::Value,
) -> tracedecay::errors::Result<SessionSyncPollState> {
    let status = outcome
        .get("status")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: format!("daemon {label} response omitted its typed status"),
        })?;
    match status {
        "accepted" | "joined" => {
            let operation_id = outcome
                .get("operation_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "daemon {label} response reported {status} without an operation id"
                    ),
                })?;
            let idempotency_key = outcome
                .get("idempotency_key")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "daemon {label} response reported {status} without an idempotency key"
                    ),
                })?;
            Ok(SessionSyncPollState::Pending {
                operation_id: operation_id.to_owned(),
                idempotency_key: idempotency_key.to_owned(),
            })
        }
        "complete" => {
            let operation_id = outcome
                .get("operation_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "daemon {label} response reported complete without an operation id"
                    ),
                })?;
            let termination = outcome
                .get("termination")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "daemon {label} response reported complete without a termination"
                    ),
                })?;
            let remaining_work = session_sync_remaining_work(outcome).ok_or_else(|| {
                tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "daemon {label} response reported complete without truthful source coverage"
                    ),
                }
            })?;
            if termination != "completed" || remaining_work > 0 {
                let failures = outcome
                    .get("failure_codes")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>();
                let detail = if failures.is_empty() {
                    termination.to_owned()
                } else {
                    format!("{termination}: {}", failures.join(", "))
                };
                let detail = if remaining_work == 0 {
                    detail
                } else {
                    format!("{detail}; remaining work {remaining_work}")
                };
                return Err(tracedecay::errors::TraceDecayError::Config {
                    message: format!("{label} did not complete successfully ({detail})"),
                });
            }
            println!("{label} completed ({operation_id})");
            Ok(SessionSyncPollState::Completed)
        }
        "cancelled" | "deadline_exceeded" | "wrong_scope" => {
            Err(tracedecay::errors::TraceDecayError::Config {
                message: format!("{label} did not complete successfully ({status})"),
            })
        }
        "unavailable" => {
            let reason = outcome
                .get("reason_code")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "daemon {label} response reported unavailable without a reason code"
                    ),
                })?;
            Err(tracedecay::errors::TraceDecayError::Config {
                message: format!("{label} unavailable ({reason})"),
            })
        }
        unexpected => Err(tracedecay::errors::TraceDecayError::Config {
            message: format!("daemon {label} response reported unknown status {unexpected:?}"),
        }),
    }
}

fn session_sync_remaining_work(outcome: &Value) -> Option<u64> {
    let coverage = outcome.get("coverage")?.as_array()?;
    if coverage.is_empty() {
        return None;
    }
    coverage.iter().try_fold(0_u64, |remaining, entry| {
        let coverage = entry.get("coverage")?;
        let deferred = match coverage.get("outcome")?.as_str()? {
            "complete" => 0,
            "partial" => coverage.get("deferred_units")?.as_u64()?,
            "backpressured" => coverage.get("rejected_units")?.as_u64()?,
            _ => return None,
        };
        Some(remaining.saturating_add(deferred))
    })
}

pub(super) async fn await_session_sync_completion(
    project_root: &Path,
    label: &str,
    mut outcome: Value,
) -> tracedecay::errors::Result<()> {
    let client_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(35);
    loop {
        match session_sync_poll_state(label, &outcome)? {
            SessionSyncPollState::Completed => return Ok(()),
            SessionSyncPollState::Pending {
                operation_id,
                idempotency_key,
            } => {
                if tokio::time::Instant::now() >= client_deadline {
                    return Err(tracedecay::errors::TraceDecayError::Config {
                        message: format!(
                            "{label} operation {operation_id} did not reach a terminal state"
                        ),
                    });
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                outcome = call_daemon_tool(
                    project_root,
                    "tracedecay_admin_cli",
                    json!({
                        "action": "sessions_sync_status",
                        "idempotency_key": idempotency_key,
                    }),
                )
                .await?;
            }
        }
    }
}

/// Resolves the `--since` argument (ISO-8601 or unix seconds) to a unix-second
/// lower bound, defaulting to 90 days before now when unset.
fn resolve_git_sync_since(since: Option<&str>) -> tracedecay::errors::Result<i64> {
    let Some(raw) = since.map(str::trim).filter(|value| !value.is_empty()) else {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        return Ok((now - GIT_SYNC_DEFAULT_WINDOW_SECS).max(0));
    };
    if let Ok(unix) = raw.parse::<i64>() {
        if unix >= 0 {
            return Ok(unix);
        }
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "--since must be >= 0".to_string(),
        });
    }
    tracedecay::timeutil::parse_rfc3339_timestamp(raw).ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "--since must be a non-negative Unix timestamp or ISO/RFC3339 string (got `{raw}`)"
            ),
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{SessionSyncPollState, session_sync_poll_state};

    #[test]
    fn session_sync_admission_is_pending_until_truthful_completion() {
        assert!(matches!(
            session_sync_poll_state(
                "session import",
                &json!({
                    "status": "accepted",
                    "operation_id": "operation.fixture",
                    "idempotency_key": "session-sync.fixture"
                })
            )
            .unwrap(),
            SessionSyncPollState::Pending { ref idempotency_key, .. }
                if idempotency_key == "session-sync.fixture"
        ));
        assert!(matches!(
            session_sync_poll_state(
                "session import",
                &json!({
                    "status": "complete",
                    "operation_id": "operation.fixture",
                    "idempotency_key": "session-sync.fixture",
                    "termination": "completed",
                    "stats": {},
                    "coverage": [{
                        "store_scope": "project",
                        "coverage": {
                            "outcome": "complete"
                        }
                    }]
                })
            )
            .unwrap(),
            SessionSyncPollState::Completed
        ));
    }

    #[test]
    fn session_sync_noncompletion_is_a_cli_error() {
        for outcome in [
            json!({"status": "wrong_scope"}),
            json!({"status": "deadline_exceeded"}),
            json!({"status": "cancelled"}),
            json!({
                "status": "unavailable",
                "reason_code": "session_sync_authority_unavailable"
            }),
            json!({
                "status": "complete",
                "operation_id": "operation.fixture",
                "idempotency_key": "session-sync.fixture",
                "termination": "failed",
                "failure_codes": ["native_transcript_scan_failed"]
            }),
            json!({
                "status": "complete",
                "operation_id": "operation.fixture",
                "idempotency_key": "session-sync.fixture",
                "termination": "partial",
                "failure_codes": ["cursor_unavailable"]
            }),
        ] {
            assert!(session_sync_poll_state("session import", &outcome).is_err());
        }
    }

    #[test]
    fn session_sync_reports_remaining_coverage_even_if_daemon_mislabels_completion() {
        let error = session_sync_poll_state(
            "session import",
            &json!({
                "status": "complete",
                "operation_id": "operation.fixture",
                "idempotency_key": "session-sync.fixture",
                "termination": "completed",
                "coverage": [{
                    "store_scope": "profile",
                    "coverage": {
                        "outcome": "partial",
                        "deferred_units": 4
                    }
                }]
            }),
        )
        .expect_err("partial transcript coverage cannot be CLI success");

        assert!(error.to_string().contains("remaining work 4"));
    }

    #[test]
    fn session_sync_rejects_completion_without_source_coverage() {
        let error = session_sync_poll_state(
            "session import",
            &json!({
                "status": "complete",
                "operation_id": "operation.fixture",
                "idempotency_key": "session-sync.fixture",
                "termination": "completed"
            }),
        )
        .expect_err("coverage-free completion cannot prove convergence");

        assert!(
            error
                .to_string()
                .contains("without truthful source coverage")
        );
    }
}

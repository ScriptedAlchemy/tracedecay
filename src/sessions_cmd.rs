use std::path::Path;

use crate::{
    cli::{SessionsAction, SessionsSearchArgs},
    resolve_cli_project_root,
};
use serde_json::{Value, json};

pub(crate) async fn handle_sessions_action(
    action: SessionsAction,
) -> tracedecay::errors::Result<()> {
    match action {
        SessionsAction::Ingest {
            provider,
            project_id,
            project_path,
        } => {
            let project_path = resolve_cli_project_root(None, project_id, project_path).await?;
            if let Some(provider) = provider.as_deref() {
                tracedecay::sessions::ProviderScope::parse_optional(Some(provider))
                    .map_err(|message| tracedecay::errors::TraceDecayError::Config { message })?;
            }
            let stats = call_daemon_tool(
                &project_path,
                "tracedecay_admin_cli",
                json!({ "action": "sessions_ingest" }),
            )
            .await?;
            println!(
                "ingested {} session(s), {} message(s)",
                stats["sessions_upserted"].as_u64().unwrap_or(0),
                stats["messages_upserted"].as_u64().unwrap_or(0)
            );
        }
        SessionsAction::Search(args) => {
            let SessionsSearchArgs {
                query,
                provider,
                scope,
                message_type,
                parent_session_id,
                limit,
                since,
                until,
                project_id,
                project_path,
                branch,
                worktree,
                commit,
            } = *args;
            let project_path = resolve_cli_project_root(None, project_id, project_path).await?;
            let payload = call_daemon_tool(
                &project_path,
                "tracedecay_message_search",
                json!({
                    "query": query,
                    "provider": provider,
                    "scope": scope,
                    "message_type": message_type,
                    "parent_session_id": parent_session_id,
                    "limit": limit,
                    "since": since,
                    "until": until,
                    "branch": branch,
                    "worktree": worktree,
                    "commit": commit,
                    "format": "json",
                }),
            )
            .await?;
            for result in payload["results"].as_array().into_iter().flatten() {
                println!(
                    "[{}] {} {}: {}",
                    result
                        .pointer("/session/provider")
                        .and_then(Value::as_str)
                        .unwrap_or("-"),
                    result
                        .pointer("/session/project_key")
                        .and_then(Value::as_str)
                        .unwrap_or("-"),
                    result
                        .pointer("/message/role")
                        .and_then(Value::as_str)
                        .unwrap_or("-"),
                    result
                        .pointer("/message/text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .replace('\n', " ")
                );
            }
        }
        SessionsAction::GitBackfill {
            project_id,
            project_path,
            since,
            limit_sessions,
            dry_run,
        } => {
            run_git_backfill(project_id, project_path, since, limit_sessions, dry_run).await?;
        }
        SessionsAction::Unfinished {
            limit,
            json,
            project_id,
            project_path,
        } => {
            let project_path = resolve_cli_project_root(None, project_id, project_path).await?;
            let payload = call_daemon_tool(
                &project_path,
                "tracedecay_admin_cli",
                json!({ "action": "sessions_unfinished", "limit": limit }),
            )
            .await?;
            let items = payload["items"].as_array().cloned().unwrap_or_default();
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&items).map_err(|e| {
                        tracedecay::errors::TraceDecayError::Config {
                            message: e.to_string(),
                        }
                    })?
                );
            } else {
                for item in items {
                    let task_id = item["task_id"].as_str().unwrap_or("-");
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        item["status"].as_str().unwrap_or("-"),
                        item["provider"].as_str().unwrap_or("-"),
                        item["session_id"].as_str().unwrap_or("-"),
                        task_id,
                        item["message_id"].as_str().unwrap_or("-"),
                        item["evidence"].as_str().unwrap_or("")
                    );
                }
            }
        }
    }
    Ok(())
}

/// Default lower bound for `git-backfill`: 90 days before now.
const GIT_BACKFILL_DEFAULT_WINDOW_SECS: i64 = 90 * 24 * 60 * 60;

async fn run_git_backfill(
    project_id: Option<String>,
    project_path: Option<String>,
    since: Option<String>,
    limit_sessions: usize,
    dry_run: bool,
) -> tracedecay::errors::Result<()> {
    let project_root = resolve_cli_project_root(None, project_id, project_path).await?;
    let since_ts = resolve_backfill_since(since.as_deref())?;
    let stats = call_daemon_tool(
        &project_root,
        "tracedecay_admin_cli",
        json!({
            "action": "sessions_git_backfill",
            "since": since_ts,
            "limit_sessions": limit_sessions,
            "dry_run": dry_run,
        }),
    )
    .await?;

    if dry_run {
        println!("git-backfill (dry-run): no rows written");
    }
    println!("sessions scanned:    {}", stats["sessions_scanned"]);
    println!("spans written:       {}", stats["spans_written"]);
    println!("commits attributed:  {}", stats["commits_attributed"]);
    println!(
        "skipped:             {} (no-window {}, not-worktree {}, git-error {})",
        stats["skipped_total"],
        stats["skipped_no_window"],
        stats["skipped_not_worktree"],
        stats["skipped_git_error"]
    );
    Ok(())
}

/// Resolves the `--since` argument (ISO-8601 or unix seconds) to a unix-second
/// lower bound, defaulting to 90 days before now when unset.
fn resolve_backfill_since(since: Option<&str>) -> tracedecay::errors::Result<i64> {
    let Some(raw) = since.map(str::trim).filter(|value| !value.is_empty()) else {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        return Ok((now - GIT_BACKFILL_DEFAULT_WINDOW_SECS).max(0));
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

async fn call_daemon_tool(
    project_root: &Path,
    tool_name: &str,
    arguments: Value,
) -> tracedecay::errors::Result<Value> {
    let handshake = tracedecay::daemon::DaemonHandshake::for_current_client(
        Some(project_root.to_path_buf()),
        None,
        false,
        false,
    )?;
    let result = tracedecay::daemon::call_default_tool(&handshake, tool_name, arguments).await?;
    tracedecay::daemon::tool_json_payload(&result, tool_name)
}

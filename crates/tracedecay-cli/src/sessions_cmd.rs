use std::fmt::Write as _;
use std::path::Path;

use crate::{
    cli::{SessionsAction, SessionsSearchArgs},
    resolve_cli_project_root,
};
use serde_json::{Map, Value, json};
use tracedecay_contracts::retained_surfaces::{MessageSearchResultV1, RetainedOutcomeStatusV1};

mod refresh;
mod session_sync;
use refresh::handle_session_refresh_action;
use session_sync::{await_session_sync_completion, run_git_sync, run_sync_status};

fn message_search_rpc_args(args: SessionsSearchArgs) -> Value {
    let SessionsSearchArgs {
        query,
        provider,
        scope,
        message_type,
        parent_session_id,
        limit,
        since,
        until,
        project_id: _,
        project_path: _,
        branch,
        worktree,
        commit,
    } = args;
    let mut arguments = Map::from_iter([
        ("query".to_string(), Value::String(query)),
        ("scope".to_string(), Value::String(scope)),
        ("message_type".to_string(), Value::String(message_type)),
        ("limit".to_string(), json!(limit)),
        ("format".to_string(), Value::String("json".to_string())),
    ]);
    for (key, value) in [
        ("provider", provider),
        ("parent_session_id", parent_session_id),
        ("since", since),
        ("until", until),
        ("branch", branch),
        ("worktree", worktree),
        ("commit", commit),
    ] {
        if let Some(value) = value {
            arguments.insert(key.to_string(), Value::String(value));
        }
    }
    Value::Object(arguments)
}

#[hotpath::measure(label = "cli.sessions.dispatch", future = true)]
pub(crate) async fn handle_sessions_action(
    action: SessionsAction,
) -> tracedecay_domain::errors::Result<()> {
    match action {
        SessionsAction::Import {
            project_id,
            project_path,
        } => {
            handle_sessions_import(project_id, project_path).await?;
        }
        SessionsAction::SyncStatus {
            idempotency_key,
            project_id,
            project_path,
        } => {
            run_sync_status(project_id, project_path, idempotency_key).await?;
        }
        SessionsAction::Search(args) => {
            handle_sessions_search(*args).await?;
        }
        SessionsAction::Refresh { action } => {
            handle_session_refresh_action(action).await?;
        }
        SessionsAction::GitSync {
            project_id,
            project_path,
            since,
            limit_sessions,
            dry_run,
        } => {
            hotpath::future!(
                run_git_sync(project_id, project_path, since, limit_sessions, dry_run),
                label = "cli.sessions.git_sync"
            )
            .await?;
        }
        SessionsAction::Unfinished {
            limit,
            json,
            project_id,
            project_path,
        } => {
            handle_sessions_unfinished(limit, json, project_id, project_path).await?;
        }
    }
    Ok(())
}

#[hotpath::measure(label = "cli.sessions.import", future = true)]
async fn handle_sessions_import(
    project_id: Option<String>,
    project_path: Option<String>,
) -> tracedecay_domain::errors::Result<()> {
    let project_path = resolve_cli_project_root(None, project_id, project_path).await?;
    let outcome = call_daemon_tool(
        &project_path,
        "tracedecay_admin_cli",
        json!({ "action": "sessions_import" }),
    )
    .await?;
    await_session_sync_completion(&project_path, "session import", outcome).await
}

#[hotpath::measure(label = "cli.sessions.search", future = true)]
async fn handle_sessions_search(args: SessionsSearchArgs) -> tracedecay_domain::errors::Result<()> {
    let project_id = args.project_id.clone();
    let project_path = args.project_path.clone();
    let project_path = resolve_cli_project_root(None, project_id, project_path).await?;
    let payload = call_daemon_tool(
        &project_path,
        "tracedecay_message_search",
        message_search_rpc_args(args),
    )
    .await?;
    let result: MessageSearchResultV1 =
        crate::commands::retained_tool_payload("tracedecay_message_search", payload)?;
    print!("{}", SessionsSearchReport::render(&result));
    if let Some(error) = &result.error {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("sessions search failed: {}: {}", error.code, error.message),
        });
    }
    Ok(())
}

/// One line per hit, or an explicit empty/refusal report. A search that
/// matched nothing must say so — and say what was searched — rather than
/// printing nothing, and a typed error travels with whatever partial results
/// accompanied it.
struct SessionsSearchReport;

impl SessionsSearchReport {
    fn render(result: &MessageSearchResultV1) -> String {
        let mut report = String::new();
        let hits = result.results.as_deref().unwrap_or_default();
        for hit in hits {
            let _ = writeln!(
                report,
                "[{}] {} {}: {}",
                hit.session.provider,
                hit.session.project_key,
                hit.message.role,
                hit.message.text.replace('\n', " ")
            );
        }
        if hits.is_empty() && result.error.is_none() {
            let status = Self::status_label(result.status);
            let query = result.query.as_deref().unwrap_or("");
            let _ = writeln!(
                report,
                "no messages matched query {query:?} \
                 (status: {status}, scope: {}, provider: {})",
                result.scope, result.provider
            );
            if let Some(message) = &result.message {
                let _ = writeln!(report, "{message}");
            }
            if let Some(next_action) = &result.next_action {
                let _ = writeln!(
                    report,
                    "next: {} {} — {}",
                    next_action.tool, next_action.action, next_action.reason
                );
            }
        }
        report
    }

    /// Wire (snake_case) spelling of a retained outcome status for report text.
    fn status_label(status: RetainedOutcomeStatusV1) -> String {
        match serde_json::to_value(status) {
            Ok(Value::String(label)) => label,
            _ => format!("{status:?}"),
        }
    }
}

#[hotpath::measure(label = "cli.sessions.unfinished", future = true)]
async fn handle_sessions_unfinished(
    limit: usize,
    json: bool,
    project_id: Option<String>,
    project_path: Option<String>,
) -> tracedecay_domain::errors::Result<()> {
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
                tracedecay_domain::errors::TraceDecayError::Config {
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
    Ok(())
}

async fn call_daemon_tool(
    project_root: &Path,
    tool_name: &str,
    arguments: Value,
) -> tracedecay_domain::errors::Result<Value> {
    crate::commands::daemon_tool_json(Some(project_root), tool_name, arguments).await
}

#[cfg(test)]
mod search_report_tests {
    use serde_json::json;

    use super::{MessageSearchResultV1, SessionsSearchReport};

    fn search_result(value: serde_json::Value) -> MessageSearchResultV1 {
        serde_json::from_value(value).expect("fixture search result decodes")
    }

    fn base_result() -> serde_json::Value {
        json!({
            "catch_up": false,
            "catch_up_failures": [],
            "catch_up_performed": false,
            "catch_up_provider": "all",
            "goals": false,
            "include_subagents": false,
            "message_type": "any",
            "outcome": "complete_zero",
            "provider": "all",
            "query": "lease fence",
            "refresh_required": false,
            "scope": "project",
            "status": "complete_zero",
        })
    }

    /// The silent-empty defect: a search that matched nothing printed nothing.
    /// An empty page must say it is empty and name what was searched.
    #[test]
    fn an_empty_search_reports_what_was_searched_instead_of_silence() {
        let mut value = base_result();
        value["message"] = json!("no indexed messages matched");
        value["next_action"] = json!({
            "kind": "refresh",
            "tool": "tracedecay_session_refresh_begin",
            "action": "begin",
            "reason": "session index is stale",
        });
        let report = SessionsSearchReport::render(&search_result(value));
        assert!(
            report.contains("no messages matched query \"lease fence\""),
            "empty search must be reported explicitly: {report}"
        );
        assert!(report.contains("status: complete_zero"), "{report}");
        assert!(report.contains("scope: project"), "{report}");
        assert!(report.contains("no indexed messages matched"), "{report}");
        assert!(
            report.contains("tracedecay_session_refresh_begin"),
            "{report}"
        );
    }

    /// A typed error travels with the report; the empty-page banner is not
    /// printed over it.
    #[test]
    fn a_typed_error_suppresses_the_empty_page_banner() {
        let mut value = base_result();
        value["status"] = json!("error");
        value["outcome"] = json!("error");
        value["error"] = json!({
            "code": "retrieval_unavailable",
            "message": "the session index is not available",
        });
        let report = SessionsSearchReport::render(&search_result(value));
        assert!(
            !report.contains("no messages matched"),
            "a refusal is not an empty page: {report}"
        );
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::message_search_rpc_args;
    use crate::cli::SessionsSearchArgs;

    #[test]
    fn message_search_rpc_args_omit_absent_optional_filters() {
        let args = message_search_rpc_args(SessionsSearchArgs {
            query: "example-query".to_string(),
            provider: None,
            scope: "all".to_string(),
            message_type: "all".to_string(),
            parent_session_id: None,
            limit: 3,
            since: None,
            until: None,
            project_id: None,
            project_path: None,
            branch: None,
            worktree: None,
            commit: None,
        });

        assert_eq!(
            args,
            json!({
                "query": "example-query",
                "scope": "all",
                "message_type": "all",
                "limit": 3,
                "format": "json",
            })
        );
    }

    #[test]
    fn message_search_rpc_args_preserve_explicit_typed_filters() {
        let args = message_search_rpc_args(SessionsSearchArgs {
            query: "example-query".to_string(),
            provider: Some("cursor".to_string()),
            scope: "subagents_only".to_string(),
            message_type: "direct_user".to_string(),
            parent_session_id: Some("parent-1".to_string()),
            limit: 5,
            since: Some("last hour".to_string()),
            until: Some("2026-07-28T00:00:00Z".to_string()),
            project_id: None,
            project_path: None,
            branch: Some("master".to_string()),
            worktree: Some("/repos/worktree".to_string()),
            commit: Some("abc123".to_string()),
        });

        assert_eq!(
            args,
            json!({
                "query": "example-query",
                "provider": "cursor",
                "scope": "subagents_only",
                "message_type": "direct_user",
                "parent_session_id": "parent-1",
                "limit": 5,
                "since": "last hour",
                "until": "2026-07-28T00:00:00Z",
                "branch": "master",
                "worktree": "/repos/worktree",
                "commit": "abc123",
                "format": "json",
            })
        );
    }
}

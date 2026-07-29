//! Read-only `tracedecay_workflows` query surface.

use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::errors::Result;
use crate::sessions::git_correlation::GitScopeFilter;
use crate::sessions::workflow_index::MAX_WORKFLOW_LIMIT;
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;
use super::super::render::{self, Md};
use super::support::{argument_error, string_arg, tool_json_with_md};
use super::workflow_index::{
    WorkflowIndexReadPort, WorkflowRunDetailCommand, WorkflowRunDetailOutcome,
    WorkflowRunDetailView, WorkflowRunListCommand, WorkflowRunListOutcome, WorkflowRunScope,
    list_workflow_runs, read_workflow_run,
};

const DEFAULT_WORKFLOWS_LIMIT: usize = 20;

enum WorkflowMode {
    Run {
        run_id: String,
        agent_label: Option<String>,
    },
    Session {
        session_id: String,
    },
    GitScope {
        filter: GitScopeFilter,
    },
}

fn parse_mode(args: &Value) -> Result<WorkflowMode> {
    let run_id = string_arg(args, "run_id");
    let session_id = string_arg(args, "session_id");
    let git_filter = GitScopeFilter::from_args(
        string_arg(args, "branch"),
        string_arg(args, "worktree"),
        string_arg(args, "commit"),
    )
    .map_err(|err| argument_error(err.to_string()))?;

    let selectors = [
        run_id.is_some(),
        session_id.is_some(),
        !git_filter.is_empty(),
    ]
    .into_iter()
    .filter(|set| *set)
    .count();
    if selectors == 0 {
        return Err(argument_error(
            "provide one of: run_id (show/drill), session_id (list runs for a thread), \
             or branch/worktree/commit (list runs on a git ref)",
        ));
    }
    if selectors > 1 {
        return Err(argument_error(
            "run_id, session_id, and the git filters are mutually exclusive; pass only one",
        ));
    }

    if let Some(run_id) = run_id {
        return Ok(WorkflowMode::Run {
            run_id: run_id.to_string(),
            agent_label: string_arg(args, "agent_label").map(str::to_string),
        });
    }
    if let Some(session_id) = session_id {
        return Ok(WorkflowMode::Session {
            session_id: session_id.to_string(),
        });
    }
    Ok(WorkflowMode::GitScope { filter: git_filter })
}

/// Reported when the daemon retained no project session authority. An
/// unretained index is a state, so it never renders as a successful empty list.
fn index_unavailable_payload() -> Value {
    json!({
        "status": "unavailable",
        "message": "registered project session database is unavailable",
        "runs": [],
        "count": 0
    })
}

pub(super) async fn handle_workflows(
    cg: &TraceDecay,
    args: Value,
    workflow_index: Option<&dyn WorkflowIndexReadPort>,
) -> Result<ToolResult> {
    let mode = parse_mode(&args)?;
    let limit = bounded_limit(&args)?;

    let payload = match &mode {
        WorkflowMode::Run {
            run_id,
            agent_label,
        } => run_payload(workflow_index, run_id, agent_label.as_deref(), limit).await?,
        WorkflowMode::Session { session_id } => {
            let command = WorkflowRunListCommand {
                scope: WorkflowRunScope::Session {
                    session_id: session_id.clone(),
                },
                limit,
            };
            match list_workflow_runs(workflow_index, command).await? {
                WorkflowRunListOutcome::Runs(runs) => json!({
                    "status": "ok",
                    "mode": "session",
                    "session_id": session_id,
                    "count": runs.len(),
                    "runs": runs,
                }),
                WorkflowRunListOutcome::IndexUnavailable => index_unavailable_payload(),
            }
        }
        WorkflowMode::GitScope { filter } => {
            let command = WorkflowRunListCommand {
                scope: WorkflowRunScope::GitScope {
                    filter: filter.clone(),
                },
                limit,
            };
            match list_workflow_runs(workflow_index, command).await? {
                WorkflowRunListOutcome::Runs(runs) => json!({
                    "status": "ok",
                    "mode": "git_scope",
                    "git_filter": filter,
                    "count": runs.len(),
                    "runs": runs,
                }),
                WorkflowRunListOutcome::IndexUnavailable => index_unavailable_payload(),
            }
        }
    };

    if render::field_str(&payload, "status") == "unavailable" {
        return Ok(tool_json_with_md(
            Some(cg.project_root()),
            &args,
            &payload,
            || "No workflow index available.".to_string(),
        ));
    }

    Ok(tool_json_with_md(
        Some(cg.project_root()),
        &args,
        &payload,
        || render_workflows_md(&payload),
    ))
}

fn bounded_limit(args: &Value) -> Result<usize> {
    match args.get("limit") {
        None | Some(Value::Null) => Ok(DEFAULT_WORKFLOWS_LIMIT),
        Some(value) => {
            let raw = value
                .as_u64()
                .ok_or_else(|| argument_error("limit must be a positive integer"))?;
            Ok((raw as usize).clamp(1, MAX_WORKFLOW_LIMIT))
        }
    }
}

fn run_not_found_payload(run_id: &str) -> Value {
    json!({
        "status": "ok",
        "mode": "run",
        "run_id": run_id,
        "found": false,
        "runs": [],
        "count": 0,
    })
}

async fn run_payload(
    workflow_index: Option<&dyn WorkflowIndexReadPort>,
    run_id: &str,
    agent_label: Option<&str>,
    limit: usize,
) -> Result<Value> {
    let command = WorkflowRunDetailCommand {
        run_id: run_id.to_string(),
        limit,
    };
    let WorkflowRunDetailView { run, agents } =
        match read_workflow_run(workflow_index, command).await? {
            WorkflowRunDetailOutcome::Run(detail) => detail,
            WorkflowRunDetailOutcome::NotFound => return Ok(run_not_found_payload(run_id)),
            WorkflowRunDetailOutcome::IndexUnavailable => {
                return Ok(index_unavailable_payload());
            }
        };
    match agent_label {
        Some(label) => {
            let agent = agents
                .iter()
                .find(|agent| agent.agent_label == label)
                .map(|agent| &agent.row);
            Ok(json!({
                "status": "ok",
                "mode": "agent",
                "run_id": run_id,
                "agent_label": label,
                "found": agent.is_some(),
                "run": run,
                "agent": agent,
            }))
        }
        None => {
            let agents = agents
                .into_iter()
                .map(|agent| agent.row)
                .collect::<Vec<_>>();
            Ok(json!({
                "status": "ok",
                "mode": "run",
                "run_id": run_id,
                "found": true,
                "run": run,
                "agents": agents,
                "agent_count": agents.len(),
            }))
        }
    }
}

fn render_workflows_md(value: &Value) -> String {
    let mut md = Md::new();
    match render::field_str(value, "mode") {
        "agent" => render_agent_md(&mut md, value),
        "run" if value.get("found").and_then(Value::as_bool) == Some(true) => {
            render_run_detail_md(&mut md, value);
        }
        _ => render_run_list_md(&mut md, value),
    }
    md.render()
}

fn render_run_list_md(md: &mut Md, value: &Value) {
    md.heading(2, "Workflow Runs");
    if let Some(session_id) = value.get("session_id").and_then(Value::as_str) {
        md.field("thread", &format!("`{session_id}`"));
    }
    if let Some(filter) = value.get("git_filter") {
        let summary = git_filter_summary(filter);
        if !summary.is_empty() {
            md.field("git", &summary);
        }
    }
    md.field("count", &render::field_i64(value, "count").to_string());
    let runs = value.get("runs").and_then(Value::as_array);
    match runs {
        Some(runs) if !runs.is_empty() => {
            md.blank();
            for run in runs {
                append_run_bullet(md, run);
            }
        }
        _ => {
            md.blank()
                .empty_note("No workflow runs recorded for this scope yet.");
        }
    }
}

fn append_run_bullet(md: &mut Md, run: &Value) {
    let run_id = render::field_str(run, "run_id");
    let name = render::field_str(run, "name");
    let status = render::field_str(run, "status");
    let mut header = format!("`{run_id}`");
    if !name.is_empty() {
        let _ = write!(header, " · {name}");
    }
    if !status.is_empty() {
        let _ = write!(header, " · {status}");
    }
    md.bullet(&header);
    let mut detail = String::new();
    let agent_count = render::field_i64(run, "agent_count");
    if agent_count > 0 {
        let _ = write!(detail, "{agent_count} agents");
    }
    if let Some(started) = run.get("started_ts").and_then(Value::as_i64) {
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        let _ = write!(
            detail,
            "started {}",
            crate::timeutil::humanize_unix_secs(started)
        );
    }
    let summary = render::field_str(run, "result_summary");
    if !summary.is_empty() {
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        detail.push_str(&crate::sessions::shared::one_line_truncated(summary, 160));
    }
    if !detail.is_empty() {
        md.line(&format!("  {detail}"));
    }
}

fn render_run_detail_md(md: &mut Md, value: &Value) {
    let run = value.get("run").unwrap_or(value);
    let run_id = render::field_str(run, "run_id");
    md.heading(2, &format!("Workflow Run `{run_id}`"));
    for (label, key) in [("name", "name"), ("status", "status")] {
        let field = render::field_str(run, key);
        if !field.is_empty() {
            md.field(label, field);
        }
    }
    if let Some(parent) = run.get("parent_session_id").and_then(Value::as_str)
        && !parent.is_empty()
    {
        md.field("thread", &format!("`{parent}`"));
    }
    let summary = render::field_str(run, "result_summary");
    if !summary.is_empty() {
        md.blank()
            .line(&crate::sessions::shared::one_line_truncated(summary, 600));
    }
    // Phases (from phase_json), then agents.
    if let Some(phases) = run
        .get("phase_json")
        .and_then(Value::as_str)
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .as_ref()
        .and_then(Value::as_array)
        && !phases.is_empty()
    {
        md.blank().line("**Phases**");
        for phase in phases {
            let title = render::field_str(phase, "title");
            if !title.is_empty() {
                md.bullet(title);
            }
        }
    }
    let agents = value.get("agents").and_then(Value::as_array);
    match agents {
        Some(agents) if !agents.is_empty() => {
            md.blank().line("**Agents**");
            for agent in agents {
                append_agent_bullet(md, agent);
            }
        }
        _ => {
            md.blank().empty_note("No agents recorded for this run.");
        }
    }
}

fn render_agent_md(md: &mut Md, value: &Value) {
    let run_id = render::field_str(value, "run_id");
    let label = render::field_str(value, "agent_label");
    md.heading(2, &format!("Agent `{label}`"));
    md.field("run", &format!("`{run_id}`"));
    match value.get("agent") {
        Some(agent) if !agent.is_null() => {
            append_agent_bullet(md, agent);
            let transcript = render::field_str(agent, "transcript_path");
            if !transcript.is_empty() {
                md.blank()
                    .line(&format!("transcript: `{transcript}`"))
                    .line("Replay it with `tracedecay_message_search` (workflow_run/workflow_agent filter) or `tracedecay_lcm_load_session`.");
            }
        }
        _ => {
            md.blank()
                .empty_note("No agent with that label in this run.");
        }
    }
}

fn append_agent_bullet(md: &mut Md, agent: &Value) {
    let label = render::field_str(agent, "agent_label");
    let phase = render::field_str(agent, "phase");
    let status = render::field_str(agent, "status");
    let mut header = format!("`{label}`");
    if !phase.is_empty() {
        let _ = write!(header, " · {phase}");
    }
    if !status.is_empty() {
        let _ = write!(header, " · {status}");
    }
    md.bullet(&header);
    let mut detail = String::new();
    let model = render::field_str(agent, "model");
    if !model.is_empty() {
        let _ = write!(detail, "{model}");
    }
    let tokens = render::field_i64(agent, "tokens");
    if tokens > 0 {
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        let _ = write!(detail, "{tokens} tok");
    }
    if !detail.is_empty() {
        md.line(&format!("  {detail}"));
    }
}

fn git_filter_summary(filter: &Value) -> String {
    let mut parts = Vec::new();
    for key in ["branch", "worktree", "commit"] {
        if let Some(value) = filter.get(key).and_then(Value::as_str) {
            parts.push(format!("{key}=`{value}`"));
        }
    }
    parts.join(" ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_mode_requires_exactly_one_selector() {
        // None → error.
        assert!(parse_mode(&json!({})).is_err());
        // Two families → error.
        assert!(parse_mode(&json!({"run_id": "wf_a", "session_id": "s"})).is_err());
        assert!(parse_mode(&json!({"session_id": "s", "branch": "main"})).is_err());
    }

    #[test]
    fn parse_mode_run_carries_optional_agent_label() {
        match parse_mode(&json!({"run_id": "wf_a", "agent_label": "mine:claude"})).unwrap() {
            WorkflowMode::Run {
                run_id,
                agent_label,
            } => {
                assert_eq!(run_id, "wf_a");
                assert_eq!(agent_label.as_deref(), Some("mine:claude"));
            }
            _ => panic!("expected run mode"),
        }
    }

    #[test]
    fn parse_mode_git_scope_normalizes_filter() {
        match parse_mode(&json!({"branch": "feat/x"})).unwrap() {
            WorkflowMode::GitScope { filter } => {
                assert_eq!(filter.branch.as_deref(), Some("feat/x"));
                assert!(filter.worktree.is_none());
            }
            _ => panic!("expected git scope mode"),
        }
    }

    #[test]
    fn bounded_limit_defaults_and_clamps() {
        assert_eq!(bounded_limit(&json!({})).unwrap(), DEFAULT_WORKFLOWS_LIMIT);
        assert_eq!(bounded_limit(&json!({"limit": 5})).unwrap(), 5);
        assert_eq!(
            bounded_limit(&json!({"limit": 10_000})).unwrap(),
            MAX_WORKFLOW_LIMIT
        );
        assert!(bounded_limit(&json!({"limit": "nope"})).is_err());
    }

    #[test]
    fn render_run_list_empty_is_summary_first() {
        let payload = json!({
            "status": "ok", "mode": "session", "session_id": "s1",
            "runs": [], "count": 0,
        });
        let md = render_workflows_md(&payload);
        assert!(md.contains("Workflow Runs"));
        assert!(md.contains("No workflow runs recorded"));
        // Never leaks a JSON blob into the markdown.
        assert!(!md.contains("\"status\""));
    }

    #[test]
    fn render_run_list_shows_name_status_and_summary() {
        let payload = json!({
            "status": "ok", "mode": "session", "session_id": "s1", "count": 1,
            "runs": [{
                "run_id": "wf_x", "name": "triggering-evals", "status": "completed",
                "agent_count": 11, "started_ts": 1_700_000_000,
                "result_summary": "36 scenarios,\n45 runs",
            }],
        });
        let md = render_workflows_md(&payload);
        assert!(md.contains("wf_x"));
        assert!(md.contains("triggering-evals"));
        assert!(md.contains("11 agents"));
        // Multi-line summary was flattened to one line.
        assert!(md.contains("36 scenarios, 45 runs"));
        assert!(!md.contains("scenarios,\n45"));
    }

    #[test]
    fn render_agent_drill_shows_transcript_and_replay_hint() {
        let payload = json!({
            "status": "ok", "mode": "agent", "run_id": "wf_x",
            "agent_label": "mine:claude", "found": true,
            "agent": {
                "agent_label": "mine:claude", "phase": "Mine", "status": "completed",
                "model": "claude-fable-5", "tokens": 30_212,
                "transcript_path": "/home/u/.claude/.../agent-a1.jsonl",
            },
        });
        let md = render_workflows_md(&payload);
        assert!(md.contains("Agent `mine:claude`"));
        assert!(md.contains("claude-fable-5"));
        assert!(md.contains("30212 tok"));
        assert!(md.contains("agent-a1.jsonl"));
        assert!(md.contains("message_search"));
    }
}

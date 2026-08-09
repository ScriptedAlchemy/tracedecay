//! Read-only `tracedecay_workflows` query surface.

use std::fmt::Write as _;

use serde_json::{Value, json};
use tracedecay_sessions::{
    WorkflowGitScope, WorkflowIndexReadPort, WorkflowIndexState, WorkflowRunDetail,
    WorkflowRunDetailOutcome, WorkflowRunDetailRequest, WorkflowRunListOutcome,
    WorkflowRunListRequest, WorkflowRunScope,
};

use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;
use tracedecay_sessions::runtime::git_correlation::GitScopeFilter;
use tracedecay_sessions::runtime::workflow_index::MAX_WORKFLOW_LIMIT;

use super::super::ToolResult;
use super::super::render::{self, Md};
use super::support::{argument_error, string_arg, tool_json_with_md};
use super::workflow_index::{list_workflow_runs, read_workflow_run};

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
            "missing required parameter: one of run_id (show/drill), session_id (list runs \
             for a thread), or branch/worktree/commit (list runs on a git ref)",
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

/// Reported when the index cannot answer, whether because no authority was
/// retained or because the store carries no workflow schema. Both are states,
/// so neither renders as a successful empty list.
///
/// `runs` and `count` are deliberately absent: a read that never reached an
/// index has no count, and emitting `0` here would let a caller that only reads
/// `count` mistake an unavailable index for an empty one.
///
/// `reason` and `retryable` follow the typed-error shape the session-retrieval
/// surface already uses, so a caller reads unavailability the same way here.
fn index_unavailable_payload(reason: WorkflowIndexState) -> Value {
    json!({
        "status": "unavailable",
        "message": reason.message(),
        "error": {
            "code": "workflow_index_unavailable",
            "message": reason.message(),
            "reason": reason.as_str(),
            "retryable": reason.is_retryable(),
        }
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
            let command = WorkflowRunListRequest {
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
                WorkflowRunListOutcome::Unavailable(reason) => index_unavailable_payload(reason),
            }
        }
        WorkflowMode::GitScope { filter } => {
            let command = WorkflowRunListRequest {
                scope: WorkflowRunScope::GitScope(WorkflowGitScope {
                    branch: filter.branch.clone(),
                    worktree: filter.worktree.clone(),
                    commit: filter.commit.clone(),
                }),
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
                WorkflowRunListOutcome::Unavailable(reason) => index_unavailable_payload(reason),
            }
        }
    };

    if render::field_str(&payload, "status") == "unavailable" {
        let message = render::field_str(&payload, "message").to_string();
        return Ok(
            tool_json_with_md(Some(cg.project_root()), &args, &payload, || {
                render_unavailable_md(&message)
            })
            .with_semantic_error(true),
        );
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

/// Says which state was hit instead of a bare "no index" line, so the markdown
/// reader learns whether to wait for the index or to fix the mount, rather than
/// assuming there are no runs.
fn render_unavailable_md(message: &str) -> String {
    let mut md = Md::new();
    md.heading(2, "Workflow Runs");
    md.blank().empty_note(message);
    md.render()
}

async fn run_payload(
    workflow_index: Option<&dyn WorkflowIndexReadPort>,
    run_id: &str,
    agent_label: Option<&str>,
    limit: usize,
) -> Result<Value> {
    let outcome = match agent_label {
        Some(label) => match workflow_index {
            Some(port) => port
                .agent(run_id.to_string(), label.to_string())
                .await
                .map_err(|error| TraceDecayError::Config {
                    message: error.to_string(),
                })?,
            None => WorkflowRunDetailOutcome::Unavailable(WorkflowIndexState::AuthorityNotRetained),
        },
        None => {
            let command = WorkflowRunDetailRequest {
                run_id: run_id.to_string(),
                limit,
            };
            read_workflow_run(workflow_index, command).await?
        }
    };
    let WorkflowRunDetail {
        run,
        agents,
        agent_count,
        agents_complete,
    } = match outcome {
        WorkflowRunDetailOutcome::Run(detail) => *detail,
        WorkflowRunDetailOutcome::NotFound => return Ok(run_not_found_payload(run_id)),
        WorkflowRunDetailOutcome::Unavailable(reason) => {
            return Ok(index_unavailable_payload(reason));
        }
    };
    match agent_label {
        Some(label) => {
            let agent = agents.iter().find(|agent| agent.agent_label == label);
            let lookup_complete = agent.is_some() || agents_complete;
            Ok(json!({
                "status": if lookup_complete { "ok" } else { "partial" },
                "mode": "agent",
                "run_id": run_id,
                "agent_label": label,
                "found": lookup_complete.then_some(agent.is_some()),
                "run": run,
                "agent": agent,
                "agent_count": agent_count,
                "agents_returned": agents.len(),
                "lookup_complete": lookup_complete,
                "lookup_coverage": if lookup_complete { "conclusive" } else { "bounded_prefix" },
            }))
        }
        None => Ok(json!({
            "status": "ok",
            "mode": "run",
            "run_id": run_id,
            "found": true,
            "run": run,
            "agents": agents,
            "agent_count": agent_count,
            "agents_returned": agents.len(),
            "agents_complete": agents_complete,
            "agents_coverage": if agents_complete { "complete" } else { "bounded_prefix" },
        })),
    }
}

fn render_workflows_md(value: &Value) -> String {
    let mut md = Md::new();
    match render::field_str(value, "mode") {
        "agent" => render_agent_md(&mut md, value),
        "run" if value.get("found").and_then(Value::as_bool) == Some(true) => {
            render_run_detail_md(&mut md, value);
        }
        "run" => render_run_not_found_md(&mut md, value),
        _ => render_run_list_md(&mut md, value),
    }
    md.render()
}

/// A run id the index looked for and does not hold.
///
/// Distinct from an empty scope. The caller named one run, so the list
/// renderer's "no runs recorded for this scope" would answer a question they
/// did not ask and hide that the id itself is unknown.
fn render_run_not_found_md(md: &mut Md, value: &Value) {
    md.heading(2, "Workflow Run");
    md.field("run", &format!("`{}`", render::field_str(value, "run_id")));
    md.blank()
        .empty_note("No workflow run is recorded under this id.");
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
        detail.push_str(&tracedecay_sessions::runtime::shared::one_line_truncated(
            summary, 160,
        ));
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
    let agent_count = render::field_i64(value, "agent_count");
    let agents_returned = render::field_i64(value, "agents_returned");
    let agents_complete = value
        .get("agents_complete")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if agents_complete {
        md.field("agents", &agent_count.to_string());
    } else {
        md.field(
            "agents",
            &format!("{agents_returned} of {agent_count} (bounded)"),
        );
    }
    let summary = render::field_str(run, "result_summary");
    if !summary.is_empty() {
        md.blank()
            .line(&tracedecay_sessions::runtime::shared::one_line_truncated(
                summary, 600,
            ));
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
        _ if agents_complete => {
            md.blank().empty_note("No agents recorded for this run.");
        }
        _ => {
            md.blank()
                .empty_note("No agents are visible within this bounded detail response.");
        }
    }
    if !agents_complete {
        md.blank().empty_note(&format!(
            "Agent coverage is partial: showing {agents_returned} of {agent_count}."
        ));
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
        _ if value
            .get("lookup_complete")
            .and_then(Value::as_bool)
            .is_some_and(|complete| !complete) =>
        {
            md.blank().empty_note(
                "Agent lookup coverage is partial; this response cannot establish absence.",
            );
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
    use tracedecay_sessions::{
        WorkflowAgent, WorkflowRun, WorkflowRunDetailFuture, WorkflowRunListFuture, WorkflowStatus,
    };

    use super::*;

    struct BoundedAgentPort {
        run: WorkflowRun,
        agents: Vec<WorkflowAgent>,
    }

    impl WorkflowIndexReadPort for BoundedAgentPort {
        fn runs(&self, _command: WorkflowRunListRequest) -> WorkflowRunListFuture<'_> {
            Box::pin(async { Ok(WorkflowRunListOutcome::Runs(Vec::new())) })
        }

        fn run(&self, command: WorkflowRunDetailRequest) -> WorkflowRunDetailFuture<'_> {
            let run = self.run.clone();
            let agents = self
                .agents
                .iter()
                .take(command.limit)
                .cloned()
                .collect::<Vec<_>>();
            let agent_count = i64::try_from(self.agents.len()).expect("agent count");
            Box::pin(async move {
                Ok(WorkflowRunDetailOutcome::Run(Box::new(WorkflowRunDetail {
                    run,
                    agents,
                    agent_count,
                    agents_complete: false,
                })))
            })
        }

        fn agent(&self, run_id: String, agent_label: String) -> WorkflowRunDetailFuture<'_> {
            let mut run = self.run.clone();
            let agents = self
                .agents
                .iter()
                .find(|agent| agent.run_id == run_id && agent.agent_label == agent_label)
                .cloned()
                .into_iter()
                .collect::<Vec<_>>();
            let agent_count = i64::try_from(self.agents.len()).expect("agent count");
            run.agent_count = agent_count;
            Box::pin(async move {
                Ok(WorkflowRunDetailOutcome::Run(Box::new(WorkflowRunDetail {
                    run,
                    agents,
                    agent_count,
                    agents_complete: true,
                })))
            })
        }
    }

    struct PrefixOnlyPort(BoundedAgentPort);

    impl WorkflowIndexReadPort for PrefixOnlyPort {
        fn runs(&self, _command: WorkflowRunListRequest) -> WorkflowRunListFuture<'_> {
            Box::pin(async { Ok(WorkflowRunListOutcome::Runs(Vec::new())) })
        }

        fn run(&self, _command: WorkflowRunDetailRequest) -> WorkflowRunDetailFuture<'_> {
            let run = self.0.run.clone();
            let agents = self.0.agents.iter().take(2).cloned().collect::<Vec<_>>();
            let agent_count = i64::try_from(self.0.agents.len()).expect("agent count");
            Box::pin(async move {
                Ok(WorkflowRunDetailOutcome::Run(Box::new(WorkflowRunDetail {
                    run,
                    agents,
                    agent_count,
                    agents_complete: false,
                })))
            })
        }
    }

    fn bounded_agent_port() -> BoundedAgentPort {
        let run = WorkflowRun {
            run_id: "wf_bounded".to_string(),
            parent_session_id: "session-1".to_string(),
            name: None,
            description: None,
            phase_json: None,
            status: WorkflowStatus::Running,
            started_ts: Some(100),
            ended_ts: None,
            result_summary: None,
            agent_count: 3,
        };
        let agents = (1..=3)
            .map(|index| WorkflowAgent {
                run_id: run.run_id.clone(),
                agent_label: format!("agent-{index}"),
                agent_id: format!("id-{index}"),
                phase: None,
                transcript_path: None,
                agent_session_id: None,
                status: WorkflowStatus::Running,
                model: None,
                tokens: 0,
                started_ts: Some(index),
                ended_ts: None,
            })
            .collect();
        BoundedAgentPort { run, agents }
    }

    /// Each unavailable state must be legible on the wire and must never look
    /// like a run list that came back empty. A caller that reads only `count`
    /// must find nothing to read rather than a zero it can mistake for a result.
    #[test]
    fn unavailable_payload_names_the_state_instead_of_reporting_zero_runs() {
        for (reason, wire, retryable) in [
            (
                WorkflowIndexState::AuthorityNotRetained,
                "authority_not_retained",
                false,
            ),
            (
                WorkflowIndexState::IndexNotBuilt,
                "workflow_index_not_built",
                true,
            ),
        ] {
            let payload = index_unavailable_payload(reason);
            assert_eq!(payload["status"], "unavailable");
            assert_ne!(payload["status"], "ok");
            assert_eq!(payload["error"]["code"], "workflow_index_unavailable");
            assert_eq!(payload["error"]["reason"], wire);
            assert_eq!(payload["error"]["retryable"], retryable);
            assert!(payload.get("count").is_none(), "no count without a read");
            assert!(payload.get("runs").is_none(), "no runs without a read");
            // The message must carry the state, not a generic absence.
            let message = payload["message"].as_str().expect("message");
            assert!(!message.is_empty());
            assert_eq!(message, reason.message());
        }
    }

    /// Three different answers to "show me runs" must read differently: the
    /// scope holds none, the named run does not exist, and the index could not
    /// be consulted. Collapsing any pair tells the reader the index looked when
    /// it did not, or that a scope is empty when only one id is missing.
    #[test]
    fn empty_scope_missing_run_and_unavailable_index_render_differently() {
        let empty_scope = render_workflows_md(&json!({
            "status": "ok",
            "mode": "session",
            "session_id": "s_alpha",
            "runs": [],
            "count": 0,
        }));
        let missing_run = render_workflows_md(&run_not_found_payload("wf_absent"));
        let unavailable = render_unavailable_md(WorkflowIndexState::IndexNotBuilt.message());

        assert_ne!(empty_scope, missing_run);
        assert_ne!(missing_run, unavailable);
        assert_ne!(empty_scope, unavailable);

        // The missing run names the id the caller asked for, and must not claim
        // the whole scope is empty.
        assert!(missing_run.contains("wf_absent"));
        assert!(!missing_run.contains("for this scope"));
        assert!(empty_scope.contains("for this scope"));
    }

    /// The unbuilt-index and unretained-authority states must not share a
    /// message, or markdown readers cannot tell the two apart.
    #[test]
    fn unavailable_markdown_distinguishes_the_states() {
        let not_built = render_unavailable_md(WorkflowIndexState::IndexNotBuilt.message());
        let no_authority =
            render_unavailable_md(WorkflowIndexState::AuthorityNotRetained.message());
        assert_ne!(not_built, no_authority);
        assert!(not_built.contains("has not been built"));
        assert!(!not_built.contains('{'), "markdown must not leak JSON");
    }

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

    #[tokio::test]
    async fn bounded_run_and_exact_agent_report_truthful_coverage() {
        let port = bounded_agent_port();

        let run = run_payload(Some(&port), "wf_bounded", None, 2)
            .await
            .expect("run payload");
        assert_eq!(run["agent_count"], 3);
        assert_eq!(run["agents_returned"], 2);
        assert_eq!(run["agents_complete"], false);
        assert_eq!(run["agents_coverage"], "bounded_prefix");

        let agent = run_payload(Some(&port), "wf_bounded", Some("agent-3"), 2)
            .await
            .expect("agent payload");
        assert_eq!(agent["status"], "ok");
        assert_eq!(agent["found"], true);
        assert_eq!(agent["agent"]["agent_label"], "agent-3");
        assert_eq!(agent["agent_count"], 3);
        assert_eq!(agent["agents_returned"], 1);
        assert_eq!(agent["lookup_complete"], true);
        assert_eq!(agent["lookup_coverage"], "conclusive");

        let absent = run_payload(Some(&port), "wf_bounded", Some("agent-4"), 2)
            .await
            .expect("missing agent payload");
        assert_eq!(absent["status"], "ok");
        assert_eq!(absent["found"], false);
        assert_eq!(absent["lookup_complete"], true);
        assert_eq!(absent["lookup_coverage"], "conclusive");
    }

    #[tokio::test]
    async fn partial_agent_lookup_never_claims_absence() {
        let port = PrefixOnlyPort(bounded_agent_port());

        let agent = run_payload(Some(&port), "wf_bounded", Some("agent-3"), 2)
            .await
            .expect("agent payload");
        assert_eq!(agent["status"], "partial");
        assert!(agent["found"].is_null());
        assert_eq!(agent["agent_count"], 3);
        assert_eq!(agent["agents_returned"], 0);
        assert_eq!(agent["lookup_complete"], false);
        assert_eq!(agent["lookup_coverage"], "bounded_prefix");
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

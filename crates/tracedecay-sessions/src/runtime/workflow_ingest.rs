//! Workflow-run ingest sweep.
//!
//! Scans Claude Code `wf_*` runs, keeps runs whose parent transcript belongs to
//! `project_root`, and upserts bounded run/agent summaries into `sessions.db`.

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};

use serde_json::Value;

use tracedecay_runtime_core::timeutil::parse_rfc3339_timestamp;

use crate::runtime::shared::ProjectRootMatcher;
use crate::runtime::workflow_index::{
    INGEST_WATERMARK_KEY, WorkflowAgent, WorkflowRun, WorkflowStatus, bump_ingest_watermark,
    read_ingest_watermark,
};

const RESULT_SUMMARY_CAP: usize = 600;

fn parse_timestamp(value: &str) -> Option<u64> {
    u64::try_from(parse_rfc3339_timestamp(value)?).ok()
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowIngestStats {
    pub runs_ingested: u64,
    pub agents_ingested: u64,
}

pub trait WorkflowIngestStore {
    fn dashboard_connection(&self) -> libsql::Connection;
    fn workflow_upsert_run(
        &self,
        run: &WorkflowRun,
    ) -> impl Future<Output = Result<(), crate::runtime::workflow_index::WorkflowIndexError>> + Send;
    fn workflow_upsert_agent(
        &self,
        agent: &WorkflowAgent,
    ) -> impl Future<Output = Result<(), crate::runtime::workflow_index::WorkflowIndexError>> + Send;
}

impl WorkflowIngestStats {
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            runs_ingested: self.runs_ingested.saturating_add(other.runs_ingested),
            agents_ingested: self.agents_ingested.saturating_add(other.agents_ingested),
        }
    }
}

struct DiscoveredRun {
    run_id: String,
    parent_session_id: String,
    meta_path: Option<PathBuf>,
    agents_dir: PathBuf,
}

/// Fail-open at every level: a store that cannot be read, a project whose home
/// cannot be resolved, or an individual malformed run all degrade to "ingest
/// less", never an error. Returns the number of runs and agents upserted.
pub async fn ingest_workflow_runs<S>(db: &S, project_root: &Path) -> WorkflowIngestStats
where
    S: WorkflowIngestStore,
{
    let Some(home) = super::home_dir() else {
        return WorkflowIngestStats::default();
    };
    ingest_workflow_runs_from(db, project_root, &home.join(".claude").join("projects")).await
}

pub(crate) async fn ingest_workflow_runs_from<S>(
    db: &S,
    project_root: &Path,
    projects_dir: &Path,
) -> WorkflowIngestStats
where
    S: WorkflowIngestStore,
{
    let conn = db.dashboard_connection();
    let watermark = read_ingest_watermark(&conn, INGEST_WATERMARK_KEY).await;

    let mut stats = WorkflowIngestStats::default();
    let mut max_mtime = watermark;

    // Resolve the fixed project-side git identity once; every in-window run's
    // membership test reuses it instead of re-resolving the same project root.
    let project_matcher = ProjectRootMatcher::new(project_root);

    for run in discover_runs(projects_dir) {
        let run_mtime = newest_mtime(&run);
        if run_mtime > 0 && run_mtime <= watermark {
            continue;
        }

        // Scope to this project by the owning session's recorded cwd. A run
        // whose parent thread began in another project is skipped without
        // touching the DB — the same per-session cwd filter ClaudeSource uses.
        // This filter also gates the watermark: `discover_runs` walks every
        // project on the machine, but the watermark is persisted per-store, so
        // only in-scope runs may advance it. Letting an out-of-project run raise
        // this store's watermark could push it past a still-changing target run
        // and strand that run (e.g. a Running run never re-ingested once it
        // completes).
        if !run_belongs_to_project(&run, &project_matcher) {
            continue;
        }
        if run_mtime > max_mtime {
            max_mtime = run_mtime;
        }

        match ingest_one_run(db, &run).await {
            Ok(run_stats) => stats = stats.merge(run_stats),
            Err(err) => {
                tracing::debug!(run_id = %run.run_id, error = %err, "skipping workflow run");
            }
        }
    }

    // Persist the advanced watermark so the next sweep skips everything we just
    // processed. Best-effort: a write failure only means the next sweep does a
    // little redundant (idempotent) work.
    if max_mtime > watermark {
        if let Err(err) = bump_ingest_watermark(&conn, INGEST_WATERMARK_KEY, max_mtime).await {
            tracing::debug!(error = %err, "workflow ingest watermark not advanced");
        }
    }

    stats
}

/// Discover every workflow run under `projects_dir` by walking
/// `<slug>/<session_id>/subagents/workflows/<run_id>/`.
fn discover_runs(projects_dir: &Path) -> Vec<DiscoveredRun> {
    let mut runs = Vec::new();
    let Ok(slugs) = std::fs::read_dir(projects_dir) else {
        return runs;
    };
    for slug in slugs.flatten() {
        let slug_path = slug.path();
        if !slug_path.is_dir() {
            continue;
        }
        let Ok(sessions) = std::fs::read_dir(&slug_path) else {
            continue;
        };
        for session in sessions.flatten() {
            let session_path = session.path();
            if !session_path.is_dir() {
                continue;
            }
            let Some(session_id) = session_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            let workflows_dir = session_path.join("subagents").join("workflows");
            let Ok(run_dirs) = std::fs::read_dir(&workflows_dir) else {
                continue;
            };
            for run in run_dirs.flatten() {
                let agents_dir = run.path();
                if !agents_dir.is_dir() {
                    continue;
                }
                let Some(run_id) = agents_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
                else {
                    continue;
                };
                let meta_path = session_path
                    .join("workflows")
                    .join(format!("{run_id}.json"));
                runs.push(DiscoveredRun {
                    run_id,
                    parent_session_id: session_id.clone(),
                    meta_path: meta_path.is_file().then_some(meta_path),
                    agents_dir,
                });
            }
        }
    }
    runs
}

/// Newest mtime (unix seconds) across a run's meta json and its agent-transcript
/// directory, for the incremental watermark. `0` when neither can be stat'd.
fn newest_mtime(run: &DiscoveredRun) -> i64 {
    let mut newest = 0;
    if let Some(meta) = run.meta_path.as_ref() {
        newest = newest.max(file_mtime(meta));
    }
    newest = newest.max(file_mtime(&run.agents_dir));
    newest
}

fn file_mtime(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |dur| i64::try_from(dur.as_secs()).unwrap_or(0))
}

/// Decide whether a run's owning session began inside the project described by
/// `project_matcher`, from the `cwd` recorded in the parent transcript
/// (preferred) or any agent transcript.
fn run_belongs_to_project(run: &DiscoveredRun, project_matcher: &ProjectRootMatcher) -> bool {
    let Some(cwd) = run_cwd(run) else {
        // No resolvable cwd: refuse rather than mis-attribute a run to a
        // project it may not belong to. ClaudeSource makes the same choice.
        return false;
    };
    project_matcher.contains(&cwd)
}

/// The owning session's working directory, probed from the parent transcript
/// (`<session_id>.jsonl`, two levels above `subagents/workflows/`) or, failing
/// that, an agent transcript in the run dir.
fn run_cwd(run: &DiscoveredRun) -> Option<PathBuf> {
    // Parent transcript sits at <slug>/<session_id>.jsonl. agents_dir is
    // <slug>/<session_id>/subagents/workflows/<run_id>; `ancestors()` yields
    // nth(0)=<run_id> dir, nth(1)=workflows, nth(2)=subagents,
    // nth(3)=<slug>/<session_id>. The parent transcript is that session dir's
    // sibling with a `.jsonl` suffix appended (not `with_extension`, which would
    // mangle a session id that happens to contain a dot).
    let parent_transcript = run.agents_dir.ancestors().nth(3).and_then(|session_dir| {
        let name = session_dir.file_name()?.to_str()?;
        Some(session_dir.with_file_name(format!("{name}.jsonl")))
    });
    if let Some(cwd) = parent_transcript
        .as_deref()
        .and_then(crate::runtime::claude::transcript_cwd)
    {
        return Some(cwd);
    }
    // Fall back to the first agent transcript that records a cwd.
    for path in agent_transcripts(&run.agents_dir) {
        if let Some(cwd) = crate::runtime::claude::transcript_cwd(&path) {
            return Some(cwd);
        }
    }
    None
}

/// Absolute paths to the `agent-<id>.jsonl` transcripts in a run directory,
/// excluding the sibling `.meta.json` files and `journal.jsonl`.
fn agent_transcripts(agents_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(agents_dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let is_jsonl = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"));
            let named_agent = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("agent-"));
            is_jsonl && named_agent
        })
        .collect();
    paths.sort();
    paths
}

/// Parse one discovered run and upsert its run row plus every agent row.
async fn ingest_one_run<S>(
    db: &S,
    run: &DiscoveredRun,
) -> Result<WorkflowIngestStats, crate::runtime::workflow_index::WorkflowIndexError>
where
    S: WorkflowIngestStore,
{
    let (mut workflow_run, mut agents) = match run.meta_path.as_deref().and_then(read_run_meta) {
        // Finished (or at least meta-written) run: authoritative roster from
        // `workflowProgress[]`.
        Some(meta) => parse_run_from_meta(&run.run_id, &run.parent_session_id, &meta),
        // In-progress / orphan dir with no meta json yet: synthesize a Running
        // run and derive the roster from journal.jsonl + present agent files.
        None => parse_run_from_dir(&run.run_id, &run.parent_session_id, &run.agents_dir),
    };

    // Enrich each agent from its transcript (path, tokens, session id, times)
    // and reconcile the run-level agent count with what we actually recorded.
    for agent in &mut agents {
        enrich_agent_from_transcript(agent, &run.agents_dir);
    }
    if workflow_run.agent_count == 0 {
        workflow_run.agent_count = i64::try_from(agents.len()).unwrap_or(i64::MAX);
    }

    db.workflow_upsert_run(&workflow_run).await?;
    for agent in &agents {
        db.workflow_upsert_agent(agent).await?;
    }
    Ok(WorkflowIngestStats {
        runs_ingested: 1,
        agents_ingested: agents.len() as u64,
    })
}

/// Read and JSON-parse a `workflows/<run_id>.json` file, or `None` when it is
/// missing or malformed (fail-open — the run is then treated as dir-only).
fn read_run_meta(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

// ---------------------------------------------------------------------------
// Pure parsing (unit-tested; no disk access below this line).
// ---------------------------------------------------------------------------

/// Build a [`WorkflowRun`] and its agent roster from a parsed run-meta JSON
/// (`workflows/<run_id>.json`).
fn parse_run_from_meta(
    run_id: &str,
    parent_session_id: &str,
    meta: &Value,
) -> (WorkflowRun, Vec<WorkflowAgent>) {
    let run_id = meta
        .get("runId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .unwrap_or(run_id)
        .to_string();

    let name = string_field(meta, "workflowName");
    let description = string_field(meta, "summary").or_else(|| string_field(meta, "description"));
    let phase_json = meta
        .get("phases")
        .filter(|phases| phases.is_array())
        .and_then(|phases| serde_json::to_string(phases).ok());
    let status = meta
        .get("status")
        .and_then(Value::as_str)
        .map_or(WorkflowStatus::Unknown, WorkflowStatus::from_disk);
    let started_ts = run_start_ts(meta);
    let ended_ts = run_end_ts(meta, started_ts);
    let result_summary = run_result_summary(meta);
    let default_model = string_field(meta, "defaultModel");

    let agents = parse_roster(&run_id, meta, default_model.as_deref());
    let agent_count = meta
        .get("agentCount")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| i64::try_from(agents.len()).unwrap_or(i64::MAX));

    (
        WorkflowRun {
            run_id,
            parent_session_id: parent_session_id.to_string(),
            name,
            description,
            phase_json,
            status,
            started_ts,
            ended_ts,
            result_summary,
            agent_count,
        },
        agents,
    )
}

/// Synthesize a Running [`WorkflowRun`] for a dir-only (in-progress / orphan)
/// run and build its roster from `journal.jsonl` plus the agent files present.
fn parse_run_from_dir(
    run_id: &str,
    parent_session_id: &str,
    agents_dir: &Path,
) -> (WorkflowRun, Vec<WorkflowAgent>) {
    let journal = read_journal(agents_dir);
    let agent_ids = roster_agent_ids(agents_dir, &journal);
    let agents: Vec<WorkflowAgent> = agent_ids
        .into_iter()
        .map(|agent_id| WorkflowAgent {
            run_id: run_id.to_string(),
            // No progress row means no human label; the agent id is the stable
            // fallback so drill-down still has a handle.
            agent_label: agent_id.clone(),
            status: journal_agent_status(&journal, &agent_id),
            agent_id,
            phase: None,
            transcript_path: None,
            agent_session_id: None,
            model: None,
            tokens: 0,
            started_ts: None,
            ended_ts: None,
        })
        .collect();

    (
        WorkflowRun {
            run_id: run_id.to_string(),
            parent_session_id: parent_session_id.to_string(),
            name: None,
            description: None,
            phase_json: None,
            status: WorkflowStatus::Running,
            started_ts: None,
            ended_ts: None,
            result_summary: None,
            agent_count: i64::try_from(agents.len()).unwrap_or(i64::MAX),
        },
        agents,
    )
}

/// Extract the agent roster from a run meta's `workflowProgress[]`, keeping only
/// `type == "workflow_agent"` entries (the array also holds `workflow_phase`
/// rows). `default_model` backfills an agent that recorded no `model`.
fn parse_roster(run_id: &str, meta: &Value, default_model: Option<&str>) -> Vec<WorkflowAgent> {
    let Some(progress) = meta.get("workflowProgress").and_then(Value::as_array) else {
        return Vec::new();
    };
    progress
        .iter()
        .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("workflow_agent"))
        .map(|entry| {
            let agent_id = string_field(entry, "agentId").unwrap_or_default();
            let label = string_field(entry, "label")
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| {
                    if agent_id.is_empty() {
                        "agent".to_string()
                    } else {
                        agent_id.clone()
                    }
                });
            let status = entry
                .get("state")
                .and_then(Value::as_str)
                .map_or(WorkflowStatus::Unknown, WorkflowStatus::from_disk);
            WorkflowAgent {
                run_id: run_id.to_string(),
                agent_label: label,
                agent_id,
                phase: string_field(entry, "phaseTitle"),
                transcript_path: None,
                agent_session_id: None,
                status,
                model: string_field(entry, "model").or_else(|| default_model.map(str::to_string)),
                tokens: 0,
                started_ts: ms_field_to_secs(entry, "startedAt"),
                ended_ts: ms_field_to_secs(entry, "lastProgressAt"),
            }
        })
        .collect()
}

/// Run start time in unix seconds: `startTime` is a millisecond epoch; fall back
/// to the ISO-8601 `timestamp`.
fn run_start_ts(meta: &Value) -> Option<i64> {
    ms_field_to_secs(meta, "startTime").or_else(|| {
        meta.get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_timestamp)
            .and_then(|secs| i64::try_from(secs).ok())
    })
}

/// Run end time in unix seconds: `started_ts + durationMs/1000` when a duration
/// is recorded, else unknown.
fn run_end_ts(meta: &Value, started_ts: Option<i64>) -> Option<i64> {
    let started = started_ts?;
    let duration_ms = meta.get("durationMs").and_then(Value::as_i64)?;
    Some(started.saturating_add(duration_ms / 1000))
}

/// Prefer the run's dedicated `summary` string; otherwise render `result` (a
/// string or a JSON blob) to a truncated one-line slice, never the whole thing.
fn run_result_summary(meta: &Value) -> Option<String> {
    if let Some(summary) = string_field(meta, "summary") {
        return Some(crate::runtime::shared::one_line_truncated(
            &summary,
            RESULT_SUMMARY_CAP,
        ));
    }
    let result = meta.get("result")?;
    let text = match result {
        Value::Null => return None,
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).ok()?,
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(crate::runtime::shared::one_line_truncated(
        trimmed,
        RESULT_SUMMARY_CAP,
    ))
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// Read a millisecond-epoch numeric field and convert it to unix seconds.
fn ms_field_to_secs(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64).map(|ms| ms / 1000)
}

// ---------------------------------------------------------------------------
// Agent transcript + journal parsing.
// ---------------------------------------------------------------------------

/// Fill in an agent's transcript-derived fields from
/// `agent-<agentId>.jsonl` when that file exists: absolute `transcript_path`,
/// summed `tokens`, `agent_session_id`, and start/end timestamps. A missing or
/// unreadable transcript leaves the roster-derived values untouched.
fn enrich_agent_from_transcript(agent: &mut WorkflowAgent, agents_dir: &Path) {
    if agent.agent_id.is_empty() {
        return;
    }
    let path = agents_dir.join(format!("agent-{}.jsonl", agent.agent_id));
    if !path.is_file() {
        return;
    }
    agent.transcript_path = Some(path.to_string_lossy().to_string());
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let summary = summarize_transcript(&text);
    if summary.tokens > 0 {
        agent.tokens = summary.tokens;
    }
    if agent.agent_session_id.is_none() {
        agent.agent_session_id = summary.session_id;
    }
    if agent.started_ts.is_none() {
        agent.started_ts = summary.first_ts;
    }
    if summary.last_ts.is_some() {
        agent.ended_ts = summary.last_ts;
    }
}

/// Aggregates extracted from one agent transcript.
#[derive(Debug, Default, PartialEq, Eq)]
struct TranscriptSummary {
    /// Sum of `input_tokens + output_tokens` across assistant `usage` objects.
    tokens: i64,
    session_id: Option<String>,
    first_ts: Option<i64>,
    last_ts: Option<i64>,
}

/// Sum tokens and read the session id / first+last timestamps from a transcript
/// body (one JSON object per line). Malformed lines are skipped.
fn summarize_transcript(body: &str) -> TranscriptSummary {
    let mut summary = TranscriptSummary::default();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if summary.session_id.is_none() {
            summary.session_id = string_field(&value, "sessionId");
        }
        if let Some(ts) = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_timestamp)
            .and_then(|secs| i64::try_from(secs).ok())
        {
            if summary.first_ts.is_none() {
                summary.first_ts = Some(ts);
            }
            summary.last_ts = Some(ts);
        }
        summary.tokens = summary.tokens.saturating_add(line_usage_tokens(&value));
    }
    summary
}

/// Input+output tokens from a transcript line's `message.usage`, or `0` when the
/// line carries no usage (user turns, tool results, meta lines).
fn line_usage_tokens(value: &Value) -> i64 {
    let usage = value
        .get("message")
        .and_then(|message| message.get("usage"))
        .or_else(|| value.get("usage"));
    let Some(usage) = usage else {
        return 0;
    };
    let input = usage
        .get("input_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    input.saturating_add(output)
}

/// One `journal.jsonl` event: a `started` / `result` (terminal) marker keyed by
/// `agentId`.
struct JournalEvent {
    event_type: String,
    agent_id: String,
}

/// Parse `journal.jsonl` into its events, skipping malformed lines. Absent
/// journal yields an empty list.
fn read_journal(agents_dir: &Path) -> Vec<JournalEvent> {
    let path = agents_dir.join("journal.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    parse_journal(&text)
}

fn parse_journal(body: &str) -> Vec<JournalEvent> {
    body.lines()
        .filter_map(|line| {
            let value: Value = serde_json::from_str(line.trim()).ok()?;
            let event_type = value.get("type").and_then(Value::as_str)?.to_string();
            let agent_id = value.get("agentId").and_then(Value::as_str)?.to_string();
            if agent_id.is_empty() {
                return None;
            }
            Some(JournalEvent {
                event_type,
                agent_id,
            })
        })
        .collect()
}

/// The set of agent ids for a dir-only run: the union of journal-`started`
/// agents and `agent-<id>.jsonl` files present, so an agent that appears in
/// either source is captured.
fn roster_agent_ids(agents_dir: &Path, journal: &[JournalEvent]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let from_files = agent_transcripts(agents_dir)
        .into_iter()
        .filter_map(|path| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.strip_prefix("agent-"))
                .filter(|id| !id.is_empty())
                .map(str::to_string)
        });
    let from_journal = journal
        .iter()
        .map(|event| event.agent_id.clone())
        .filter(|id| !id.is_empty());
    for id in from_files.chain(from_journal) {
        if seen.insert(id.clone()) {
            ids.push(id);
        }
    }
    ids
}

/// Status of one agent in a dir-only run, inferred from its journal events: a
/// terminal `result` reads as Completed, otherwise Running.
fn journal_agent_status(journal: &[JournalEvent], agent_id: &str) -> WorkflowStatus {
    let mut seen = false;
    for event in journal.iter().filter(|event| event.agent_id == agent_id) {
        seen = true;
        match event.event_type.as_str() {
            "result" | "done" | "completed" => return WorkflowStatus::Completed,
            "error" | "failed" | "blocked" | "interrupted" => return WorkflowStatus::Failed,
            _ => {}
        }
    }
    if seen {
        WorkflowStatus::Running
    } else {
        WorkflowStatus::Unknown
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;

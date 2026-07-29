use std::fmt::Write as _;

use super::lcm_args::{
    MAX_LCM_RESULT_LIMIT, bounded_usize_arg, non_negative_timestamp_arg, required_string_arg,
};
use super::lcm_compact::truncate_chars;
use super::*;

const MESSAGE_SEARCH_SNIPPET_CHARS: usize = 240;

/// Renders `tracedecay_message_search` results as compact markdown. Each hit
/// shows provider, session (id + title), role, timestamp, and score with a
/// plain-text snippet of the message body — deliberately dropping the raw
/// `metadata_json`, `source_path`, and `transcript_path` blobs that the generic
/// renderer would dump verbatim into table cells. Pass `format:"json"` to get
/// the full structured records.
pub(super) fn render_message_search_md(value: &Value) -> String {
    let mut md = Md::new();
    let goals_mode = value.get("goals").and_then(Value::as_bool).unwrap_or(false);
    md.heading(
        2,
        if goals_mode {
            "Session Goals"
        } else {
            "Transcript Search"
        },
    );
    if goals_mode {
        md.field("mode", "goals (latest goal per session)");
    }
    for key in ["query", "provider", "scope"] {
        let field = render::field_str(value, key);
        if !field.is_empty() {
            md.field(key, field);
        }
    }
    md.field("count", &render::field_i64(value, "count").to_string());
    if let Some(scope) = value
        .get("project_scope")
        .and_then(Value::as_str)
        .filter(|scope| !scope.is_empty())
    {
        let searched = render::field_i64(value, "searched_project_count");
        let skipped = render::field_i64(value, "skipped_project_count");
        md.field(
            "project scope",
            &format!("{scope} (searched {searched}, skipped {skipped})"),
        );
    }
    if let Some(summary) = git_filter_summary(value) {
        md.field("git filter", &summary);
    }
    if let Some(summary) = workflow_filter_summary(value) {
        md.field("workflow filter", &summary);
    }
    let results = value.get("results").and_then(Value::as_array);
    match results {
        Some(results) if !results.is_empty() => {
            md.blank();
            for hit in results {
                append_message_search_hit(&mut md, hit);
            }
        }
        _ => {
            md.blank().empty_note(if goals_mode {
                "No goals recorded for this project."
            } else {
                "No matching messages."
            });
        }
    }
    md.render()
}

/// One-line `scoped to run wf_… agent …` summary of an applied workflow-run
/// filter, or `None` when none was applied. Reads the `workflow_run` /
/// `workflow_agent` keys echoed into the payload by the message-search handler.
fn workflow_filter_summary(value: &Value) -> Option<String> {
    if !value
        .get("workflow_filter_applied")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let run_id = value.get("workflow_run").and_then(Value::as_str)?;
    let mut summary = format!("scoped to run `{run_id}`");
    if let Some(agent) = value.get("workflow_agent").and_then(Value::as_str) {
        let _ = write!(summary, " agent `{agent}`");
    }
    Some(summary)
}

/// One-line `branch=… worktree=… commit=…` summary of the applied git-scope
/// filter, or `None` when no filter was applied. Reads the `git_filter` object
/// echoed into the payload by the message-search / lcm-grep handlers.
fn git_filter_summary(value: &Value) -> Option<String> {
    if !value
        .get("git_filter_applied")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let filter = value.get("git_filter")?;
    let mut parts = Vec::new();
    for key in ["branch", "worktree", "commit"] {
        if let Some(field) = filter.get(key).and_then(Value::as_str) {
            parts.push(format!("{key}={field}"));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn append_message_search_hit(md: &mut Md, hit: &Value) {
    let session = hit.get("session");
    let message = hit.get("message");
    let provider = message
        .and_then(|m| m.get("provider"))
        .or_else(|| session.and_then(|s| s.get("provider")))
        .and_then(Value::as_str)
        .unwrap_or("");
    let role = message
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let score = hit.get("score").and_then(Value::as_f64).unwrap_or(0.0);
    let session_id = session
        .and_then(|s| s.get("session_id"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let title = session
        .and_then(|s| s.get("title"))
        .and_then(Value::as_str)
        .filter(|title| !title.is_empty());
    let timestamp = message
        .and_then(|m| m.get("timestamp"))
        .and_then(Value::as_i64);

    let mut header = format!("**{role}** · {provider} · score {score:.1}");
    if let Some(ts) = timestamp {
        let _ = write!(header, " · t={ts}");
    }
    md.bullet(&header);
    let mut locator = format!("session `{session_id}`");
    if let Some(title) = title {
        let _ = write!(locator, " — {title}");
    }
    md.line(&format!("  {locator}"));
    // A `goal` row carries its lifecycle status in metadata; surface it so a
    // reader can tell whether the session's goal is still active.
    if let Some(goal_line) = goal_status_line(message) {
        md.line(&format!("  {goal_line}"));
    }
    let text = message
        .and_then(|m| m.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let snippet = message_text_snippet(text, MESSAGE_SEARCH_SNIPPET_CHARS);
    if !snippet.is_empty() {
        md.line(&format!("  {snippet}"));
    }
}

/// `goal [status]` prefix for a `kind = 'goal'` hit, reading `status` out of the
/// row's `metadata_json`. Returns `None` for non-goal rows (or goal rows with no
/// recorded status, which still render their objective as the snippet).
fn goal_status_line(message: Option<&Value>) -> Option<String> {
    let message = message?;
    if message.get("kind").and_then(Value::as_str) != Some("goal") {
        return None;
    }
    let status = message
        .get("metadata_json")
        .and_then(Value::as_str)
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|meta| {
            meta.get("status")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    Some(match status {
        Some(status) => format!("goal [{status}]"),
        None => "goal".to_string(),
    })
}

/// Best-effort single-line plain-text snippet from a stored message body.
/// Message text is frequently itself JSON (`tool_use` / `tool_result` blocks), so
/// pull the human-readable fields out rather than showing an escaped blob.
pub(super) fn message_text_snippet(text: &str, max_chars: usize) -> String {
    let readable = readable_message_text(text, max_chars.saturating_mul(8));
    let collapsed = readable.split_whitespace().collect::<Vec<_>>().join(" ");
    let (snippet, truncated) = truncate_chars(&collapsed, max_chars);
    if truncated {
        format!("{snippet}…")
    } else {
        snippet
    }
}

fn readable_message_text(text: &str, budget: usize) -> String {
    let trimmed = text.trim_start();
    if (trimmed.starts_with('[') || trimmed.starts_with('{'))
        && let Ok(value) = serde_json::from_str::<Value>(text)
    {
        let mut out = String::new();
        collect_readable_text(&value, &mut out, budget);
        if !out.trim().is_empty() {
            return out;
        }
    }
    text.to_string()
}

fn collect_readable_text(value: &Value, out: &mut String, budget: usize) {
    if out.len() >= budget {
        return;
    }
    match value {
        Value::String(s) if !s.is_empty() => {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(s);
        }
        Value::Array(arr) => {
            for item in arr {
                collect_readable_text(item, out, budget);
            }
        }
        Value::Object(map) => {
            // Prefer human-facing fields; ignore ids, kinds, and metadata blobs.
            for key in ["text", "content", "thinking", "input"] {
                if let Some(field) = map.get(key) {
                    collect_readable_text(field, out, budget);
                }
            }
        }
        _ => {}
    }
}

const DEFAULT_SESSIONS_FOR_LIMIT: usize = 20;

/// Renders `tracedecay_sessions_for` results as compact markdown: one bullet
/// per correlated session with its activity window or commit attribution.
fn render_sessions_for_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Sessions For Git Ref");
    for key in ["git_ref", "value"] {
        let field = render::field_str(value, key);
        if !field.is_empty() {
            md.field(key, field);
        }
    }
    md.field("count", &render::field_i64(value, "count").to_string());
    let results = value.get("results").and_then(Value::as_array);
    match results {
        Some(results) if !results.is_empty() => {
            md.blank();
            for hit in results {
                append_sessions_for_hit(&mut md, hit);
            }
        }
        _ => {
            let index_empty = value
                .get("index_empty")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            md.blank();
            if index_empty {
                if value.get("git_ref").and_then(Value::as_str) == Some("commit") {
                    md.empty_note(
                        "No commit evidence is indexed yet. Run `tracedecay sync` to ingest \
                         direct host/tool evidence; `tracedecay sessions git-backfill` adds \
                         weaker historical overlap evidence.",
                    );
                } else {
                    md.empty_note(
                        "Correlation index is empty — no git spans recorded yet. It will \
                         auto-backfill on the next MCP server startup, or run \
                         `tracedecay sessions git-backfill` to populate it now.",
                    );
                }
            } else {
                md.empty_note("No correlated sessions recorded for this git ref.");
            }
        }
    }
    md.render()
}

fn append_sessions_for_hit(md: &mut Md, hit: &Value) {
    let session_id = render::field_str(hit, "session_id");
    let provider = render::field_str(hit, "provider");
    let mut header = format!("session `{session_id}`");
    if !provider.is_empty() {
        let _ = write!(header, " · {provider}");
    }
    md.bullet(&header);
    let mut detail = String::new();
    if let Some(branch) = hit.get("branch").and_then(Value::as_str) {
        let _ = write!(detail, "branch `{branch}`");
    }
    if let Some(worktree) = hit.get("worktree").and_then(Value::as_str) {
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        let _ = write!(detail, "worktree `{worktree}`");
    }
    if let (Some(first), Some(last)) = (
        hit.get("first_ts").and_then(Value::as_i64),
        hit.get("last_ts").and_then(Value::as_i64),
    ) {
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        let _ = write!(
            detail,
            "active {} .. {}",
            crate::timeutil::humanize_unix_secs(first),
            crate::timeutil::humanize_unix_secs(last)
        );
    }
    if let Some(sha) = hit.get("commit_sha").and_then(Value::as_str) {
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        let short = sha.get(..12).unwrap_or(sha);
        let _ = write!(detail, "commit `{short}`");
        if let Some(relation) = hit.get("relation").and_then(Value::as_str) {
            let _ = write!(detail, " · {relation}");
        }
        if let Some(evidence) = hit.get("evidence").and_then(Value::as_str) {
            let _ = write!(detail, " via {evidence}");
        }
        if let Some(confidence) = hit.get("confidence").and_then(Value::as_i64) {
            let _ = write!(detail, " ({confidence}/100)");
        }
        if let Some(committed_at) = hit.get("committed_at").and_then(Value::as_i64) {
            let _ = write!(
                detail,
                " at {}",
                crate::timeutil::humanize_unix_secs(committed_at)
            );
        }
    }
    if !detail.is_empty() {
        md.line(&format!("  {detail}"));
    }
}

pub(in super::super) async fn handle_sessions_for(
    cg: &TraceDecay,
    session_db: Option<&RegisteredGlobalDb>,
    args: Value,
) -> Result<ToolResult> {
    let kind = required_string_arg(&args, "git_ref")?;
    let value = required_string_arg(&args, "value")?;
    let git_ref =
        GitRefFilter::parse(kind, value).map_err(|err| argument_error(err.to_string()))?;
    let since = non_negative_timestamp_arg(&args, "since", SearchTimeBound::Start)?;
    let until = non_negative_timestamp_arg(&args, "until", SearchTimeBound::End)?;
    let limit = bounded_usize_arg(&args, "limit", 1, MAX_LCM_RESULT_LIMIT)?
        .unwrap_or(DEFAULT_SESSIONS_FOR_LIMIT);
    let relation = CommitRelationFilter::parse(string_arg(&args, "relation"))
        .map_err(|err| argument_error(err.to_string()))?;
    let query = SessionsForQuery {
        git_ref,
        since,
        until,
        limit,
    };

    let Some(db) = session_db else {
        return Ok(tool_json(
            Some(cg.project_root()),
            &args,
            &json!({
                "status": "unavailable",
                "message": "registered project session database is unavailable",
                "results": [],
                "count": 0
            }),
        ));
    };
    let (results, index_health, observed_fallback) = {
        // Read the correlation-index health from the same open so an empty
        // index (never populated) can be reported distinctly from a populated
        // index that simply had no rows matching this git ref.
        let correlation = crate::store::GlobalDbGitCorrelationStore::new(db);
        let health = correlation.correlation_index_health().await.ok();
        let results = correlation
            .sessions_for_with_relation(&query, relation)
            .await
            .map_err(|err| TraceDecayError::Config {
                message: err.to_string(),
            })?;
        // A commit attributed only by time overlap (or a migrated v2 store) has
        // observed rows but no producer, so the producer-default query is
        // empty. That must not read as "no session touched this commit": look
        // up the observed sessions so the caller can be pointed at them.
        let observed_fallback = if results.is_empty()
            && matches!(query.git_ref, GitRefFilter::Commit(_))
            && relation == CommitRelationFilter::Produced
        {
            correlation
                .sessions_for_with_relation(&query, CommitRelationFilter::Observed)
                .await
                .ok()
                .filter(|hits| !hits.is_empty())
        } else {
            None
        };
        (results, health, observed_fallback)
    };

    // The index is "empty" when there is no store, the correlation tables are
    // absent, or the row family for this ref kind is empty (spans for
    // branch/worktree, commit rows for commit). That must not read as a genuine
    // "no sessions matched" result.
    let index_empty = index_health
        .as_ref()
        .is_none_or(|health| health.is_empty_for(&query.git_ref));
    let mut payload = json!({
        "status": "ok",
        "git_ref": query.git_ref.kind(),
        "value": query.git_ref.value(),
        "since": since,
        "until": until,
        "relation": relation.as_str(),
        "count": results.len(),
        "results": results,
        "index_empty": index_empty,
    });
    if let Some(health) = &index_health {
        payload["index"] = json!({
            "tables_present": health.tables_present,
            "span_count": health.span_count,
            "commit_count": health.commit_count,
            "last_span_write": health.last_span_write,
            "backfill_watermark": health.backfill_watermark,
        });
    }
    // When nothing matched, say *why*: an empty index self-heals via startup
    // auto-backfill (or a manual `tracedecay sessions git-backfill`), whereas a
    // populated index genuinely had no session on this ref.
    if results.is_empty() {
        if let Some(observed) = &observed_fallback {
            payload["observed_count"] = json!(observed.len());
            payload["observed_sessions"] = json!(observed);
            payload["message"] = json!(format!(
                "no producing sessions; {} session(s) observed this commit — pass relation=observed to list them",
                observed.len()
            ));
        } else {
            payload["message"] = json!(if index_empty {
                if matches!(&query.git_ref, GitRefFilter::Commit(_)) {
                    "no commit evidence indexed yet — run `tracedecay sync` to ingest direct host/tool evidence; `tracedecay sessions git-backfill` adds weaker historical overlap evidence"
                } else {
                    "correlation index empty (no git spans recorded yet) — it will auto-backfill on the next MCP server startup, or run `tracedecay sessions git-backfill` to populate it now"
                }
            } else {
                "no sessions matched this git ref"
            });
        }
    }
    Ok(tool_json_with_md(
        Some(cg.project_root()),
        &args,
        &payload,
        || render_sessions_for_md(&payload),
    ))
}

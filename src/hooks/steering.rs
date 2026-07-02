//! Session/steering context builders shared by the Cursor, Claude, and Codex
//! session hooks: index-freshness lines, the workflow-skill index, and the
//! post-compaction context-recovery hint.

use std::path::Path;

use serde_json::Value;

use super::now_unix_secs;

/// Model-invocable workflow skills shipped in the tracedecay Cursor plugin's
/// `skills/` directory (slash dispatchers with `disable-model-invocation:
/// true` are excluded). Kept as one constant so the session steering context
/// and the bundle coverage test in `agents::cursor` stay in sync.
pub const CURSOR_PLUGIN_SKILLS: &[&str] = &[
    "architecture-overview",
    "assessing-test-coverage",
    "atomic-code-edits",
    "auditing-code-safety",
    "cleaning-up-dead-code",
    "code-health-report",
    "cross-branch-investigation",
    "curating-project-memory",
    "drafting-commit-and-pr",
    "exploring-types-and-traits",
    "finding-duplicate-logic",
    "finding-impacted-areas",
    "fixing-build-and-type-errors",
    "inspecting-managed-skills",
    "porting-code",
    "project-status",
    "reading-code-cheaply",
    "recalling-project-memory",
    "recalling-session-context",
    "refactoring-safely",
    "reviewing-a-diff",
    "running-impacted-tests",
    "searching-for-code",
    "tracing-functions",
    "tracking-session-health",
    "using-the-cli",
];

pub(super) const COMPACTION_CONTEXT_RECOVERY_HINT: &str = "Context was just compacted. If important prior-session context seems missing, query TraceDecay session context before assuming the compacted summary is complete. Start with `tracedecay_message_search` or `tracedecay_lcm_expand_query`; use `tracedecay_lcm_describe` and `tracedecay_lcm_expand` when you need the summary DAG sources.";

/// Builds the Cursor `sessionStart` `additional_context` text.
///
/// Intentionally lean: the always-applied plugin rule already carries the
/// tool-routing steering, so repeating it here would burn tokens every
/// session. This adds only the session-specific signals — index freshness,
/// the workflow-skill index, and the tokens-saved counter.
pub fn build_cursor_session_context(
    initialized: bool,
    staleness_hint: Option<&str>,
    tokens_saved: Option<u64>,
) -> String {
    let mut s = index_status_line(initialized, staleness_hint);
    if initialized {
        s.push_str("Workflow skills: tracedecay:");
        s.push_str(&CURSOR_PLUGIN_SKILLS.join(", "));
        s.push_str(" — each maps a common workflow to the right tracedecay tools.\n");
        if let Some(saved) = tokens_saved.filter(|saved| *saved > 0) {
            s.push_str("Tokens saved by tracedecay this session: ");
            s.push_str(&saved.to_string());
            s.push_str(".\n");
        }
    }
    s
}

/// One-line index freshness signal shared by the Cursor and Claude session
/// contexts. Both hosts carry the tool-routing steering in an always-applied
/// rule (Cursor plugin rule, CLAUDE.md), so their session hooks report only
/// session-specific signals.
pub(super) fn index_status_line(initialized: bool, staleness_hint: Option<&str>) -> String {
    if initialized {
        match staleness_hint {
            Some(hint) => format!("tracedecay index status: {hint}.\n"),
            None => "tracedecay index status: initialized.\n".to_string(),
        }
    } else {
        "tracedecay index status: no project index found in this workspace — \
         run `tracedecay init` to enable tracedecay MCP tools.\n"
            .to_string()
    }
}

/// Builds the Codex session/prompt steering context. Codex has no
/// always-applied tracedecay rule, so the full tool-routing steering lives
/// here.
pub fn build_codex_session_context(initialized: bool, staleness_hint: Option<&str>) -> String {
    let status = if initialized {
        HookWorkspaceStatus::Initialized
    } else {
        HookWorkspaceStatus::UnindexedProject
    };
    build_codex_session_context_for_workspace(status, staleness_hint)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookWorkspaceStatus {
    Initialized,
    UnindexedProject,
    Generic,
}

impl HookWorkspaceStatus {
    pub(super) fn as_key(self) -> &'static str {
        match self {
            HookWorkspaceStatus::Initialized => "initialized",
            HookWorkspaceStatus::UnindexedProject => "unindexed_project",
            HookWorkspaceStatus::Generic => "generic",
        }
    }
}

/// Builds the Codex session/prompt context for the detected workspace kind.
pub fn build_codex_session_context_for_workspace(
    status: HookWorkspaceStatus,
    staleness_hint: Option<&str>,
) -> String {
    let mut s = String::new();
    match status {
        HookWorkspaceStatus::Initialized | HookWorkspaceStatus::UnindexedProject => {
            s.push_str(
                "tracedecay is available via MCP. Prefer tracedecay MCP tools \
                 (tracedecay_context, tracedecay_search, tracedecay_callers, tracedecay_callees, \
                 tracedecay_impact, tracedecay_files, tracedecay_affected) over broad file reads \
                 or shell search for codebase exploration, symbol lookup, call graphs, and \
                 impact analysis. Fall back to file reads only when tracedecay cannot answer.\n\
                 If an MCP call errors, times out, or the server is disconnected, every tool \
                 is also a shell command: `tracedecay tool <name> --key value` (`tracedecay \
                 tool` lists tools, `tracedecay tool <name> --help` shows parameters). Use \
                 that CLI instead of querying .tracedecay databases directly or abandoning \
                 tracedecay.\n",
            );
            append_codex_recall_and_registry_guidance(&mut s);
            match status {
                HookWorkspaceStatus::Initialized => match staleness_hint {
                    Some(hint) => {
                        s.push_str("Index status: ");
                        s.push_str(hint);
                        s.push_str(".\n");
                    }
                    None => s.push_str("Index status: initialized.\n"),
                },
                HookWorkspaceStatus::UnindexedProject => s.push_str(
                    "Index status: no project index found in this code workspace — \
                     run `tracedecay init` to enable tracedecay code-graph tools.\n",
                ),
                HookWorkspaceStatus::Generic => {}
            }
        }
        HookWorkspaceStatus::Generic => {
            s.push_str(
                "TraceDecay session context is available via MCP. For prior conversation \
                 recovery, use tracedecay_lcm_expand_query, tracedecay_message_search, and \
                 tracedecay_lcm_describe before asking the user to repeat themselves. When \
                 a durable preference, decision, correction, or pitfall surfaces, store it \
                 proactively with tracedecay_fact_store (action \"add\"). Do NOT store \
                 secrets or credentials, transient errors, environment-specific failures, \
                 one-off narratives, task progress, or soon-stale session outcomes; \
                 recover those from transcripts instead.\n",
            );
            s.push_str("Workspace status: no active project workspace; no setup guidance needed for this prompt.\n");
        }
    }
    s
}

fn append_codex_recall_and_registry_guidance(s: &mut String) {
    s.push_str(
        "For other registered projects or sibling workspaces, check \
         tracedecay_project_list or tracedecay_project_search first; use \
         tracedecay_project_context to confirm the target and pass project_id or \
         project_path to tracedecay_context/search for cross-project code context before \
         scanning parent directories. When the user references prior conversation or \
         missing context, use tracedecay_message_search or tracedecay_lcm_expand_query \
         before asking the user to repeat themselves. When a durable decision, user \
         preference, correction, or pitfall surfaces, store it proactively with \
         tracedecay_fact_store (action \"add\") with calibrated trust — do not wait \
         to be asked. Do NOT store secrets or credentials, transient errors, \
         environment-specific failures, one-off narratives, task progress, or \
         soon-stale session outcomes; recover those from transcripts instead.\n",
    );
}

pub(super) fn append_context_block(context: &mut String, block: &str) {
    if !context.is_empty() && !context.ends_with('\n') {
        context.push('\n');
    }
    context.push_str(block);
    if !block.ends_with('\n') {
        context.push('\n');
    }
}

pub(super) fn append_context_recovery_hint(context: &mut String) {
    if !context.is_empty() && !context.ends_with('\n') {
        context.push('\n');
    }
    context.push_str(COMPACTION_CONTEXT_RECOVERY_HINT);
    context.push('\n');
}

pub(super) fn session_start_from_compaction(event_json: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return false;
    };
    ["source", "trigger", "reason", "boundary_reason"]
        .iter()
        .filter_map(|key| parsed.get(*key).and_then(Value::as_str))
        .any(matches_compaction_source)
}

fn matches_compaction_source(value: &str) -> bool {
    let normalized = value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "compact" | "compaction" | "contextcompacted" | "compression"
    )
}

/// Formats a short relative-age staleness hint from a sync age in seconds.
pub fn cursor_staleness_hint(age_secs: i64) -> String {
    let age = age_secs.max(0);
    if age < 60 {
        "last indexed just now".to_string()
    } else if age < 3_600 {
        format!("last indexed {}m ago", age / 60)
    } else if age < 86_400 {
        format!("last indexed {}h ago", age / 3_600)
    } else {
        format!("last indexed {}d ago", age / 86_400)
    }
}

/// Opens the index once and reads both session-steering signals: the
/// staleness hint and the session tokens-saved counter.
pub(super) async fn cursor_index_signals_for_root(root: &Path) -> (Option<String>, Option<u64>) {
    let Ok(cg) = crate::tracedecay::TraceDecay::open(root).await else {
        return (None, None);
    };
    let last = cg.last_sync_timestamp().await;
    let staleness = (last > 0).then(|| cursor_staleness_hint(now_unix_secs() - last));
    let tokens_saved = cg.get_tokens_saved().await.ok();
    (staleness, tokens_saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_session_start_events_get_recovery_hint() {
        let event = serde_json::json!({ "source": "compact" }).to_string();
        assert!(session_start_from_compaction(&event));

        let mut context = build_codex_session_context(true, None);
        append_context_recovery_hint(&mut context);
        assert!(context.contains("Context was just compacted"));
        assert!(context.contains("tracedecay_lcm_expand_query"));
        assert!(context.contains("tracedecay_lcm_describe"));
    }

    #[test]
    fn non_compact_session_start_events_do_not_get_recovery_hint() {
        let event = serde_json::json!({ "source": "resume" }).to_string();
        assert!(!session_start_from_compaction(&event));
    }

    #[test]
    fn index_status_line_formats_freshness_and_init_nudge() {
        assert_eq!(
            index_status_line(true, Some("last indexed 5m ago")),
            "tracedecay index status: last indexed 5m ago.\n"
        );
        assert_eq!(
            index_status_line(true, None),
            "tracedecay index status: initialized.\n"
        );
        assert!(index_status_line(false, None).contains("run `tracedecay init`"));
    }
}

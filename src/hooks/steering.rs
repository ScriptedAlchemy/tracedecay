//! Shared session/steering context builders.

use std::path::Path;

#[cfg(test)]
use serde_json::Value;

use super::now_unix_secs;

/// Model-invocable skills that Cursor ships in its `skills/` directory.
pub const CURSOR_PLUGIN_SKILLS: &[&str] = &[
    "assessing-impact",
    "code-health",
    "diagnosing-analytics",
    "discovering-tracedecay",
    "editing-safely",
    "exploring-code",
    "fixing-build-and-type-errors",
    "inspecting-managed-skills",
    "investigating-unexpected-changes",
    "managing-session-context",
    "project-memory",
    "reviewing-changes",
    "tracing-functions",
    "using-the-cli",
    "using-tracedecay",
];

pub(super) fn append_tracedecay_bootstrap_context(s: &mut String) {
    s.push_str(
        "TraceDecay project hint: use graph tools when code context is needed; \
         deferred tools may require ToolSearch. Route literal or regex text -> \
         tracedecay_grep; symbol names -> tracedecay_search or \
         tracedecay_find_exact_symbol; concepts -> tracedecay_context; call \
         questions -> tracedecay_callers/callees; impact/tests -> \
         tracedecay_impact, tracedecay_affected, or tracedecay_test_map. Use \
         tracedecay_message_search or \
         tracedecay_lcm_expand_query for prior-session context, and \
         tracedecay_fact_store only for durable non-secret facts. If workflow \
         details are needed, open the bundled tracedecay skill for that task \
         instead of relying on repeated session-start instructions.\n",
    );
}

#[cfg(test)]
pub(super) const COMPACTION_CONTEXT_RECOVERY_HINT: &str = "Context was just compacted. If important prior-session context seems missing, query TraceDecay session context before assuming the compacted summary is complete. Start with `tracedecay_message_search` or `tracedecay_lcm_expand_query`; use `tracedecay_lcm_describe` and `tracedecay_lcm_expand` when you need the summary DAG sources.";

/// Builds the Cursor `sessionStart` `additional_context` text.
pub fn build_cursor_session_context(
    initialized: bool,
    staleness_hint: Option<&str>,
    tokens_saved: Option<u64>,
) -> String {
    let mut s = index_status_line(initialized, staleness_hint);
    if initialized {
        append_tracedecay_bootstrap_context(&mut s);
        s.push_str("Workflow skills: tracedecay:");
        s.push_str(&CURSOR_PLUGIN_SKILLS.join(", "));
        s.push_str(" — each maps a common workflow stage to the right tracedecay tools.\n");
        if let Some(saved) = tokens_saved.filter(|saved| *saved > 0) {
            s.push_str("Tokens saved by tracedecay this session: ");
            s.push_str(&saved.to_string());
            s.push_str(".\n");
        }
    }
    s
}

/// One-line index freshness signal.
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

/// Builds the Codex session/prompt steering context.
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
            if matches!(status, HookWorkspaceStatus::Initialized) {
                append_tracedecay_bootstrap_context(&mut s);
            } else {
                s.push_str(
                    "After initialization, use tracedecay MCP tools (tracedecay_context, \
                     tracedecay_grep, tracedecay_search, tracedecay_callers, \
                     tracedecay_callees, tracedecay_impact, tracedecay_files, \
                     tracedecay_affected) before broad file reads or shell search for codebase \
                     exploration, symbol lookup, call graphs, and impact analysis. Route \
                     searches by target: literal or regex text -> tracedecay_grep; symbol name \
                     -> tracedecay_search; concept -> tracedecay_context; files by role/path \
                     -> tracedecay_files.\n",
                );
            }
            s.push_str(
                "Before `cargo check`/tsc/clippy or after compile errors, use \
                 tracedecay_diagnostics for fresh errors or pass captured output to \
                 tracedecay_diagnose; both map errors to symbols and callers.\n",
            );
            s.push_str(
                "Agents: tracedecay-code-explorer,tracedecay-code-health-auditor,\
                 tracedecay-session-historian,tracedecay-runtime-storage-doctor,\
                 tracedecay-cross-host-integration-auditor,tracedecay-change-risk-reviewer,\
                 tracedecay-usage-intelligence-analyst,tracedecay-automation-auditor\n",
            );
            s.push_str(crate::agents::CLI_FALLBACK_PROMPT_RULES);
            s.push('\n');
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
                 proactively with tracedecay_fact_store (action \"add\") and \
                 memory_scope \"user\". The CLI fallback supports this user scope even \
                 without an initialized project. Do NOT store \
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

#[cfg(test)]
pub(super) fn append_context_recovery_hint(context: &mut String) {
    if !context.is_empty() && !context.ends_with('\n') {
        context.push('\n');
    }
    context.push_str(COMPACTION_CONTEXT_RECOVERY_HINT);
    context.push('\n');
}

#[cfg(test)]
pub(super) fn session_start_from_compaction(event_json: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<Value>(event_json) else {
        return false;
    };
    ["source", "trigger", "reason", "boundary_reason"]
        .iter()
        .filter_map(|key| parsed.get(*key).and_then(Value::as_str))
        .any(matches_compaction_source)
}

#[cfg(test)]
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

/// Opens the index once and reads both session-steering signals.
pub(super) async fn cursor_index_signals_for_root(root: &Path) -> (Option<String>, Option<u64>) {
    let Ok(status) = super::daemon_tool_json(
        Some(root),
        "tracedecay_status",
        serde_json::json!({ "format": "json" }),
    )
    .await
    else {
        return (None, None);
    };
    let last = status
        .get("last_updated")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let staleness = (last > 0).then(|| cursor_staleness_hint(now_unix_secs() - last));
    let tokens_saved = status
        .get("tokens_saved")
        .and_then(serde_json::Value::as_u64);
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
    fn codex_session_context_carries_diagnostics_moment() {
        // Both the initialized and unindexed surfaces must route the shell
        // compile/type-check moment to tracedecay diagnostics.
        for status in [
            HookWorkspaceStatus::Initialized,
            HookWorkspaceStatus::UnindexedProject,
        ] {
            let context = build_codex_session_context_for_workspace(status, None);
            assert!(
                context.contains("tracedecay_diagnostics"),
                "missing tracedecay_diagnostics for {status:?}"
            );
            assert!(
                context.contains("tracedecay_diagnose"),
                "missing tracedecay_diagnose for {status:?}"
            );
            assert!(
                context.contains("cargo check"),
                "missing compile-moment cue for {status:?}"
            );
        }
    }

    #[test]
    fn codex_session_context_advertises_managed_subagents() {
        // Codex sessions have no other discovery surface for the managed
        // tracedecay-* subagents besides this steering line, so it must
        // survive on both the initialized and unindexed code-workspace
        // surfaces, and stay off the generic (non-code-workspace) surface.
        const AGENTS: &[&str] = &[
            "tracedecay-code-explorer",
            "tracedecay-code-health-auditor",
            "tracedecay-session-historian",
            "tracedecay-runtime-storage-doctor",
            "tracedecay-cross-host-integration-auditor",
            "tracedecay-change-risk-reviewer",
            "tracedecay-usage-intelligence-analyst",
            "tracedecay-automation-auditor",
        ];
        for status in [
            HookWorkspaceStatus::Initialized,
            HookWorkspaceStatus::UnindexedProject,
        ] {
            let context = build_codex_session_context_for_workspace(status, None);
            for agent in AGENTS {
                assert_eq!(
                    context.matches(agent).count(),
                    1,
                    "{agent} must be advertised exactly once for {status:?}"
                );
            }
        }

        let generic = build_codex_session_context_for_workspace(HookWorkspaceStatus::Generic, None);
        for agent in AGENTS {
            assert!(
                !generic.contains(agent),
                "generic (non-code-workspace) surface should omit {agent}"
            );
        }
    }

    #[test]
    fn codex_unindexed_context_routes_grep_search_context() {
        // Content/symbol/concept routing must survive on the unindexed surface,
        // which cannot lean on the bootstrap skill for the tool ladder.
        let context =
            build_codex_session_context_for_workspace(HookWorkspaceStatus::UnindexedProject, None);
        assert!(context.contains("literal or regex text -> tracedecay_grep"));
        assert!(context.contains("symbol name -> tracedecay_search"));
        assert!(context.contains("concept -> tracedecay_context"));
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

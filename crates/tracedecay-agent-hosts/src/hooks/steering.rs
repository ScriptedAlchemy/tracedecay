//! Shared session/steering context builders.

/// Model-invocable skills that Cursor ships in its `skills/` directory.
pub use crate::agents::cursor::CURSOR_PLUGIN_SKILLS;

pub(super) fn append_tracedecay_bootstrap_context(s: &mut String) {
    s.push_str(
        "TraceDecay project hint: use the graph when the task needs unfamiliar code \
         structure or relationships. Use tracedecay_context for concepts, \
         tracedecay_search for symbols, tracedecay_grep for literal or regex text, and \
         tracedecay_callers/callees or tracedecay_impact for relationships. Use native \
         reads and edits for known files. Use tracedecay_message_search or \
         tracedecay_lcm_expand_query when prior-session context matters, and \
         tracedecay_fact_store_add only for durable non-secret facts. Load a bundled \
         tracedecay skill when its specific workflow matches the task.\n",
    );
}

/// Character budget for the Cursor `sessionStart` `additional_context` text.
///
/// This is the steering contract, not a test detail: session context is
/// injected on every Cursor session start, so growing it costs every session.
/// Rewording the prose is free; exceeding this budget is a deliberate decision
/// that must be made here, in production, rather than by relaxing a test.
pub const CURSOR_SESSION_CONTEXT_BUDGET: usize = 1_300;

/// Character budget for the Codex session/prompt steering context.
///
/// Same contract as [`CURSOR_SESSION_CONTEXT_BUDGET`]; Codex carries more
/// routing guidance, so its budget is larger.
pub const CODEX_SESSION_CONTEXT_BUDGET: usize = 2_600;

/// Builds the Cursor `sessionStart` `additional_context` text.
pub fn build_cursor_session_context(
    initialized: bool,
    staleness_hint: Option<&str>,
    tokens_saved: Option<u64>,
) -> String {
    let mut s = index_status_line(initialized, staleness_hint);
    if initialized {
        s.reserve(CURSOR_SESSION_CONTEXT_BUDGET.saturating_sub(s.len()));
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
    let mut s = String::with_capacity(CODEX_SESSION_CONTEXT_BUDGET);
    match status {
        HookWorkspaceStatus::Initialized | HookWorkspaceStatus::UnindexedProject => {
            if matches!(status, HookWorkspaceStatus::Initialized) {
                append_tracedecay_bootstrap_context(&mut s);
            } else {
                s.push_str(
                    "TraceDecay graph tools are unavailable until this workspace is initialized. \
                     If the task needs graph-backed code context, run `tracedecay init`; known-file \
                     work can continue with native tools.\n",
                );
            }
            s.push_str(
                "For compiler failures, tracedecay_diagnostics or tracedecay_diagnose can map \
                 errors to affected symbols and callers.\n",
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
                "TraceDecay session context is available via MCP. When prior conversation \
                 context matters, use tracedecay_lcm_expand_query or \
                 tracedecay_message_search. Store durable user preferences, decisions, \
                 corrections, or recurring pitfalls with tracedecay_fact_store_add and \
                 memory_scope \"user\". Do not store secrets, credentials, transient \
                 failures, task progress, or soon-stale outcomes.\n",
            );
            s.push_str("Workspace status: no active project workspace; no setup guidance needed for this prompt.\n");
        }
    }
    s.push_str(
        "Continue authorized work through the requested outcome and relevant verification; \
         pause for a missing decision or an external or destructive action outside that authority.\n",
    );
    s
}

fn append_codex_recall_and_registry_guidance(s: &mut String) {
    s.push_str(
        "For cross-project context, resolve the registered target with \
         tracedecay_project_search or tracedecay_project_context and preserve its project \
         selector. When prior conversation context matters, use tracedecay_message_search \
         or tracedecay_lcm_expand_query. Store durable decisions, preferences, corrections, \
         or recurring pitfalls with tracedecay_fact_store_add. Do not store secrets, \
         credentials, transient failures, task progress, or soon-stale outcomes.\n",
    );
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

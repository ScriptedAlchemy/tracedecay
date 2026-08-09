use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::automation::backend::{AgentTaskKind, AgentTaskResponse};
use crate::automation::config::AutomationConfig;
use crate::automation::lifecycle::AgentTaskRunContext;
use crate::automation::run_ledger::{AutomationRunLedgerRecord, AutomationTrigger};
use crate::errors::{Result, TraceDecayError};

use super::curation::unpersisted_rejected_parts;
use super::session_reflector::{default_include_recent_sessions, default_recent_sessions_limit};
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillWriterAutomationOptions {
    #[serde(default)]
    pub trigger: AutomationTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default = "default_skill_writer_provider")]
    pub provider: String,
    #[serde(default = "default_skill_writer_query")]
    pub query: String,
    #[serde(default = "default_skill_writer_evidence_limit")]
    pub evidence_limit: usize,
    /// When true, include bounded turn-ordered slices of recently active
    /// sessions as a primary evidence channel alongside the keyword grep.
    #[serde(default = "default_include_recent_sessions")]
    pub include_recent_sessions: bool,
    /// How many recently active sessions to replay.
    #[serde(default = "default_recent_sessions_limit")]
    pub recent_sessions_limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_root: Option<PathBuf>,
}

impl Default for SkillWriterAutomationOptions {
    fn default() -> Self {
        Self {
            trigger: AutomationTrigger::ManualCli,
            run_id: None,
            provider: default_skill_writer_provider(),
            query: default_skill_writer_query(),
            evidence_limit: default_skill_writer_evidence_limit(),
            include_recent_sessions: default_include_recent_sessions(),
            recent_sessions_limit: default_recent_sessions_limit(),
            profile_root: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillWriterAutomationRun {
    pub run_id: String,
    pub report: Value,
    pub ledger_record: AutomationRunLedgerRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_response: Option<AgentTaskResponse>,
}

pub(super) enum SkillWriterFinalization {
    Completed {
        report: Value,
        record: AutomationRunLedgerRecord,
    },
    FailedRecorded {
        error: TraceDecayError,
        record: AutomationRunLedgerRecord,
    },
}

pub(super) fn default_skill_writer_provider() -> String {
    "all".to_string()
}

pub(super) fn default_skill_writer_query() -> String {
    "workflow correction repeated skill tool pattern".to_string()
}

fn default_skill_writer_evidence_limit() -> usize {
    20
}

pub(super) fn build_skill_writer_prompt(evidence: &Value) -> String {
    const POLICY: &str = concat!(
        "Review these bounded TraceDecay session snippets and propose only reusable managed skills for repeated workflows, corrections, or tool-use patterns.\n",
        "Evidence has two channels: recent_session_slices holds turn-ordered head/tail turns and summary nodes replayed from recently active sessions, and hits holds keyword search matches.\n",
        "\n",
        "Target shape of the skill library: CLASS-LEVEL umbrella skills, each with a rich body and support files for session-specific detail — not a long flat list of narrow one-session-one-skill entries. This shapes HOW you update, not WHETHER you update.\n",
        "\n",
        "Signals that warrant a skill proposal (any one is enough):\n",
        "- The user corrected the agent's style, tone, format, verbosity, workflow, or approach. Frustration signals like 'stop doing X', 'this is too verbose', 'don't format like this', 'you always do Y and I hate it', or an explicit 'remember this' are FIRST-CLASS skill signals, not just memory signals. Embed the correction in the body of the skill that governs that class of task so the next session starts already knowing; a memory fact alone is not enough.\n",
        "- A non-trivial technique, fix, workaround, debugging path, or tool-usage pattern emerged that a future session would benefit from.\n",
        "- A skill that evidence shows was used or loaded this session turned out to be wrong, missing a step, or outdated. Patch it now.\n",
        "\n",
        "Preference order — pick the EARLIEST action that fits:\n",
        "1. UPDATE a skill that the evidence (skill_usage_summaries, skill_improvement_recommendations, existing_managed_skills) shows was used or loaded recently. It was in play, so it is the right one to extend.\n",
        "2. PATCH an existing umbrella skill from existing_managed_skills whose class covers the new learning. Add a subsection, a pitfall, or broaden a trigger.\n",
        "3. ADD to an existing skill's scope via its support_files (reference notes, templates, or re-runnable snippets), with a one-line pointer in the skill body so future sessions find it.\n",
        "4. CREATE a new skill only when nothing existing fits. The name MUST be at the class level and MUST survive the test: 'does this name only make sense for today's task?' If yes, it is wrong — no PR numbers, error strings, feature codenames, or fix-X/debug-Y session artifacts. Fall back to option 1, 2, or 3 instead.\n",
        "\n",
        "Do NOT capture (these become persistent self-imposed constraints that bite later when the environment changes):\n",
        "- Environment-dependent failures: missing binaries, 'command not found', unconfigured credentials, uninstalled packages, post-migration path mismatches. The user can fix these; they are not durable rules.\n",
        "- Negative claims about tools or features ('X is broken', 'browser tools do not work'). These harden into refusals the agent cites against itself long after the actual problem was fixed. If a tool failed because of setup state, capture the FIX (install command, config step, env var) under an existing setup or troubleshooting skill — never 'this tool does not work' as a standalone constraint.\n",
        "- Session-specific transient errors that resolved before the session ended. If retrying worked, the lesson is the retry pattern, not the original failure.\n",
        "- One-off task narratives. A single 'summarize this' or 'analyze this PR' request is not a class of work that warrants a skill.\n",
        "- Secrets, credentials, or tokens in any skill body or support file.\n",
        "\n",
        "An empty skills array is a real option when the session ran smoothly with no corrections and produced no new technique, but do not reach for it as a default.\n",
        "\n",
        "Response contract: Return only JSON with a skills array of managed skill creates or updates. New skills may omit action or use action=create and must include id, title, summary, category, body_markdown, optional targets, optional support_files with text content, and reason. Targets, when present, must be an array using cursor, codex, claude, agents, opencode, kimi, kiro, or hermes; Hermes exports are generated read-only under the TraceDecay plugin package and never overwrite host-owned user skills. Updates must use action=update or action=patch, include id and base_checksum, and include at least one changed field among title, summary, category, targets, body_markdown/body, support_files, or pinned. For updates, support_files is a complete replacement list, not a partial file patch. Consolidations: when skill_overlap_candidates shows overlapping managed skills, you may propose action=merge (include id for the surviving skill, base_checksum, source_skill_id, source_base_checksum, reason, and optional merged title/summary/category/targets/body_markdown/support_files) or action=archive (include id, base_checksum, reason). Consolidations preserve archived source content. Valid proposals are activated and exported automatically. Never propose merge or archive for pinned or user-authored skills.\n",
    );
    format!(
        "{POLICY}{}",
        serde_json::to_string_pretty(evidence).unwrap_or_else(|_| "{}".to_string())
    )
}

pub(super) async fn skipped_skill_writer_run(
    run: &AgentTaskRunContext<'_>,
    reason: &str,
    evidence_hash: Option<String>,
) -> Result<SkillWriterAutomationRun> {
    let (report, record) = run
        .skipped_parts(evidence_hash, reason, Some("skill_writer"))
        .await?;
    Ok(SkillWriterAutomationRun {
        run_id: run.run_id.clone(),
        report,
        ledger_record: record,
        backend_response: None,
    })
}

pub(super) fn rejected_skill_writer_run(
    run: &AgentTaskRunContext<'_>,
    config: &AutomationConfig,
    reason: &str,
    evidence_hash: Option<String>,
) -> SkillWriterAutomationRun {
    let (report, record) = unpersisted_rejected_parts(
        run,
        config,
        AgentTaskKind::SkillWriter,
        reason,
        evidence_hash,
        "skill_writer",
    );
    SkillWriterAutomationRun {
        run_id: run.run_id.clone(),
        report,
        ledger_record: record,
        backend_response: None,
    }
}

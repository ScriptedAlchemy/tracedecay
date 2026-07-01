use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};

#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub enum AutomationAction {
    /// Read or mutate the project automation sidecar config.
    Config {
        #[command(subcommand)]
        action: AutomationConfigAction,
    },
    /// Run an explicit self-improvement automation job.
    Run {
        #[command(subcommand)]
        action: AutomationRunAction,
    },
    /// Inspect automation run history.
    Runs {
        #[command(subcommand)]
        action: AutomationRunsAction,
    },
    /// Manage profile-owned automation skills and approvals.
    Skills {
        #[command(subcommand)]
        action: AutomationSkillsAction,
    },
    /// Review and apply session-reflection fact proposals.
    Facts {
        #[command(subcommand)]
        action: AutomationFactsAction,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum AutomationConfigScope {
    Project,
    Global,
}

#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub enum AutomationConfigAction {
    /// Print effective automation config.
    Get {
        /// Config scope to inspect.
        #[arg(long, value_enum, default_value_t = AutomationConfigScope::Project)]
        scope: AutomationConfigScope,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
        /// Project path (default: current directory, with discovery).
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Explain effective automation config, merge source, and backend availability.
    Explain {
        /// Config scope to inspect.
        #[arg(long, value_enum, default_value_t = AutomationConfigScope::Project)]
        scope: AutomationConfigScope,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
        /// Project path (default: current directory, with discovery).
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Enable project automation.
    Enable {
        /// Config scope to mutate.
        #[arg(long, value_enum, default_value_t = AutomationConfigScope::Project)]
        scope: AutomationConfigScope,
        /// Project path (default: current directory, with discovery).
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Disable project automation.
    Disable {
        /// Config scope to mutate.
        #[arg(long, value_enum, default_value_t = AutomationConfigScope::Project)]
        scope: AutomationConfigScope,
        /// Project path (default: current directory, with discovery).
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Patch project automation config fields.
    Set {
        /// Config scope to mutate.
        #[arg(long, value_enum, default_value_t = AutomationConfigScope::Project)]
        scope: AutomationConfigScope,
        /// Backend: disabled, codex-app-server.
        #[arg(long)]
        backend: Option<String>,
        /// Host mode: standalone, delegated-host.
        #[arg(long)]
        host_mode: Option<String>,
        /// Model id. Empty string clears the project override.
        #[arg(long)]
        model: Option<String>,
        /// Timeout in seconds.
        #[arg(long)]
        timeout_secs: Option<u64>,
        /// Scheduler polling cadence in seconds.
        #[arg(long)]
        scheduler_tick_secs: Option<u64>,
        /// Maximum backend output tokens. Empty string clears the override.
        #[arg(long)]
        max_tokens: Option<String>,
        /// Backend sampling temperature. Empty string clears the override.
        #[arg(long)]
        temperature: Option<String>,
        /// Require dashboard approval before applying generated changes.
        #[arg(long)]
        require_dashboard_approval: Option<bool>,
        /// Allow accepted memory operations to apply automatically when policy permits.
        #[arg(long)]
        auto_apply_memory_ops: Option<bool>,
        /// Allow generated skills to become active automatically when policy permits.
        #[arg(long)]
        auto_enable_skills: Option<bool>,
        /// Enable or disable the memory curator task.
        #[arg(long)]
        memory_curator: Option<bool>,
        /// Schedule label for the memory curator task. Empty string clears it.
        #[arg(long)]
        memory_curator_schedule: Option<String>,
        /// Memory curator interval seconds. Empty string clears it.
        #[arg(long)]
        memory_curator_interval_secs: Option<String>,
        /// Memory curator cooldown seconds. Empty string clears it.
        #[arg(long)]
        memory_curator_cooldown_secs: Option<String>,
        /// Memory curator idle seconds. Empty string clears it.
        #[arg(long)]
        memory_curator_min_idle_secs: Option<String>,
        /// Memory curator stale-lock seconds. Empty string clears it.
        #[arg(long)]
        memory_curator_stale_lock_secs: Option<String>,
        /// Enable or disable the session reflector task.
        #[arg(long)]
        session_reflector: Option<bool>,
        /// Schedule label for the session reflector task. Empty string clears it.
        #[arg(long)]
        session_reflector_schedule: Option<String>,
        /// Session reflector interval seconds. Empty string clears it.
        #[arg(long)]
        session_reflector_interval_secs: Option<String>,
        /// Session reflector cooldown seconds. Empty string clears it.
        #[arg(long)]
        session_reflector_cooldown_secs: Option<String>,
        /// Session reflector idle seconds. Empty string clears it.
        #[arg(long)]
        session_reflector_min_idle_secs: Option<String>,
        /// Session reflector stale-lock seconds. Empty string clears it.
        #[arg(long)]
        session_reflector_stale_lock_secs: Option<String>,
        /// Enable or disable the skill writer task.
        #[arg(long)]
        skill_writer: Option<bool>,
        /// Schedule label for the skill writer task. Empty string clears it.
        #[arg(long)]
        skill_writer_schedule: Option<String>,
        /// Skill writer interval seconds. Empty string clears it.
        #[arg(long)]
        skill_writer_interval_secs: Option<String>,
        /// Skill writer cooldown seconds. Empty string clears it.
        #[arg(long)]
        skill_writer_cooldown_secs: Option<String>,
        /// Skill writer idle seconds. Empty string clears it.
        #[arg(long)]
        skill_writer_min_idle_secs: Option<String>,
        /// Skill writer stale-lock seconds. Empty string clears it.
        #[arg(long)]
        skill_writer_stale_lock_secs: Option<String>,
        /// Project path (default: current directory, with discovery).
        #[arg(short, long)]
        path: Option<String>,
    },
}

#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub enum AutomationRunAction {
    /// Build a memory-curation review, call the configured backend, and validate proposed ops.
    #[command(name = "memory-curation")]
    MemoryCuration {
        /// Keep the run non-mutating. This is currently the only supported mode.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        dry_run: bool,
        /// Maximum candidate clusters included in the backend review request.
        #[arg(long, default_value_t = tracedecay::dashboard::memory_curate::CURATION_DEFAULT_MAX_CLUSTERS)]
        max_clusters: usize,
        /// Confidence floor below which backend ops are rejected.
        #[arg(long, default_value_t = tracedecay::dashboard::memory_curate::CURATION_DEFAULT_MIN_CONFIDENCE)]
        min_confidence: f64,
        /// Project path (default: current directory, with discovery).
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Build a session-reflection fact proposal review from LCM evidence.
    #[command(name = "session-reflection")]
    SessionReflection {
        /// Keep the run non-mutating. This is currently the only supported mode.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        dry_run: bool,
        /// LCM provider to inspect.
        #[arg(long, default_value = "cursor")]
        provider: String,
        /// LCM grep query used to collect bounded evidence.
        #[arg(long, default_value = "remember prefer decision requirement workflow")]
        query: String,
        /// Maximum LCM evidence snippets included in the backend review request.
        #[arg(long, default_value_t = 20)]
        evidence_limit: usize,
        /// LCM storage scope: project_local or hermes_profile.
        #[arg(long, default_value = "project_local")]
        storage_scope: String,
        /// Absolute Hermes profile home directory when --storage-scope hermes_profile.
        #[arg(long)]
        hermes_home: Option<PathBuf>,
        /// LCM grep scope: all, session, or current.
        #[arg(long, default_value = "all")]
        scope: String,
        /// Provider-local session id when --scope session/current or to filter all-scope evidence.
        #[arg(long)]
        session_id: Option<String>,
        /// Include LCM summary nodes when no raw-message-only filters are active.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        include_summaries: bool,
        /// LCM grep sort: recency, relevance, or hybrid.
        #[arg(long, default_value = "recency")]
        sort: String,
        /// Optional LCM raw-message source filter.
        #[arg(long)]
        source: Option<String>,
        /// Optional LCM raw-message role filter.
        #[arg(long)]
        role: Option<String>,
        /// Optional inclusive minimum raw-message timestamp.
        #[arg(long)]
        start_time: Option<i64>,
        /// Optional inclusive maximum raw-message timestamp.
        #[arg(long)]
        end_time: Option<i64>,
        /// Project path (default: current directory, with discovery).
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Draft managed skills from repeated workflow evidence without activating them.
    #[command(name = "skill-writing")]
    SkillWriting {
        /// Keep the run non-mutating. This is currently the only supported mode.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        dry_run: bool,
        /// LCM provider to inspect. Use all for unified cross-provider evidence.
        #[arg(long, default_value = "all")]
        provider: String,
        /// LCM grep query used to collect bounded evidence.
        #[arg(
            long,
            default_value = "workflow correction repeated skill tool pattern"
        )]
        query: String,
        /// Maximum LCM evidence snippets included in the backend review request.
        #[arg(long, default_value_t = 20)]
        evidence_limit: usize,
        /// LCM storage scope: project_local or hermes_profile.
        #[arg(long, default_value = "project_local")]
        storage_scope: String,
        /// Absolute Hermes profile home directory when --storage-scope hermes_profile.
        #[arg(long)]
        hermes_home: Option<PathBuf>,
        /// Project path (default: current directory, with discovery).
        #[arg(short, long)]
        path: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum AutomationRunsAction {
    /// List recent automation runs.
    List {
        /// Maximum number of newest runs to return.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Project path (default: current directory, with discovery).
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Show one automation run by run id.
    View {
        run_id: String,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Project path (default: current directory, with discovery).
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Read a verified automation run artifact payload.
    Artifact {
        run_id: String,
        /// Artifact kind, such as codex_handoff or validation_gate.
        kind: String,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Project path (default: current directory, with discovery).
        #[arg(short, long)]
        path: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum AutomationSkillsAction {
    /// List managed skills.
    List {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show one managed skill.
    View {
        id: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Create a pending managed skill draft.
    Draft {
        #[arg(long)]
        id: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        summary: String,
        #[arg(long)]
        category: String,
        #[arg(long)]
        body: String,
        /// Pin the skill against future stale/archive recommendations.
        #[arg(long, default_value_t = false)]
        pinned: bool,
    },
    /// Update an existing skill and restage content changes for approval.
    Update {
        id: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        summary: Option<String>,
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        pinned: Option<bool>,
    },
    /// Approve a pending skill.
    Approve { id: String },
    /// Disable an active or pending skill.
    Disable { id: String },
    /// Archive a managed skill.
    Archive { id: String },
    /// Restore an archived skill back to pending approval.
    Restore { id: String },
    /// Export approved managed skills into a host plugin overlay or prompt index.
    Install {
        /// Host target to install for.
        #[arg(long, value_enum)]
        target: AutomationSkillsInstallTarget,
        /// Plugin root for cursor/codex, or prompt/index file for prompt targets.
        #[arg(long, value_name = "PATH")]
        output: String,
        /// For Codex, write a complete shareable plugin bundle instead of only a managed-skill overlay.
        #[arg(long)]
        plugin_artifact: bool,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AutomationSkillsInstallTarget {
    Cursor,
    Codex,
    Claude,
    Agents,
    #[value(alias = "opencode")]
    OpenCode,
    Kimi,
    Kiro,
    Hermes,
}

impl From<AutomationSkillsInstallTarget>
    for tracedecay::automation::skill_targets::SkillInstallTarget
{
    fn from(value: AutomationSkillsInstallTarget) -> Self {
        match value {
            AutomationSkillsInstallTarget::Cursor => Self::Cursor,
            AutomationSkillsInstallTarget::Codex => Self::Codex,
            AutomationSkillsInstallTarget::Claude => Self::Claude,
            AutomationSkillsInstallTarget::Agents => Self::Agents,
            AutomationSkillsInstallTarget::OpenCode => Self::OpenCode,
            AutomationSkillsInstallTarget::Kimi => Self::Kimi,
            AutomationSkillsInstallTarget::Kiro => Self::Kiro,
            AutomationSkillsInstallTarget::Hermes => Self::Hermes,
        }
    }
}

#[derive(Subcommand)]
pub enum AutomationFactsAction {
    /// List session-reflection fact proposals.
    List {
        /// Proposal state filter: pending_approval, applied, rejected, rejected_validation.
        #[arg(long)]
        state: Option<String>,
        /// Maximum proposals to show.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Project path (default: current directory, with discovery).
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Show one fact proposal.
    View {
        id: String,
        /// Project path (default: current directory, with discovery).
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Approve and apply a pending fact proposal to memory.
    Apply {
        id: String,
        /// Project path (default: current directory, with discovery).
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Reject a pending fact proposal.
    Reject {
        id: String,
        /// Optional decision reason.
        #[arg(long)]
        reason: Option<String>,
        /// Project path (default: current directory, with discovery).
        #[arg(short, long)]
        path: Option<String>,
    },
}

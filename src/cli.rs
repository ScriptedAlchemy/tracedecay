use clap::{builder::PossibleValuesParser, Parser, Subcommand, ValueEnum};

fn agent_value_parser() -> PossibleValuesParser {
    PossibleValuesParser::new(tracedecay::agents::available_integrations())
}

/// Code intelligence for Rust codebases.
#[derive(Parser)]
#[command(
    name = "tracedecay",
    about = "Code intelligence for 34 languages — semantic graph queries instead of file reads",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub enum Commands {
    /// Initialize a new TraceDecay project (full index)
    Init {
        /// Project path (default: current directory)
        path: Option<String>,
        /// Folders to skip during indexing (can be repeated)
        #[arg(long = "skip-folder", num_args = 1..)]
        skip_folders: Vec<String>,
        /// Folders to include even when ignored by default skips or .gitignore (can be repeated)
        #[arg(long = "include-folder", num_args = 1..)]
        include_folders: Vec<String>,
    },
    /// Incremental sync (project must already be initialized with `tracedecay init`)
    Sync {
        /// Project path (default: current directory)
        path: Option<String>,
        /// Force a full re-index
        #[arg(short, long)]
        force: bool,
        /// Folders to skip during indexing (can be repeated)
        #[arg(long = "skip-folder", num_args = 1..)]
        skip_folders: Vec<String>,
        /// Folders to include even when ignored by default skips or .gitignore (can be repeated)
        #[arg(long = "include-folder", num_args = 1..)]
        include_folders: Vec<String>,
        /// List added, modified, and removed files after sync
        #[arg(long)]
        doctor: bool,
        /// Print per-phase diagnostics (file counts, timings) to help debug slow syncs
        #[arg(short, long)]
        verbose: bool,
    },
    /// Show project statistics
    Status {
        /// Project path (default: current directory)
        path: Option<String>,
        /// Registered project id to inspect instead of discovering from cwd
        #[arg(long, conflicts_with = "path")]
        project_id: Option<String>,
        /// Registered project root path or alias to inspect instead of discovering from cwd
        #[arg(long, conflicts_with_all = ["path", "project_id"])]
        project_path: Option<String>,
        /// Output as JSON
        #[arg(short, long)]
        json: bool,
        /// Show only the header (version, tokens, sync times)
        #[arg(short, long)]
        short: bool,
        /// Show node-kind breakdown
        #[arg(short, long)]
        details: bool,
        /// Capture a runtime telemetry snapshot (PID, RSS, CPU%, DB / WAL
        /// sizes) — useful when reporting unexpected resource use (#80).
        #[arg(long)]
        runtime: bool,
    },
    /// Invoke an MCP tool from the CLI (e.g. `tracedecay tool search foo`).
    ///
    /// Run `tracedecay tool` (no name) to list every available tool.
    /// Run `tracedecay tool <name> --help` to see that tool's parameters.
    //
    // `disable_help_flag = true` lets `-h`/`--help` flow through to our parser
    // so we can print the per-tool schema instead of clap's generic help.
    #[command(disable_help_flag = true)]
    Tool {
        /// Project root to open before dispatching the tool. Defaults to the
        /// nearest initialised project walking up from cwd.
        #[arg(long)]
        project: Option<String>,
        /// MCP tool name (with or without the `tracedecay_` prefix). Omit to list all tools.
        name: Option<String>,
        /// Tool arguments as alternating `--key value` flags, plus reserved flags
        /// `--json`, `--project <path>`, `--args <json>`, and `-h`/`--help`.
        /// Any value starting with `@` is read from that file (handy for
        /// multi-line replacement bodies).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Configure agent integration (MCP server, permissions, hooks, prompt rules)
    #[command(name = "install", visible_alias = "claude-install")]
    Install {
        /// Agent to configure (auto-detects if omitted)
        #[arg(long, value_parser = agent_value_parser())]
        agent: Option<String>,
        /// Write project-local configuration in the current directory
        #[arg(long)]
        local: bool,
        /// Hermes profile to install into (only used with --agent hermes)
        #[arg(long)]
        profile: Option<String>,
        /// Install into the default profile and every Hermes profile directory
        #[arg(long, conflicts_with = "profile")]
        all_profiles: bool,
        /// Pin the generated plugin to a project root (absolute path; only
        /// used with --agent hermes). All plugin tool calls then resolve that
        /// project's .tracedecay/ stores regardless of the Hermes cwd.
        #[arg(long, conflicts_with = "all_profiles")]
        project_root: Option<String>,
        /// Skip deploying the tracedecay dashboard plugin page into the
        /// Hermes dashboard (and remove a previously deployed one; only
        /// used with --agent hermes).
        #[arg(long)]
        no_dashboard: bool,
        /// Install/update a Codex-native project automation in ~/.codex
        /// (only used with --agent codex).
        #[arg(long)]
        automation: bool,
    },
    /// Refresh settings for all already-installed agents
    Reinstall,
    /// Refresh generated plugin code/assets for detected installs without
    /// touching agent config files.
    ///
    /// Rewrites only tracedecay-generated artifacts — the Hermes plugin
    /// (.py files, schemas.json, dashboard page) for every detected profile,
    /// the Cursor plugin bundle, the Codex plugin bundle/cache, and the Kiro
    /// managed agent — re-baking the current binary path and version. Config
    /// files (Hermes config.yaml and its project_root pin, mcp.json, settings,
    /// prompt rules) are left byte-for-byte intact; use `tracedecay reinstall`
    /// to refresh those.
    #[command(name = "update-plugin", visible_alias = "update-plugins")]
    UpdatePlugin,
    /// Remove agent integration (MCP server, permissions, hooks, prompt rules)
    #[command(name = "uninstall", visible_alias = "claude-uninstall")]
    Uninstall {
        /// Agent to remove (removes all if omitted)
        #[arg(long, value_parser = agent_value_parser())]
        agent: Option<String>,
        /// Hermes profile to uninstall from (only used with --agent hermes)
        #[arg(long)]
        profile: Option<String>,
        /// Uninstall from the default profile and every Hermes profile directory
        #[arg(long, conflicts_with = "profile")]
        all_profiles: bool,
    },
    /// Extraction worker (spawned by tracedecay itself; not for direct use).
    #[command(name = "extract-worker", hide = true)]
    ExtractWorker,
    /// PreToolUse hook handler (called by Claude Code, not by users directly)
    #[command(name = "hook-pre-tool-use", hide = true)]
    HookPreToolUse,
    /// UserPromptSubmit hook handler (resets session counter)
    #[command(name = "hook-prompt-submit", hide = true)]
    HookPromptSubmit,
    /// Stop hook handler (prints session token savings)
    #[command(name = "hook-stop", hide = true)]
    HookStop,
    /// Kiro PreToolUse hook handler (called by Kiro, not by users directly)
    #[command(name = "hook-kiro-pre-tool-use", hide = true)]
    HookKiroPreToolUse,
    /// Kiro UserPromptSubmit hook handler (called by Kiro, not by users directly)
    #[command(name = "hook-kiro-prompt-submit", hide = true)]
    HookKiroPromptSubmit,
    /// Kiro PostToolUse hook handler for incremental sync
    #[command(name = "hook-kiro-post-tool-use", hide = true)]
    HookKiroPostToolUse,
    /// Cursor subagentStart hook handler (called by Cursor, not by users directly)
    #[command(name = "hook-cursor-subagent-start", hide = true)]
    HookCursorSubagentStart,
    /// Cursor postToolUse hook handler (called by Cursor, not by users directly)
    #[command(name = "hook-cursor-post-tool-use", hide = true)]
    HookCursorPostToolUse,
    /// Cursor beforeSubmitPrompt hook handler (called by Cursor, not by users directly)
    #[command(name = "hook-cursor-before-submit-prompt", hide = true)]
    HookCursorBeforeSubmitPrompt,
    /// Cursor preCompact hook handler (called by Cursor, not by users directly)
    #[command(name = "hook-cursor-pre-compact", hide = true)]
    HookCursorPreCompact,
    /// Cursor afterFileEdit hook handler (called by Cursor, not by users directly)
    #[command(name = "hook-cursor-after-file-edit", hide = true)]
    HookCursorAfterFileEdit,
    /// Cursor sessionStart hook handler (called by Cursor, not by users directly)
    #[command(name = "hook-cursor-session-start", hide = true)]
    HookCursorSessionStart,
    /// Cursor sessionEnd hook handler (called by Cursor, not by users directly)
    #[command(name = "hook-cursor-session-end", hide = true)]
    HookCursorSessionEnd,
    /// Cursor afterShellExecution hook handler (called by Cursor, not by users directly)
    #[command(name = "hook-cursor-after-shell", hide = true)]
    HookCursorAfterShell,
    /// Cursor workspaceOpen hook handler (called by Cursor, not by users directly)
    #[command(name = "hook-cursor-workspace-open", hide = true)]
    HookCursorWorkspaceOpen,
    /// Cursor stop hook handler (called by Cursor, not by users directly)
    #[command(name = "hook-cursor-stop", hide = true)]
    HookCursorStop,
    /// Codex SessionStart hook handler (called by Codex, not by users directly)
    #[command(name = "hook-codex-session-start", hide = true)]
    HookCodexSessionStart,
    /// Codex UserPromptSubmit hook handler (called by Codex, not by users directly)
    #[command(name = "hook-codex-user-prompt-submit", hide = true)]
    HookCodexUserPromptSubmit,
    /// Codex SubagentStart hook handler (called by Codex, not by users directly)
    #[command(name = "hook-codex-subagent-start", hide = true)]
    HookCodexSubagentStart,
    /// Codex PostToolUse hook handler for incremental sync (called by Codex)
    #[command(name = "hook-codex-post-tool-use", hide = true)]
    HookCodexPostToolUse,
    /// Codex PostCompact hook handler for app-server LCM summaries (called by Codex)
    #[command(name = "hook-codex-post-compact", hide = true)]
    HookCodexPostCompact,
    /// Serve the local dashboard UI (holographic memory + LCM + code graph explorers)
    Dashboard {
        /// Project path (default: current directory, with discovery)
        #[arg(short, long)]
        path: Option<String>,
        /// Address to bind
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port to listen on (0 = pick a free port)
        #[arg(long, default_value_t = tracedecay::dashboard::DEFAULT_PORT)]
        port: u16,
        /// Open the dashboard URL in the default browser after the server starts
        #[arg(long)]
        open: bool,
    },
    /// Start MCP server over stdio
    Serve {
        /// Project path
        #[arg(short, long)]
        path: Option<String>,
        /// Annotate every `tools/call` response with `_meta.duration_us`,
        /// reporting the handler's pure execution time in microseconds.
        /// Useful for profiling index work vs. JSON-RPC / stdio overhead.
        #[arg(long)]
        timings: bool,
    },
    /// Manage the long-running TraceDecay daemon used by MCP clients
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Download and install the latest version from GitHub
    Upgrade,
    /// Refresh the tracedecay binary, generated plugins, and daemon
    Update,
    /// Refresh plugins and daemon after the binary has been updated.
    #[command(name = "post-update", hide = true)]
    PostUpdate,
    /// Show or switch the update channel (stable or beta)
    Channel {
        /// Target channel: "stable" or "beta" (omit to show current)
        channel: Option<String>,
    },
    /// Show the resettable project-local token counter
    #[command(name = "current-counter")]
    CurrentCounter {
        /// Project path (default: current directory)
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Reset the project-local token counter to zero
    #[command(name = "reset-counter")]
    ResetCounter {
        /// Project path (default: current directory)
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Disable uploading token counts to the worldwide counter
    #[command(name = "disable-upload-counter")]
    DisableUploadCounter,
    /// Enable uploading token counts to the worldwide counter
    #[command(name = "enable-upload-counter")]
    EnableUploadCounter,
    /// Show or change whether .gitignore rules are respected during indexing
    #[command(name = "gitignore")]
    Gitignore {
        /// Project path (default: current directory)
        #[arg(short, long)]
        path: Option<String>,
        /// "on" to enable, "off" to disable, omit to show current setting
        action: Option<String>,
    },
    /// Check tracedecay installation, configuration, and agent integration
    Doctor {
        /// Check only this agent (default: all agents)
        #[arg(long, value_parser = agent_value_parser())]
        agent: Option<String>,
    },
    /// Token cost summary from Claude Code sessions
    Cost {
        /// Time range: "today", "7d", "30d", "month", or "all"
        #[arg(default_value = "7d")]
        range: String,
        /// Group by model
        #[arg(long)]
        by_model: bool,
        /// Group by task category
        #[arg(long)]
        by_task: bool,
        /// Export format: csv or json
        #[arg(long)]
        export: Option<String>,
    },
    /// Run a reproducible retrieval benchmark against the current project.
    Bench {
        /// Path to a TOML query file (defaults to the shipped default set).
        #[arg(long)]
        queries: Option<String>,
        /// Output as JSON instead of the colored console table.
        #[arg(long)]
        json: bool,
        /// Project path (default: current directory).
        #[arg(short, long)]
        path: Option<String>,
        /// Max nodes per query (default: 20).
        #[arg(long, default_value = "20")]
        max_nodes: usize,
    },
    /// Show token savings (and dollar estimates) recorded in the global ledger.
    Gain {
        /// Show all projects (default: only the current project).
        #[arg(short, long)]
        all: bool,
        /// Print per-day history instead of a single total.
        #[arg(long)]
        history: bool,
        /// Time range: "today", "7d", "30d", "month", or "all" (default: "30d").
        #[arg(long, default_value = "30d")]
        range: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Live token savings monitor (global, all projects)
    Monitor,
    /// Ingest and search local agent session transcripts
    Sessions {
        #[command(subcommand)]
        action: SessionsAction,
    },
    /// Manage multi-branch indexing
    Branch {
        #[command(subcommand)]
        action: BranchAction,
    },
    /// Holographic memory maintenance (curation without the dashboard)
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// Self-improvement automation config and manual runs
    Automation {
        #[command(subcommand)]
        action: AutomationAction,
    },
    /// Inspect stores before profile-storage migration
    Migrate {
        #[command(subcommand)]
        action: MigrateAction,
    },
    /// Wipe local tracedecay DBs (current folder, parents, and children)
    Wipe {
        /// Wipe ALL tracked projects so the global DB ends empty
        #[arg(short, long)]
        all: bool,
    },
    /// List tracedecay projects (current folder, parents, and children)
    List {
        /// List ALL tracked projects from the global DB
        #[arg(short, long)]
        all: bool,
    },
}

#[derive(Subcommand)]
pub enum DaemonAction {
    /// Run the foreground daemon process
    Run {
        /// Unix socket path for MCP clients
        #[arg(long)]
        socket: Option<String>,
    },
    /// Install the daemon as a user service
    #[command(name = "install-service")]
    InstallService {
        /// Unix socket path for MCP clients
        #[arg(long)]
        socket: Option<String>,
        /// Write the service file but do not start/enable it
        #[arg(long)]
        no_start: bool,
    },
    /// Remove the installed daemon user service
    #[command(name = "uninstall-service")]
    UninstallService {
        /// Remove the service file but do not stop/disable a running service
        #[arg(long)]
        no_stop: bool,
    },
    /// Print daemon service/socket status
    Status,
}

#[derive(Subcommand)]
pub enum SessionsAction {
    /// Ingest all supported transcript providers into the project session DB
    Ingest {
        /// Deprecated compatibility option; ingest always sweeps all supported providers
        #[arg(long)]
        provider: Option<String>,
        /// Registered project id whose session store should receive ingested messages
        #[arg(long)]
        project_id: Option<String>,
        /// Registered project root path or alias whose session store should receive ingested messages
        #[arg(long, conflicts_with = "project_id")]
        project_path: Option<String>,
    },
    /// Search previously ingested session messages
    Search {
        /// Full-text query to search for
        query: String,
        /// Optional explicit result scope; omit or use all for unified cross-provider search
        #[arg(long)]
        provider: Option<String>,
        /// Maximum number of matches
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Registered project id whose session store should be searched
        #[arg(long)]
        project_id: Option<String>,
        /// Registered project root path or alias whose session store should be searched
        #[arg(long, conflicts_with = "project_id")]
        project_path: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum MemoryAction {
    /// Inspect holographic-memory health and derived-capacity signals.
    Status {
        /// Output as JSON instead of a human-readable report.
        #[arg(long)]
        json: bool,
        /// Project path (default: current directory, with discovery)
        #[arg(short, long)]
        path: Option<String>,
        /// Registered project id to inspect instead of discovering from cwd
        #[arg(long, conflicts_with = "path")]
        project_id: Option<String>,
        /// Registered project root path or alias to inspect instead of discovering from cwd
        #[arg(long, conflicts_with_all = ["path", "project_id"])]
        project_path: Option<String>,
    },
    /// Similarity-dedup curation (and the LLM-review plan/apply halves),
    /// suitable for a cron job — no dashboard server required.
    ///
    /// Default is a dry-run preview. The LLM tier never calls a model from
    /// this binary: `--llm` emits the review request (clusters + chat
    /// messages); run it through your own LLM and feed the strict-JSON ops
    /// back with `--llm-ops <file>` to validate and (with `--apply`) execute
    /// them.
    Curate {
        /// Apply the proposed deletions/ops instead of previewing them
        #[arg(long)]
        apply: bool,
        /// Include the LLM-review request (clusters + messages) in the report
        #[arg(long)]
        llm: bool,
        /// JSON file with externally produced LLM ops ({"ops": [...]}); "-" reads stdin
        #[arg(long, value_name = "FILE")]
        llm_ops: Option<String>,
        /// Maximum candidate clusters included in the LLM review request
        #[arg(long, default_value_t = tracedecay::dashboard::memory_curate::CURATION_DEFAULT_MAX_CLUSTERS)]
        max_clusters: usize,
        /// Confidence floor below which LLM ops are rejected
        #[arg(long, default_value_t = tracedecay::dashboard::memory_curate::CURATION_DEFAULT_MIN_CONFIDENCE)]
        min_confidence: f64,
        /// Project path (default: current directory, with discovery)
        #[arg(short, long)]
        path: Option<String>,
    },
}

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
        hermes_home: Option<String>,
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
        /// LCM provider to inspect.
        #[arg(long, default_value = "cursor")]
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

#[derive(Subcommand)]
pub enum MigrateAction {
    /// Build a readonly migration inventory or manifest plan
    Plan {
        /// Root directory to scan (repeatable). Defaults to the current directory.
        #[arg(long = "root")]
        roots: Vec<String>,
        /// Include all registered projects even when explicit roots are supplied.
        #[arg(long = "include-all-registered")]
        include_all_registered: bool,
        /// Follow symlinked directories while scanning.
        #[arg(long)]
        follow_symlinks: bool,
        /// Write a manifest plan to this path instead of only printing inventory.
        #[arg(long)]
        manifest: Option<String>,
        /// Save a manifest under the target profile's migration-inventory directory.
        #[arg(long)]
        save: bool,
        /// Target profile root for manifest-backed profile-shard planning.
        #[arg(long)]
        profile_root: Option<String>,
        /// Project id to use for manifest-backed profile-shard planning.
        #[arg(long)]
        project_id: Option<String>,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Export a profile-sharded project store to a standalone directory.
    Export {
        /// Export from the current profile-sharded store layout.
        #[arg(long = "from-profile", required = true)]
        from_profile: bool,
        /// Project path whose enrollment marker identifies the profile shard.
        #[arg(long, conflicts_with = "project_id")]
        project: Option<String>,
        /// Project id to export from the current profile root.
        #[arg(long = "project-id", conflicts_with = "project")]
        project_id: Option<String>,
        /// Destination directory for the exported store.
        #[arg(long)]
        to: String,
    },
    /// Apply a single-store manifest plan with staged profile-shard copy and cutover.
    Apply {
        /// Manifest path to apply.
        #[arg(long)]
        manifest: String,
        /// Confirmation token from `migrate plan`.
        #[arg(long = "confirm-token")]
        confirm_token: String,
    },
    /// Verify a manifest plan without mutating source stores.
    Verify {
        /// Manifest path to verify.
        #[arg(long)]
        manifest: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Reconstruct registry plans from profile-sharded store manifests without applying them.
    Reconstruct {
        /// Profile root containing projects/<project_id>/store_manifest.json files.
        #[arg(long = "profile-root")]
        profile_root: String,
        /// Apply registry reconstruction plans after scanning manifests.
        #[arg(long)]
        apply: bool,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Remove stale registry rows for projects whose canonical roots no longer exist.
    #[command(name = "registry-gc")]
    RegistryGc {
        /// Only consider registered projects whose canonical root starts with this prefix.
        #[arg(long)]
        prefix: Option<String>,
        /// Apply deletions. Omit for a dry-run preview.
        #[arg(long)]
        apply: bool,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Roll back a manifest plan when the rollback preconditions are supported.
    Rollback {
        /// Manifest path to roll back.
        #[arg(long)]
        manifest: String,
        /// Confirmation token from `migrate plan`.
        #[arg(long = "confirm-token")]
        confirm_token: String,
    },
    /// Remove old source artifacts after a verified manifest-backed migration.
    CleanupSources {
        /// Manifest path to clean up.
        #[arg(long)]
        manifest: String,
        /// Confirmation token from `migrate plan`.
        #[arg(long = "confirm-token")]
        confirm_token: String,
    },
}

#[derive(Subcommand)]
pub enum BranchAction {
    /// List tracked branches and their DB sizes
    List {
        /// Project path (default: current directory)
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Track a new branch (copies nearest ancestor DB + incremental sync)
    Add {
        /// Branch name to track (default: current branch)
        name: Option<String>,
        /// Project path (default: current directory)
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Remove a tracked branch and delete its DB
    Remove {
        /// Branch name to remove
        name: String,
        /// Project path (default: current directory)
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Remove all tracked branches (keeps only the default branch)
    Removeall {
        /// Project path (default: current directory)
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Remove DBs for branches that no longer exist in git
    Gc {
        /// Project path (default: current directory)
        #[arg(short, long)]
        path: Option<String>,
    },
}

#[cfg(test)]
mod cli_parse_tests {
    use super::{
        AutomationAction, AutomationConfigAction, AutomationConfigScope, AutomationRunAction,
        AutomationRunsAction, AutomationSkillsAction, AutomationSkillsInstallTarget, BranchAction,
        Cli, Commands, DaemonAction, MemoryAction, MigrateAction, SessionsAction,
    };
    use clap::{error::ErrorKind, CommandFactory, Parser};

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn tool_command_preserves_trailing_help_and_reserved_args() {
        let cli = Cli::try_parse_from([
            "tracedecay",
            "tool",
            "--project",
            "/tmp/project",
            "search",
            "--help",
            "--json",
            "--args",
            r#"{"query":"foo"}"#,
            "@payload.json",
        ])
        .expect("tool command should parse");

        assert!(matches!(
            cli.command,
            Some(Commands::Tool { project, name, args })
                if project.as_deref() == Some("/tmp/project")
                    && name.as_deref() == Some("search")
                    && args
                        == vec![
                            "--help".to_string(),
                            "--json".to_string(),
                            "--args".to_string(),
                            r#"{"query":"foo"}"#.to_string(),
                            "@payload.json".to_string(),
                        ]
        ));
    }

    #[test]
    fn claude_install_alias_dispatches_to_install_command() {
        let cli = Cli::try_parse_from([
            "tracedecay",
            "claude-install",
            "--agent",
            "hermes",
            "--profile",
            "dev",
            "--project-root",
            "/tmp/project",
            "--no-dashboard",
        ])
        .expect("install alias should parse");

        assert!(matches!(
            cli.command,
            Some(Commands::Install {
                agent,
                local,
                profile,
                all_profiles,
                project_root,
                no_dashboard,
                ..
            }) if agent.as_deref() == Some("hermes")
                && !local
                && profile.as_deref() == Some("dev")
                && !all_profiles
                && project_root.as_deref() == Some("/tmp/project")
                && no_dashboard
        ));
    }

    #[test]
    fn update_plugins_alias_dispatches_to_update_plugin_command() {
        let cli = Cli::try_parse_from(["tracedecay", "update-plugins"])
            .expect("update-plugin alias should parse");

        assert!(matches!(cli.command, Some(Commands::UpdatePlugin)));
    }

    #[test]
    fn update_upgrade_and_update_plugin_parse_to_distinct_commands() {
        let update = Cli::try_parse_from(["tracedecay", "update"]).expect("update should parse");
        let upgrade = Cli::try_parse_from(["tracedecay", "upgrade"]).expect("upgrade should parse");
        let update_plugin = Cli::try_parse_from(["tracedecay", "update-plugin"])
            .expect("update-plugin should parse");

        assert!(matches!(update.command, Some(Commands::Update)));
        assert!(matches!(upgrade.command, Some(Commands::Upgrade)));
        assert!(matches!(
            update_plugin.command,
            Some(Commands::UpdatePlugin)
        ));
    }

    #[test]
    fn update_help_describes_refresh_scope() {
        let help = Cli::command().render_long_help().to_string();

        assert!(help.contains("update"));
        assert!(help.contains("Refresh the tracedecay binary, generated plugins, and daemon"));
    }

    #[test]
    fn codex_install_automation_flag_parses_without_extra_knobs() {
        let cli =
            Cli::try_parse_from(["tracedecay", "install", "--agent", "codex", "--automation"])
                .expect("Codex automation install should parse");

        assert!(matches!(
            cli.command,
            Some(Commands::Install {
                agent,
                automation,
                ..
            }) if agent.as_deref() == Some("codex") && automation
        ));
    }

    #[test]
    fn daemon_install_service_command_parses_socket_and_no_start() {
        let cli = Cli::try_parse_from([
            "tracedecay",
            "daemon",
            "install-service",
            "--socket",
            "/tmp/tracedecay.sock",
            "--no-start",
        ])
        .expect("daemon install-service should parse");

        assert!(matches!(
            cli.command,
            Some(Commands::Daemon {
                action: DaemonAction::InstallService { socket, no_start }
            }) if socket.as_deref() == Some("/tmp/tracedecay.sock") && no_start
        ));
    }

    #[test]
    fn status_and_branch_add_commands_dispatch_to_expected_variants() {
        let status = Cli::try_parse_from([
            "tracedecay",
            "status",
            "/tmp/project",
            "--json",
            "--short",
            "--details",
            "--runtime",
        ])
        .expect("status command should parse");
        assert!(matches!(
            status.command,
            Some(Commands::Status {
                path,
                project_id,
                project_path,
                json,
                short,
                details,
                runtime,
            }) if path.as_deref() == Some("/tmp/project")
                && project_id.is_none()
                && project_path.is_none()
                && json
                && short
                && details
                && runtime
        ));

        let branch = Cli::try_parse_from([
            "tracedecay",
            "branch",
            "add",
            "feature/dispatch-tests",
            "--path",
            "/tmp/project",
        ])
        .expect("branch add should parse");
        assert!(matches!(
            branch.command,
            Some(Commands::Branch {
                action: BranchAction::Add { name, path }
            }) if name.as_deref() == Some("feature/dispatch-tests")
                && path.as_deref() == Some("/tmp/project")
        ));
    }

    #[test]
    fn init_and_sync_parse_runtime_skip_and_include_folders() {
        let init = Cli::try_parse_from([
            "tracedecay",
            "init",
            "/tmp/project",
            "--skip-folder",
            "vendor",
            "dist",
            "--include-folder",
            "dist/generated",
        ])
        .expect("init skip/include folders should parse");
        assert!(matches!(
            init.command,
            Some(Commands::Init {
                path,
                skip_folders,
                include_folders,
            }) if path.as_deref() == Some("/tmp/project")
                && skip_folders == strings(&["vendor", "dist"])
                && include_folders == strings(&["dist/generated"])
        ));

        let sync = Cli::try_parse_from([
            "tracedecay",
            "sync",
            "/tmp/project",
            "--force",
            "--include-folder",
            "dist",
            "vendor/generated",
        ])
        .expect("sync include folders should parse");
        assert!(matches!(
            sync.command,
            Some(Commands::Sync {
                path,
                force,
                skip_folders,
                include_folders,
                ..
            }) if path.as_deref() == Some("/tmp/project")
                && force
                && skip_folders.is_empty()
                && include_folders == strings(&["dist", "vendor/generated"])
        ));
    }

    #[test]
    fn init_and_sync_parse_repeated_include_folder_flags() {
        let init = Cli::try_parse_from([
            "tracedecay",
            "init",
            "/tmp/project",
            "--include-folder",
            "dist",
            "--include-folder",
            "vendor/generated",
        ])
        .expect("repeated init include folders should parse");
        assert!(matches!(
            init.command,
            Some(Commands::Init {
                path,
                include_folders,
                ..
            }) if path.as_deref() == Some("/tmp/project")
                && include_folders == strings(&["dist", "vendor/generated"])
        ));

        let sync = Cli::try_parse_from([
            "tracedecay",
            "sync",
            "/tmp/project",
            "--include-folder",
            "dist",
            "--include-folder",
            "vendor/generated",
        ])
        .expect("repeated sync include folders should parse");
        assert!(matches!(
            sync.command,
            Some(Commands::Sync {
                path,
                include_folders,
                ..
            }) if path.as_deref() == Some("/tmp/project")
                && include_folders == strings(&["dist", "vendor/generated"])
        ));
    }

    #[test]
    fn memory_status_command_dispatches_to_expected_variant() {
        let cli = Cli::try_parse_from([
            "tracedecay",
            "memory",
            "status",
            "--json",
            "--path",
            "/tmp/project",
        ])
        .expect("memory status command should parse");

        assert!(matches!(
            cli.command,
            Some(Commands::Memory {
                action: MemoryAction::Status {
                    json,
                    path,
                    project_id,
                    project_path,
                }
            }) if json
                && path.as_deref() == Some("/tmp/project")
                && project_id.is_none()
                && project_path.is_none()
        ));
    }

    #[test]
    fn automation_config_commands_parse_project_sidecar_flags() {
        let get = Cli::try_parse_from([
            "tracedecay",
            "automation",
            "config",
            "get",
            "--json",
            "--path",
            "/tmp/project",
        ])
        .expect("automation config get should parse");
        assert!(matches!(
            get.command,
            Some(Commands::Automation {
                action:
                    AutomationAction::Config {
                        action:
                            AutomationConfigAction::Get {
                                scope: AutomationConfigScope::Project,
                                json,
                                path
                            }
                    }
            }) if json && path.as_deref() == Some("/tmp/project")
        ));

        let explain = Cli::try_parse_from([
            "tracedecay",
            "automation",
            "config",
            "explain",
            "--json",
            "--scope",
            "global",
        ])
        .expect("automation config explain should parse");
        assert!(matches!(
            explain.command,
            Some(Commands::Automation {
                action:
                    AutomationAction::Config {
                        action:
                            AutomationConfigAction::Explain {
                                scope: AutomationConfigScope::Global,
                                json,
                                path
                            }
                    }
            }) if json && path.is_none()
        ));

        let enable = Cli::try_parse_from([
            "tracedecay",
            "automation",
            "config",
            "enable",
            "--scope",
            "global",
        ])
        .expect("automation config enable should parse");
        assert!(matches!(
            enable.command,
            Some(Commands::Automation {
                action:
                    AutomationAction::Config {
                        action:
                            AutomationConfigAction::Enable {
                                scope: AutomationConfigScope::Global,
                                path
                            }
                    }
            }) if path.is_none()
        ));

        let disable = Cli::try_parse_from(["tracedecay", "automation", "config", "disable"])
            .expect("automation config disable should parse");
        assert!(matches!(
            disable.command,
            Some(Commands::Automation {
                action:
                    AutomationAction::Config {
                        action:
                            AutomationConfigAction::Disable {
                                scope: AutomationConfigScope::Project,
                                path
                            }
                    }
            }) if path.is_none()
        ));

        let set = Cli::try_parse_from([
            "tracedecay",
            "automation",
            "config",
            "set",
            "--backend",
            "codex-app-server",
            "--host-mode",
            "delegated-host",
            "--model",
            "gpt-test",
            "--timeout-secs",
            "120",
            "--scheduler-tick-secs",
            "30",
            "--max-tokens",
            "4096",
            "--temperature",
            "0.2",
            "--require-dashboard-approval",
            "true",
            "--auto-apply-memory-ops",
            "false",
            "--auto-enable-skills",
            "false",
            "--memory-curator",
            "true",
            "--memory-curator-schedule",
            "manual",
            "--memory-curator-interval-secs",
            "900",
            "--memory-curator-cooldown-secs",
            "300",
            "--memory-curator-min-idle-secs",
            "120",
            "--memory-curator-stale-lock-secs",
            "3600",
            "--session-reflector",
            "true",
            "--session-reflector-schedule",
            "interval",
            "--session-reflector-interval-secs",
            "1800",
            "--session-reflector-cooldown-secs",
            "600",
            "--session-reflector-min-idle-secs",
            "60",
            "--session-reflector-stale-lock-secs",
            "7200",
            "--skill-writer",
            "true",
            "--skill-writer-schedule",
            "manual",
            "--skill-writer-interval-secs",
            "",
            "--skill-writer-cooldown-secs",
            "none",
        ])
        .expect("automation config set should parse");
        let Some(Commands::Automation {
            action:
                AutomationAction::Config {
                    action:
                        AutomationConfigAction::Set {
                            scope,
                            backend,
                            host_mode,
                            model,
                            timeout_secs,
                            scheduler_tick_secs,
                            max_tokens,
                            temperature,
                            require_dashboard_approval,
                            auto_apply_memory_ops,
                            auto_enable_skills,
                            memory_curator,
                            memory_curator_schedule,
                            memory_curator_interval_secs,
                            memory_curator_cooldown_secs,
                            memory_curator_min_idle_secs,
                            memory_curator_stale_lock_secs,
                            session_reflector,
                            session_reflector_schedule,
                            session_reflector_interval_secs,
                            session_reflector_cooldown_secs,
                            session_reflector_min_idle_secs,
                            session_reflector_stale_lock_secs,
                            skill_writer,
                            skill_writer_schedule,
                            skill_writer_interval_secs,
                            skill_writer_cooldown_secs,
                            skill_writer_min_idle_secs,
                            skill_writer_stale_lock_secs,
                            path,
                        },
                },
        }) = set.command
        else {
            panic!("automation config set should parse into Set action");
        };
        assert_eq!(scope, AutomationConfigScope::Project);
        assert_eq!(backend.as_deref(), Some("codex-app-server"));
        assert_eq!(host_mode.as_deref(), Some("delegated-host"));
        assert_eq!(model.as_deref(), Some("gpt-test"));
        assert_eq!(timeout_secs, Some(120));
        assert_eq!(scheduler_tick_secs, Some(30));
        assert_eq!(max_tokens.as_deref(), Some("4096"));
        assert_eq!(temperature.as_deref(), Some("0.2"));
        assert_eq!(require_dashboard_approval, Some(true));
        assert_eq!(auto_apply_memory_ops, Some(false));
        assert_eq!(auto_enable_skills, Some(false));
        assert_eq!(memory_curator, Some(true));
        assert_eq!(memory_curator_schedule.as_deref(), Some("manual"));
        assert_eq!(memory_curator_interval_secs.as_deref(), Some("900"));
        assert_eq!(memory_curator_cooldown_secs.as_deref(), Some("300"));
        assert_eq!(memory_curator_min_idle_secs.as_deref(), Some("120"));
        assert_eq!(memory_curator_stale_lock_secs.as_deref(), Some("3600"));
        assert_eq!(session_reflector, Some(true));
        assert_eq!(session_reflector_schedule.as_deref(), Some("interval"));
        assert_eq!(session_reflector_interval_secs.as_deref(), Some("1800"));
        assert_eq!(session_reflector_cooldown_secs.as_deref(), Some("600"));
        assert_eq!(session_reflector_min_idle_secs.as_deref(), Some("60"));
        assert_eq!(session_reflector_stale_lock_secs.as_deref(), Some("7200"));
        assert_eq!(skill_writer, Some(true));
        assert_eq!(skill_writer_schedule.as_deref(), Some("manual"));
        assert_eq!(skill_writer_interval_secs.as_deref(), Some(""));
        assert_eq!(skill_writer_cooldown_secs.as_deref(), Some("none"));
        assert!(skill_writer_min_idle_secs.is_none());
        assert!(skill_writer_stale_lock_secs.is_none());
        assert!(path.is_none());
    }

    #[test]
    fn automation_run_memory_curation_parses_manual_dry_run_flags() {
        let cli = Cli::try_parse_from([
            "tracedecay",
            "automation",
            "run",
            "memory-curation",
            "--dry-run",
            "true",
            "--max-clusters",
            "8",
            "--min-confidence",
            "0.7",
            "--path",
            "/tmp/project",
        ])
        .expect("automation memory-curation run should parse");

        assert!(matches!(
            cli.command,
            Some(Commands::Automation {
                action:
                    AutomationAction::Run {
                        action:
                            AutomationRunAction::MemoryCuration {
                                dry_run,
                                max_clusters,
                                min_confidence,
                                path,
                            }
                    }
            }) if dry_run
                && max_clusters == 8
                && (min_confidence - 0.7).abs() < f64::EPSILON
                && path.as_deref() == Some("/tmp/project")
        ));
    }

    #[test]
    fn automation_run_session_reflection_parses_manual_dry_run_flags() {
        let cli = Cli::try_parse_from([
            "tracedecay",
            "automation",
            "run",
            "session-reflection",
            "--dry-run",
            "true",
            "--provider",
            "codex",
            "--query",
            "remember decisions",
            "--evidence-limit",
            "12",
            "--storage-scope",
            "hermes_profile",
            "--hermes-home",
            "/tmp/hermes-profile",
            "--scope",
            "session",
            "--session-id",
            "session-123",
            "--include-summaries",
            "false",
            "--sort",
            "hybrid",
            "--source",
            "hermes",
            "--role",
            "assistant",
            "--start-time",
            "1715100000",
            "--end-time",
            "1715100100",
            "--path",
            "/tmp/project",
        ])
        .expect("automation session-reflection run should parse");

        assert!(matches!(
            cli.command,
            Some(Commands::Automation {
                action:
                    AutomationAction::Run {
                        action:
                            AutomationRunAction::SessionReflection {
                                dry_run,
                                provider,
                                query,
                                evidence_limit,
                                storage_scope,
                                hermes_home,
                                scope,
                                session_id,
                                include_summaries,
                                sort,
                                source,
                                role,
                                start_time,
                                end_time,
                                path,
                            }
                    }
            }) if dry_run
                && provider == "codex"
                && query == "remember decisions"
                && evidence_limit == 12
                && storage_scope == "hermes_profile"
                && hermes_home.as_deref() == Some("/tmp/hermes-profile")
                && scope == "session"
                && session_id.as_deref() == Some("session-123")
                && !include_summaries
                && sort == "hybrid"
                && source.as_deref() == Some("hermes")
                && role.as_deref() == Some("assistant")
                && start_time == Some(1_715_100_000)
                && end_time == Some(1_715_100_100)
                && path.as_deref() == Some("/tmp/project")
        ));
    }

    #[test]
    fn automation_run_skill_writing_parses_manual_dry_run_flags() {
        let cli = Cli::try_parse_from([
            "tracedecay",
            "automation",
            "run",
            "skill-writing",
            "--dry-run",
            "true",
            "--provider",
            "cursor",
            "--query",
            "workflow corrections",
            "--evidence-limit",
            "9",
            "--path",
            "/tmp/project",
        ])
        .expect("automation skill-writing run should parse");

        assert!(matches!(
            cli.command,
            Some(Commands::Automation {
                action:
                    AutomationAction::Run {
                        action:
                            AutomationRunAction::SkillWriting {
                                dry_run,
                                provider,
                                query,
                                evidence_limit,
                                path,
                            }
                    }
            }) if dry_run
                && provider == "cursor"
                && query == "workflow corrections"
                && evidence_limit == 9
                && path.as_deref() == Some("/tmp/project")
        ));
    }

    #[test]
    fn automation_runs_commands_parse_history_flags() {
        let list = Cli::try_parse_from([
            "tracedecay",
            "automation",
            "runs",
            "list",
            "--limit",
            "5",
            "--json",
            "--path",
            "/tmp/project",
        ])
        .expect("automation runs list should parse");

        assert!(matches!(
            list.command,
            Some(Commands::Automation {
                action:
                    AutomationAction::Runs {
                        action:
                            AutomationRunsAction::List {
                                limit,
                                json,
                                path,
                            }
                    }
            }) if limit == 5 && json && path.as_deref() == Some("/tmp/project")
        ));

        let view = Cli::try_parse_from([
            "tracedecay",
            "automation",
            "runs",
            "view",
            "run-123",
            "--json",
            "--path",
            "/tmp/project",
        ])
        .expect("automation runs view should parse");

        assert!(matches!(
            view.command,
            Some(Commands::Automation {
                action:
                    AutomationAction::Runs {
                        action:
                            AutomationRunsAction::View { run_id, json, path }
                    }
            }) if run_id == "run-123" && json && path.as_deref() == Some("/tmp/project")
        ));

        let artifact = Cli::try_parse_from([
            "tracedecay",
            "automation",
            "runs",
            "artifact",
            "run-123",
            "codex_handoff",
            "--json",
            "--path",
            "/tmp/project",
        ])
        .expect("automation runs artifact should parse");

        assert!(matches!(
            artifact.command,
            Some(Commands::Automation {
                action:
                    AutomationAction::Runs {
                        action:
                            AutomationRunsAction::Artifact {
                                run_id,
                                kind,
                                json,
                                path
                            }
                    }
            }) if run_id == "run-123"
                && kind == "codex_handoff"
                && json
                && path.as_deref() == Some("/tmp/project")
        ));
    }

    #[test]
    fn automation_skills_commands_parse_lifecycle_flags() {
        let draft = Cli::try_parse_from([
            "tracedecay",
            "automation",
            "skills",
            "draft",
            "--id",
            "repo-hygiene",
            "--title",
            "Repository hygiene",
            "--summary",
            "Keep checks focused",
            "--category",
            "maintenance",
            "--body",
            "Run focused tests.",
            "--pinned",
        ])
        .expect("automation skills draft should parse");
        assert!(matches!(
            draft.command,
            Some(Commands::Automation {
                action:
                    AutomationAction::Skills {
                        action:
                            AutomationSkillsAction::Draft {
                                id,
                                title,
                                summary,
                                category,
                                body,
                                pinned,
                            }
                    }
            }) if id == "repo-hygiene"
                && title == "Repository hygiene"
                && summary == "Keep checks focused"
                && category == "maintenance"
                && body == "Run focused tests."
                && pinned
        ));

        let update = Cli::try_parse_from([
            "tracedecay",
            "automation",
            "skills",
            "update",
            "repo-hygiene",
            "--summary",
            "Updated",
            "--pinned",
            "false",
        ])
        .expect("automation skills update should parse");
        assert!(matches!(
            update.command,
            Some(Commands::Automation {
                action:
                    AutomationAction::Skills {
                        action:
                            AutomationSkillsAction::Update {
                                id,
                                summary,
                                pinned,
                                ..
                            }
                    }
            }) if id == "repo-hygiene"
                && summary.as_deref() == Some("Updated")
                && pinned == Some(false)
        ));

        let approve = Cli::try_parse_from([
            "tracedecay",
            "automation",
            "skills",
            "approve",
            "repo-hygiene",
        ])
        .expect("automation skills approve should parse");
        assert!(matches!(
            approve.command,
            Some(Commands::Automation {
                action:
                    AutomationAction::Skills {
                        action: AutomationSkillsAction::Approve { id }
                    }
            }) if id == "repo-hygiene"
        ));

        let install = Cli::try_parse_from([
            "tracedecay",
            "automation",
            "skills",
            "install",
            "--target",
            "cursor",
            "--output",
            "/tmp/plugin",
            "--json",
        ])
        .expect("automation skills install should parse");
        assert!(matches!(
            install.command,
            Some(Commands::Automation {
                action:
                    AutomationAction::Skills {
                        action:
                            AutomationSkillsAction::Install {
                                target,
                                output,
                                plugin_artifact,
                                json,
                            }
                    }
            }) if target == AutomationSkillsInstallTarget::Cursor
                && output == "/tmp/plugin"
                && !plugin_artifact
                && json
        ));

        let opencode_install = Cli::try_parse_from([
            "tracedecay",
            "automation",
            "skills",
            "install",
            "--target",
            "opencode",
            "--output",
            "/tmp/AGENTS.md",
        ])
        .expect("automation skills install should accept opencode alias");
        assert!(matches!(
            opencode_install.command,
            Some(Commands::Automation {
                action:
                    AutomationAction::Skills {
                        action:
                            AutomationSkillsAction::Install {
                                target,
                                output,
                                plugin_artifact,
                                json,
                            }
                    }
            }) if target == AutomationSkillsInstallTarget::OpenCode
                && output == "/tmp/AGENTS.md"
                && !plugin_artifact
                && !json
        ));

        let codex_artifact = Cli::try_parse_from([
            "tracedecay",
            "automation",
            "skills",
            "install",
            "--target",
            "codex",
            "--output",
            "/tmp/codex-plugin",
            "--plugin-artifact",
        ])
        .expect("automation skills install codex artifact should parse");
        assert!(matches!(
            codex_artifact.command,
            Some(Commands::Automation {
                action:
                    AutomationAction::Skills {
                        action:
                            AutomationSkillsAction::Install {
                                target,
                                output,
                                plugin_artifact,
                                json,
                            }
                    }
            }) if target == AutomationSkillsInstallTarget::Codex
                && output == "/tmp/codex-plugin"
                && plugin_artifact
                && !json
        ));
    }

    #[test]
    fn project_selector_flags_parse_for_cli_read_surfaces() {
        let status =
            Cli::try_parse_from(["tracedecay", "status", "--project-id", "proj_123", "--json"])
                .expect("status project selector should parse");
        assert!(matches!(
            status.command,
            Some(Commands::Status {
                path,
                project_id,
                project_path,
                json,
                ..
            }) if path.is_none()
                && project_id.as_deref() == Some("proj_123")
                && project_path.is_none()
                && json
        ));

        let memory = Cli::try_parse_from([
            "tracedecay",
            "memory",
            "status",
            "--project-path",
            "/tmp/project",
        ])
        .expect("memory status project selector should parse");
        assert!(matches!(
            memory.command,
            Some(Commands::Memory {
                action:
                    MemoryAction::Status {
                        path,
                        project_id,
                        project_path,
                        ..
                    }
            }) if path.is_none()
                && project_id.is_none()
                && project_path.as_deref() == Some("/tmp/project")
        ));

        let sessions = Cli::try_parse_from([
            "tracedecay",
            "sessions",
            "search",
            "needle",
            "--project-id",
            "proj_123",
        ])
        .expect("sessions search project selector should parse");
        assert!(matches!(
            sessions.command,
            Some(Commands::Sessions {
                action:
                    SessionsAction::Search {
                        project_id,
                        project_path,
                        ..
                    }
            }) if project_id.as_deref() == Some("proj_123") && project_path.is_none()
        ));
    }

    #[test]
    fn migrate_commands_parse_manifest_scaffolding_flags() {
        let plan = Cli::try_parse_from([
            "tracedecay",
            "migrate",
            "plan",
            "--root",
            "/tmp/project",
            "--manifest",
            "/tmp/manifest.json",
            "--profile-root",
            "/tmp/profile",
            "--project-id",
            "proj_123",
            "--json",
        ])
        .expect("migrate plan should parse");
        assert!(matches!(
            plan.command,
            Some(Commands::Migrate {
                action:
                    MigrateAction::Plan {
                        roots,
                        manifest,
                        profile_root,
                        project_id,
                        json,
                        ..
                    }
            }) if roots == vec!["/tmp/project".to_string()]
                && manifest.as_deref() == Some("/tmp/manifest.json")
                && profile_root.as_deref() == Some("/tmp/profile")
                && project_id.as_deref() == Some("proj_123")
                && json
        ));

        let apply = Cli::try_parse_from([
            "tracedecay",
            "migrate",
            "apply",
            "--manifest",
            "/tmp/manifest.json",
            "--confirm-token",
            "confirm-mig_123",
        ])
        .expect("migrate apply should parse");
        assert!(matches!(
            apply.command,
            Some(Commands::Migrate {
                action:
                    MigrateAction::Apply {
                        manifest,
                        confirm_token,
                    }
            }) if manifest == "/tmp/manifest.json" && confirm_token == "confirm-mig_123"
        ));

        let verify = Cli::try_parse_from([
            "tracedecay",
            "migrate",
            "verify",
            "--manifest",
            "/tmp/manifest.json",
            "--json",
        ])
        .expect("migrate verify should parse");
        assert!(matches!(
            verify.command,
            Some(Commands::Migrate {
                action: MigrateAction::Verify { manifest, json }
            }) if manifest == "/tmp/manifest.json" && json
        ));
    }

    #[test]
    fn install_conflicting_profile_flags_fail_during_parse() {
        let err = match Cli::try_parse_from([
            "tracedecay",
            "install",
            "--agent",
            "hermes",
            "--profile",
            "dev",
            "--all-profiles",
        ]) {
            Ok(_) => panic!("conflicting profile flags should fail"),
            Err(err) => err,
        };

        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn migrate_reconstruct_apply_flag_parses() {
        let cli = Cli::try_parse_from([
            "tracedecay",
            "migrate",
            "reconstruct",
            "--profile-root",
            "/tmp/profile",
            "--apply",
            "--json",
        ])
        .expect("migrate reconstruct should parse");

        assert!(matches!(
            cli.command,
            Some(Commands::Migrate {
                action:
                    MigrateAction::Reconstruct {
                        profile_root,
                        apply,
                        json,
                    }
            }) if profile_root == "/tmp/profile" && apply && json
        ));
    }

    #[test]
    fn migrate_export_requires_from_profile_flag() {
        let err = match Cli::try_parse_from([
            "tracedecay",
            "migrate",
            "export",
            "--project-id",
            "proj_123",
            "--to",
            "/tmp/exported",
        ]) {
            Ok(_) => panic!("migrate export should require --from-profile"),
            Err(err) => err,
        };

        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn migrate_registry_gc_parses() {
        let cli = Cli::try_parse_from([
            "tracedecay",
            "migrate",
            "registry-gc",
            "--prefix",
            "/tmp",
            "--apply",
            "--json",
        ])
        .expect("migrate registry-gc should parse");

        assert!(matches!(
            cli.command,
            Some(Commands::Migrate {
                action:
                    MigrateAction::RegistryGc {
                        prefix,
                        apply,
                        json,
                    }
            }) if prefix.as_deref() == Some("/tmp") && apply && json
        ));
    }

    #[test]
    fn branch_remove_requires_a_branch_name() {
        let err = match Cli::try_parse_from(["tracedecay", "branch", "remove"]) {
            Ok(_) => panic!("branch remove should require a name"),
            Err(err) => err,
        };

        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn parses_sessions_ingest_and_search_commands() {
        let ingest =
            Cli::try_parse_from(["tracedecay", "sessions", "ingest", "--provider", "cursor"])
                .unwrap();
        match ingest.command {
            Some(Commands::Sessions {
                action:
                    SessionsAction::Ingest {
                        provider,
                        project_id,
                        project_path,
                    },
            }) => {
                assert_eq!(provider.as_deref(), Some("cursor"));
                assert!(project_id.is_none());
                assert!(project_path.is_none());
            }
            _ => panic!("expected sessions ingest command"),
        }

        let search = Cli::try_parse_from([
            "tracedecay",
            "sessions",
            "search",
            "needle",
            "--provider",
            "codex",
            "--limit",
            "5",
        ])
        .unwrap();
        match search.command {
            Some(Commands::Sessions {
                action:
                    SessionsAction::Search {
                        query,
                        provider,
                        limit,
                        project_id,
                        project_path,
                    },
            }) => {
                assert_eq!(query, "needle");
                assert_eq!(provider.as_deref(), Some("codex"));
                assert_eq!(limit, 5);
                assert!(project_id.is_none());
                assert!(project_path.is_none());
            }
            _ => panic!("expected sessions search command"),
        }

        let all_provider_search =
            Cli::try_parse_from(["tracedecay", "sessions", "search", "needle"]).unwrap();
        match all_provider_search.command {
            Some(Commands::Sessions {
                action:
                    SessionsAction::Search {
                        query,
                        provider,
                        limit,
                        ..
                    },
            }) => {
                assert_eq!(query, "needle");
                assert!(provider.is_none());
                assert_eq!(limit, 10);
            }
            _ => panic!("expected sessions search command"),
        }
    }
}

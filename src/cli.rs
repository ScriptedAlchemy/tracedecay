use clap::{builder::PossibleValuesParser, Parser, Subcommand};

mod automation;
pub use automation::{
    AutomationAction, AutomationConfigAction, AutomationConfigScope, AutomationFactsAction,
    AutomationRunAction, AutomationRunsAction, AutomationSkillsAction,
    AutomationSkillsInstallTarget,
};

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
    /// Inspect language-server support for dashboard code diagnostics
    Lsp {
        #[command(subcommand)]
        action: LspAction,
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
        /// Enable the TraceDecay daemon automation loop (memory curator,
        /// session reflector, skill writer) for the current project
        /// (only used with --agent codex).
        #[arg(long)]
        automation: bool,
        /// With --automation: opt in to applying accepted memory-curation ops
        /// (permanent deletes/merges) without dashboard approval.
        #[arg(long, requires = "automation")]
        auto_apply: bool,
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
    /// Inspect registered TraceDecay projects from the global registry
    Projects {
        #[command(subcommand)]
        action: ProjectsAction,
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
pub enum ProjectsAction {
    /// List registered projects
    List {
        /// Maximum projects to show
        #[arg(long, default_value_t = 25)]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Search registered projects by id, path, alias, remote, or branch
    Search {
        /// Query text
        query: String,
        /// Maximum projects to show
        #[arg(long, default_value_t = 25)]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show registry context for one project id or path
    Context {
        /// Project id, root path, or registered alias
        selector: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum LspAction {
    /// List supported language servers, availability, and install hints
    Servers {
        /// Output as JSON
        #[arg(long)]
        json: bool,
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
mod parse_tests;

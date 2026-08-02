use clap::{Args, Parser, Subcommand, ValueEnum, builder::PossibleValuesParser};

mod automation;
pub mod dispatch;
mod help;
pub(crate) mod output;
pub use automation::{
    AutomationAction, AutomationConfigAction, AutomationConfigScope, AutomationFactsAction,
    AutomationRunAction, AutomationRunsAction, AutomationSkillsAction,
    AutomationSkillsInstallTarget,
};
use help::*;

fn agent_value_parser() -> PossibleValuesParser {
    PossibleValuesParser::new(tracedecay::agents::available_integrations())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum HostBundleComponentArg {
    Core,
    Agent,
    ContextMcp,
    OperatorMcp,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HostBundleCliOptions {
    pub component: Option<HostBundleComponentArg>,
    pub dry_run: bool,
    pub yes: bool,
    /// Operator confirmation for claiming a path no receipt records. Distinct
    /// from `yes`, which only confirms the plan the preview already showed.
    pub adopt: bool,
}

#[derive(Clone, Debug, Subcommand)]
pub enum FeedbackRollbackAction {
    /// Preview switching the installed Core feedback route to this binary's compiled route
    DryRun {
        #[arg(long, value_parser = agent_value_parser())]
        agent: String,
    },
    /// Apply the compiled Core feedback route and persist a restart-safe state file
    Apply {
        #[arg(long, value_parser = agent_value_parser())]
        agent: String,
        /// Durable rollback state file
        #[arg(long)]
        state: String,
        /// Confirm the feedback-route mutation
        #[arg(long)]
        yes: bool,
    },
    /// Restore the previous Core feedback route from a durable state file
    Restore {
        /// Durable rollback state file created by apply
        #[arg(long)]
        state: String,
        /// Confirm the feedback-route restoration
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Clone, Debug, Subcommand)]
pub enum HostBundleAction {
    /// List every host whose component-set lifecycle journal is awaiting recovery
    Status,
    /// Roll an interrupted host component transaction back to its pre-transaction state
    ///
    /// Recovery converges automatically when a second writer left the deployed
    /// bytes equal to the pre-transaction backup or to the transaction's own
    /// cataloged output. Genuinely foreign bytes stay fail-closed; pass
    /// `--quarantine` to set the journal aside (backups are preserved) and
    /// unblock the host.
    Recover {
        /// Recover only this agent's host journal (default: every pending host)
        #[arg(long, value_parser = agent_value_parser())]
        agent: Option<String>,
        /// Set aside a journal that convergent recovery cannot resolve
        #[arg(long)]
        quarantine: bool,
    },
    /// Snapshot one installed component's managed artifact files
    ArtifactBackup {
        /// Agent whose selected component owns the managed artifacts
        #[arg(long, value_parser = agent_value_parser())]
        agent: String,
    },
    /// Restore managed artifact files without changing host registration
    ArtifactRestore {
        /// Agent whose selected component owns the managed artifacts
        #[arg(long, value_parser = agent_value_parser())]
        agent: String,
        /// Lowercase 32-character hexadecimal artifact-backup receipt id
        #[arg(long)]
        backup_id: String,
    },
}

/// Code intelligence for Rust codebases.
#[derive(Parser)]
#[command(
    name = "tracedecay",
    about = "Code intelligence for 34 languages — semantic graph queries instead of file reads",
    after_help = TOP_LEVEL_AFTER_HELP,
    version = tracedecay::version::build_version()
)]
pub struct Cli {
    /// Select one compiled first-party host component; without it, lifecycle commands apply
    /// the host's canonical component set atomically
    #[arg(long, global = true, value_enum)]
    pub component: Option<HostBundleComponentArg>,
    /// Verify and print the exact signed lifecycle plan without mutating.
    /// Valid only alongside the agent-lifecycle commands; dispatch enforces the
    /// `--component` pairing so this global flag never demands `--component`
    /// from unrelated subcommands (e.g. `branch gc`, `migrate registry-gc`).
    #[arg(long, global = true, conflicts_with = "yes")]
    pub dry_run: bool,
    /// Confirm a first-party component mutation, or a `wipe`. Scope is enforced
    /// in dispatch, not by a global clap `requires`, so it does not leak onto
    /// other commands.
    #[arg(long, global = true)]
    pub yes: bool,
    /// Additionally confirm taking ownership of an existing file that no
    /// TraceDecay receipt records. Required alongside `--yes` for
    /// `reinstall --component`; the previous bytes are always backed up first,
    /// and a file another owner claims is refused regardless of this flag.
    #[arg(long, global = true)]
    pub adopt: bool,
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum PostUpdateMode {
    #[default]
    Normal,
    DogfoodForwardOnly,
    DogfoodRecoverInactive,
}

#[derive(Subcommand)]
pub enum PackageHookAction {
    /// Run Scoop package lifecycle integration.
    Scoop {
        #[command(subcommand)]
        action: ScoopPackageHookAction,
    },
}

#[derive(Subcommand)]
pub enum ScoopPackageHookAction {
    /// Snapshot and quiesce a managed service before Scoop replaces the app tree.
    Prepare {
        #[arg(long, value_parser = ["tracedecay", "tracedecay-beta"])]
        package_id: String,
        #[arg(long)]
        state_file: std::path::PathBuf,
    },
    /// Restore a snapshotted managed service after Scoop installs the new binary.
    Restore {
        #[arg(long, value_parser = ["tracedecay", "tracedecay-beta"])]
        package_id: String,
        #[arg(long)]
        state_file: std::path::PathBuf,
    },
}

#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub enum Commands {
    /// Initialize a new TraceDecay project (full index)
    #[command(long_about = INIT_LONG_ABOUT, after_help = INIT_AFTER_HELP)]
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
    #[command(long_about = SYNC_LONG_ABOUT, after_help = SYNC_AFTER_HELP)]
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
    #[command(long_about = STATUS_LONG_ABOUT, after_help = STATUS_AFTER_HELP)]
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
    //
    // `disable_help_flag = true` lets `-h`/`--help` flow through to our parser
    // so we can print the per-tool schema instead of clap's generic help.
    // `main::render_dynamic_command_help` still renders this long help for a
    // bare `tracedecay tool --help`.
    #[command(
        disable_help_flag = true,
        long_about = TOOL_LONG_ABOUT,
        after_help = TOOL_AFTER_HELP
    )]
    Tool {
        /// Project root to open before dispatching the tool. Defaults to the
        /// nearest initialised project walking up from cwd.
        #[arg(long)]
        project: Option<String>,
        /// MCP tool name (with or without the `tracedecay_` prefix). Omit to list all tools.
        name: Option<String>,
        /// Tool arguments: the tool's MCP arguments object via
        /// `--args <json|-|@file|file>` (`-` reads stdin), or `--key value`
        /// flags for quick scalar calls. Reserved flags: `--json`, `--dry-run`,
        /// `--project <path>`, `-h`/`--help`. Any per-key value starting with
        /// `@` is read from that file (handy for multi-line replacement bodies).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Inspect language-server support for dashboard code diagnostics
    #[command(long_about = LSP_LONG_ABOUT, after_help = LSP_AFTER_HELP)]
    Lsp {
        #[command(subcommand)]
        action: LspAction,
    },
    /// Configure agent integration (MCP server, permissions, hooks, prompt rules)
    #[command(
        name = "install",
        visible_alias = "claude-install",
        long_about = INSTALL_LONG_ABOUT,
        after_help = INSTALL_AFTER_HELP
    )]
    Install {
        /// Agent to configure (auto-detects if omitted)
        #[arg(long, value_parser = agent_value_parser())]
        agent: Option<String>,
        /// Write project-local configuration in the current directory
        #[arg(long)]
        local: bool,
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
    #[command(long_about = REINSTALL_LONG_ABOUT, after_help = REINSTALL_AFTER_HELP)]
    Reinstall {
        /// Reconcile one project-local integration in the current directory
        #[arg(long, requires = "agent")]
        local: bool,
        /// Project-local host to reconcile (required with --local)
        #[arg(long, value_parser = agent_value_parser(), requires = "local")]
        agent: Option<String>,
    },
    /// Refresh generated plugin code/assets for detected installs without
    /// touching agent config files.
    ///
    /// Rewrites only tracedecay-generated artifacts — the Hermes plugin
    /// (.py files, schemas.json, dashboard page) for the user integration,
    /// the Cursor plugin bundle, the Codex plugin bundle/cache, and the Kiro
    /// managed agent — re-baking the current binary path and version. Config
    /// files (Hermes config.yaml, mcp.json, settings,
    /// prompt rules) are left byte-for-byte intact; use `tracedecay reinstall`
    /// to refresh those.
    #[command(
        name = "update-plugin",
        visible_alias = "update-plugins",
        after_help = UPDATE_PLUGIN_AFTER_HELP
    )]
    UpdatePlugin {
        /// Update one project-local integration in the current directory
        #[arg(long, requires = "agent")]
        local: bool,
        /// Project-local host to update (required with --local)
        #[arg(long, value_parser = agent_value_parser(), requires = "local")]
        agent: Option<String>,
    },
    /// Remove agent integration (MCP server, permissions, hooks, prompt rules)
    #[command(
        name = "uninstall",
        visible_alias = "claude-uninstall",
        long_about = UNINSTALL_LONG_ABOUT,
        after_help = UNINSTALL_AFTER_HELP
    )]
    Uninstall {
        /// Agent to remove (removes all if omitted)
        #[arg(long, value_parser = agent_value_parser())]
        agent: Option<String>,
        /// Remove the selected project-local integration from the current directory
        #[arg(long, requires = "agent")]
        local: bool,
    },
    /// Dry-run, apply, or restore the direct host feedback-route rollback switch
    #[command(
        name = "feedback-rollback",
        long_about = FEEDBACK_ROLLBACK_LONG_ABOUT,
        after_help = FEEDBACK_ROLLBACK_AFTER_HELP
    )]
    FeedbackRollback {
        #[command(subcommand)]
        action: FeedbackRollbackAction,
    },
    /// Inspect or recover an interrupted first-party host component transaction
    #[command(
        name = "host-bundle",
        long_about = HOST_BUNDLE_LONG_ABOUT,
        after_help = HOST_BUNDLE_AFTER_HELP
    )]
    HostBundle {
        #[command(subcommand)]
        action: HostBundleAction,
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
    /// Claude Code SessionStart hook handler (called by Claude Code, not by users directly)
    #[command(name = "hook-claude-session-start", hide = true)]
    HookClaudeSessionStart,
    /// Claude Code PostToolUse hook handler for incremental sync (called by Claude Code)
    #[command(name = "hook-claude-post-tool-use", hide = true)]
    HookClaudePostToolUse,
    /// Claude Code SubagentStart hook handler (called by Claude Code, not by users directly)
    #[command(name = "hook-claude-subagent-start", hide = true)]
    HookClaudeSubagentStart,
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
    /// Codex Stop hook handler for final-turn user-session ingestion (called by Codex)
    #[command(name = "hook-codex-stop", hide = true)]
    HookCodexStop,
    /// Hermes terminal receipt handler (called by the TraceDecay plugin)
    #[command(name = "hook-hermes-terminal-receipt", hide = true)]
    HookHermesTerminalReceipt,
    /// Kimi Code native Hook V2 event handler.
    #[command(name = "hook-kimi-event", hide = true)]
    HookKimiEvent,
    /// OpenCode event-bus Hook V2 handler.
    #[command(name = "hook-opencode-event", hide = true)]
    HookOpenCodeEvent,
    /// OpenCode direct tool.execute.after Hook V2 handler.
    #[command(name = "hook-opencode-tool-after", hide = true)]
    HookOpenCodeToolAfter,
    /// Detached profile user-session automation review.
    #[command(name = "hook-user-session-review", hide = true)]
    HookUserSessionReview,
    /// Serve the local dashboard UI (holographic memory + LCM + code graph explorers)
    #[command(long_about = DASHBOARD_LONG_ABOUT, after_help = DASHBOARD_AFTER_HELP)]
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
    #[command(long_about = SERVE_LONG_ABOUT, after_help = SERVE_AFTER_HELP)]
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
    #[command(long_about = DAEMON_LONG_ABOUT, after_help = DAEMON_AFTER_HELP)]
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Install the latest version; refreshes plugins only after a real install
    ///
    /// Downloads and installs the newest release. When a new binary was
    /// installed, also refreshes generated plugins and the daemon service and
    /// runs the post-update health pass on the new version. When already up
    /// to date it stops there — use `tracedecay update` to refresh regardless.
    #[command(after_help = UPGRADE_AFTER_HELP)]
    Upgrade {
        /// Skip the post-update health pass (safe repairs + doctor summary)
        #[arg(long)]
        no_heal: bool,
        /// Skip refreshing already-configured agent integrations
        #[arg(long)]
        no_reinstall: bool,
    },
    /// Refresh generated plugins and the daemon, even when already up to date
    ///
    /// Upgrades the binary first when a newer release exists, then always
    /// refreshes generated plugins and the daemon service and runs the
    /// post-update health pass — even when the binary was already current.
    #[command(after_help = UPDATE_AFTER_HELP)]
    Update {
        /// Skip the post-update health pass (safe repairs + doctor summary)
        #[arg(long)]
        no_heal: bool,
        /// Skip refreshing already-configured agent integrations
        #[arg(long)]
        no_reinstall: bool,
    },
    /// Install this source-built executable into the live user environment.
    #[command(long_about = DOGFOOD_LONG_ABOUT, after_help = DOGFOOD_AFTER_HELP)]
    Dogfood,
    /// Refresh plugins and daemon after the binary has been updated.
    #[command(name = "post-update", hide = true)]
    PostUpdate {
        /// Skip the post-update health pass (safe repairs + doctor summary)
        #[arg(long)]
        no_heal: bool,
        /// Skip refreshing already-configured agent integrations
        #[arg(long)]
        no_reinstall: bool,
        /// Lifecycle lease token passed only by the parent updater.
        #[arg(long, hide = true)]
        lifecycle_lease_token: Option<String>,
        /// Fail when any tracked integration cannot be refreshed and verify
        /// that the managed daemon returns to its exact pre-update state.
        #[arg(long, hide = true)]
        strict: bool,
        /// Select migration-boundary recovery semantics. Dogfood uses the
        /// forward-only mode after installing the new binary.
        #[arg(long, value_enum, default_value_t, hide = true)]
        mode: PostUpdateMode,
    },
    /// Internal package-manager lifecycle integration.
    #[command(name = "package-hook", hide = true)]
    PackageHook {
        #[command(subcommand)]
        action: PackageHookAction,
    },
    /// Show or switch the update channel (stable or beta)
    #[command(long_about = CHANNEL_LONG_ABOUT, after_help = CHANNEL_AFTER_HELP)]
    Channel {
        /// Target channel: "stable" or "beta" (omit to show current)
        channel: Option<String>,
    },
    /// Show the resettable project-local token counter
    #[command(
        name = "current-counter",
        long_about = CURRENT_COUNTER_LONG_ABOUT,
        after_help = CURRENT_COUNTER_AFTER_HELP
    )]
    CurrentCounter {
        /// Project path (default: current directory)
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Reset the project-local token counter to zero
    #[command(
        name = "reset-counter",
        long_about = RESET_COUNTER_LONG_ABOUT,
        after_help = RESET_COUNTER_AFTER_HELP
    )]
    ResetCounter {
        /// Project path (default: current directory)
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Disable uploading token counts to the worldwide counter
    #[command(
        name = "disable-upload-counter",
        long_about = DISABLE_UPLOAD_COUNTER_LONG_ABOUT,
        after_help = DISABLE_UPLOAD_COUNTER_AFTER_HELP
    )]
    DisableUploadCounter,
    /// Enable uploading token counts to the worldwide counter
    #[command(
        name = "enable-upload-counter",
        long_about = ENABLE_UPLOAD_COUNTER_LONG_ABOUT,
        after_help = ENABLE_UPLOAD_COUNTER_AFTER_HELP
    )]
    EnableUploadCounter,
    /// Show or change whether .gitignore rules are respected during indexing
    #[command(
        name = "gitignore",
        long_about = GITIGNORE_LONG_ABOUT,
        after_help = GITIGNORE_AFTER_HELP
    )]
    Gitignore {
        /// Project path (default: current directory)
        #[arg(short, long)]
        path: Option<String>,
        /// "on" to enable, "off" to disable, omit to show current setting
        action: Option<String>,
    },
    /// Check tracedecay installation, configuration, and agent integration
    #[command(long_about = DOCTOR_LONG_ABOUT, after_help = DOCTOR_AFTER_HELP)]
    Doctor {
        /// Check only this agent (default: all agents)
        #[arg(long, value_parser = agent_value_parser())]
        agent: Option<String>,
    },
    /// Token cost summary from Claude Code sessions
    #[command(long_about = COST_LONG_ABOUT, after_help = COST_AFTER_HELP)]
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
    #[command(long_about = BENCH_LONG_ABOUT, after_help = BENCH_AFTER_HELP)]
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
    #[command(long_about = GAIN_LONG_ABOUT, after_help = GAIN_AFTER_HELP)]
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
    #[command(long_about = MONITOR_LONG_ABOUT, after_help = MONITOR_AFTER_HELP)]
    Monitor,
    /// Ingest and search local agent session transcripts
    #[command(long_about = SESSIONS_LONG_ABOUT, after_help = SESSIONS_AFTER_HELP)]
    Sessions {
        #[command(subcommand)]
        action: SessionsAction,
    },
    /// Adoption analytics: durable tool/hook events and diagnostics summary
    #[command(long_about = ANALYTICS_LONG_ABOUT, after_help = ANALYTICS_AFTER_HELP)]
    Analytics {
        #[command(subcommand)]
        action: AnalyticsAction,
    },
    /// Inspect registered TraceDecay projects from the global registry
    #[command(long_about = PROJECTS_LONG_ABOUT, after_help = PROJECTS_AFTER_HELP)]
    Projects {
        #[command(subcommand)]
        action: ProjectsAction,
    },
    /// Manage multi-branch indexing
    #[command(long_about = BRANCH_LONG_ABOUT, after_help = BRANCH_AFTER_HELP)]
    Branch {
        #[command(subcommand)]
        action: BranchAction,
    },
    /// Holographic memory maintenance (curation without the dashboard)
    #[command(long_about = MEMORY_LONG_ABOUT, after_help = MEMORY_AFTER_HELP)]
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// Self-improvement automation config and manual runs
    #[command(long_about = AUTOMATION_LONG_ABOUT, after_help = AUTOMATION_AFTER_HELP)]
    Automation {
        #[command(subcommand)]
        action: AutomationAction,
    },
    /// Inspect stores before profile-storage migration
    #[command(long_about = MIGRATE_LONG_ABOUT, after_help = MIGRATE_AFTER_HELP)]
    Migrate {
        #[command(subcommand)]
        action: MigrateAction,
    },
    /// Wipe local tracedecay DBs (current folder, parents, and children)
    #[command(long_about = WIPE_LONG_ABOUT, after_help = WIPE_AFTER_HELP)]
    Wipe {
        /// Wipe ALL tracked projects so the global DB ends empty
        #[arg(short, long)]
        all: bool,
    },
    /// List tracedecay projects (current folder, parents, and children)
    #[command(long_about = LIST_LONG_ABOUT, after_help = LIST_AFTER_HELP)]
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
    /// Bridge one host LSP stdio stream to an authenticated daemon session
    Bridge {
        /// Use standard input and output with strict Content-Length framing
        #[arg(long, required = true)]
        stdio: bool,
        /// Explicit project root to authorize for this session
        #[arg(long)]
        project: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum DaemonAction {
    /// Run the foreground daemon process
    Run {
        /// Unix socket path for MCP clients
        #[arg(long)]
        socket: Option<String>,
        /// Profile data root owned by this daemon process
        #[arg(long = "profile-root")]
        profile_root: Option<String>,
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
    /// Start the installed daemon service
    Start,
    /// Stop the installed daemon service
    Stop,
    /// Restart the installed daemon service (e.g. after a version mismatch)
    Restart,
    /// Print daemon service/socket status
    Status,
}

#[derive(Subcommand)]
pub enum AnalyticsAction {
    /// Print the adoption diagnostics summary (durable analytics events plus
    /// hook telemetry) for the current project
    Diagnostics {
        /// Include events for every project, not just the current one
        #[arg(long)]
        all: bool,
        /// Skip the hook-JSONL import pass before summarizing
        #[arg(long)]
        no_sync: bool,
        /// Keep compatibility with JSON-capable diagnostics commands.
        #[arg(long)]
        json: bool,
    },
    /// Import hook_analytics.jsonl rows into the durable analytics_events table
    Sync,
}

#[derive(clap::Args)]
pub(crate) struct SessionsSearchArgs {
    /// Full-text query to search for
    pub(crate) query: String,
    /// Optional explicit result scope; omit or use all for unified cross-provider search
    #[arg(long)]
    pub(crate) provider: Option<String>,
    /// Relationship scope: all, parents_only, or subagents_only
    #[arg(long, default_value = "all", value_parser = ["all", "parents_only", "subagents_only"])]
    pub(crate) scope: String,
    /// Semantic message type: all, direct_user, or tool_result
    #[arg(long, default_value = "all", value_parser = ["all", "direct_user", "tool_result"])]
    pub(crate) message_type: String,
    /// Only child sessions belonging to this parent session
    #[arg(long)]
    pub(crate) parent_session_id: Option<String>,
    /// Maximum number of matches
    #[arg(long, default_value_t = 10)]
    pub(crate) limit: usize,
    /// Inclusive minimum message timestamp. Accepts Unix seconds, RFC3339, YYYY-MM-DD, or relative time like "last hour"
    #[arg(long, alias = "time-from", alias = "start-time")]
    pub(crate) since: Option<String>,
    /// Inclusive maximum message timestamp. Accepts Unix seconds, RFC3339, YYYY-MM-DD, or relative time like "last hour"
    #[arg(long, alias = "time-to", alias = "end-time")]
    pub(crate) until: Option<String>,
    /// Registered project id whose session store should be searched
    #[arg(long)]
    pub(crate) project_id: Option<String>,
    /// Registered project root path or alias whose session store should be searched
    #[arg(long, conflicts_with = "project_id")]
    pub(crate) project_path: Option<String>,
    /// Only sessions correlated with this git branch
    #[arg(long)]
    pub(crate) branch: Option<String>,
    /// Only sessions correlated with this worktree path
    #[arg(long)]
    pub(crate) worktree: Option<String>,
    /// Only sessions that produced this commit (full or >=6-char prefix)
    #[arg(long)]
    pub(crate) commit: Option<String>,
}

#[derive(Subcommand)]
pub enum SessionsRefreshAction {
    /// Start or join the durable refresh and return an opaque handle
    Start(SessionRefreshBeginArgs),
    /// Report read-only progress or a terminal receipt using a refresh handle
    Status(SessionRefreshOperationArgs),
    /// Join or start the durable refresh and return an opaque handle
    Join(SessionRefreshBeginArgs),
    /// Resume, join, or start the durable refresh and return an opaque handle
    Resume(SessionRefreshBeginArgs),
    /// Durably cancel using a handle from start, join, resume, or begin
    Cancel(SessionRefreshOperationArgs),
    /// Compatibility spelling for start; returns an opaque handle
    Begin(SessionRefreshBeginArgs),
}

#[derive(Args)]
pub(crate) struct SessionRefreshSelectors {
    /// Registered project id that owns the refresh operation
    #[arg(
        long,
        conflicts_with_all = ["project_path", "profile_id"],
        required_unless_present_any = ["project_path", "profile_id"]
    )]
    pub(crate) project_id: Option<String>,
    /// Registered project root path or alias that owns the refresh operation
    #[arg(
        long,
        conflicts_with_all = ["project_id", "profile_id"],
        required_unless_present_any = ["project_id", "profile_id"]
    )]
    pub(crate) project_path: Option<String>,
    /// Typed profile id that owns a profile-scoped refresh operation
    #[arg(
        long,
        conflicts_with_all = ["project_id", "project_path"],
        required_unless_present_any = ["project_id", "project_path"]
    )]
    pub(crate) profile_id: Option<String>,
    /// Exact session id to refresh
    #[arg(long)]
    pub(crate) session_id: String,
    /// Exact provider scope for the session
    #[arg(long)]
    pub(crate) provider: String,
    /// Committed-through source frontier (must not exceed --target)
    #[arg(long)]
    pub(crate) source: u64,
    /// Observed-through target frontier; mode=current, grain=logical_message
    #[arg(long)]
    pub(crate) target: u64,
}

#[derive(Args)]
pub(crate) struct SessionRefreshBeginArgs {
    #[command(flatten)]
    pub(crate) selectors: SessionRefreshSelectors,
    /// Output the typed refresh outcome as JSON
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Args)]
pub(crate) struct SessionRefreshOperationArgs {
    #[command(flatten)]
    pub(crate) selectors: SessionRefreshSelectors,
    /// Opaque daemon-local handle returned by start, join, resume, or begin; --operation-id is deprecated
    #[arg(long, visible_alias = "operation-id")]
    pub(crate) handle: String,
    /// Output the typed refresh outcome as JSON
    #[arg(long)]
    pub(crate) json: bool,
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
    Search(Box<SessionsSearchArgs>),
    /// Run an explicit daemon-owned temporal refresh for one exact session scope
    Refresh {
        #[command(subcommand)]
        action: SessionsRefreshAction,
    },
    /// Backfill the session↔git correlation index from historical session,
    /// analytics, and reflog signals
    GitBackfill {
        /// Registered project id whose session store should be backfilled
        #[arg(long)]
        project_id: Option<String>,
        /// Registered project root path or alias whose session store should be backfilled
        #[arg(long, conflicts_with = "project_id")]
        project_path: Option<String>,
        /// Lower bound on session activity and commit times (ISO-8601 or unix
        /// seconds); defaults to 90 days ago
        #[arg(long)]
        since: Option<String>,
        /// Maximum number of sessions to scan
        #[arg(long, default_value_t = 500)]
        limit_sessions: usize,
        /// Derive and report counts without writing to the session store
        #[arg(long)]
        dry_run: bool,
    },
    /// List unfinished workflow/task evidence from ingested session messages
    Unfinished {
        /// Maximum evidence rows
        #[arg(long, default_value_t = 25)]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
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
    /// Consolidate two non-empty profile shards for one repository identity.
    #[command(
        long_about = CONSOLIDATE_LONG_ABOUT,
        after_help = CONSOLIDATE_AFTER_HELP
    )]
    Consolidate {
        /// Repository path whose git-common-dir identity owns both shards.
        #[arg(long, default_value = ".")]
        project: String,
        /// Legacy/input project id to preserve and merge.
        #[arg(long = "source-project-id")]
        source_project_id: String,
        /// Currently selected project id to use as the merge base.
        #[arg(long = "target-project-id")]
        target_project_id: String,
        /// Profile root containing projects/<project-id> shards.
        #[arg(long = "profile-root")]
        profile_root: Option<String>,
        /// Apply the planned consolidation. Omit for a read-only inventory.
        #[arg(long)]
        apply: bool,
        /// Confirmation token printed by the read-only inventory.
        #[arg(long = "confirm-token", requires = "apply")]
        confirm_token: Option<String>,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
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
        /// Run full SQLite integrity checks during a read-only inventory preview.
        #[arg(long = "verify-integrity")]
        verify_integrity: bool,
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
    /// Read-only per-store size, free-page ratio, and retention-backlog report
    /// (plan 38 §7). Never mutates anything; use `branch gc` and the daemon's
    /// automatic sweeps to reclaim what this reports.
    #[command(name = "storage-report")]
    StorageReport {
        /// Profile root to inspect (defaults to the resolved user data dir).
        #[arg(long = "profile-root")]
        profile_root: Option<String>,
        /// Inspect only this project shard, bypassing the global registry.
        #[arg(long = "project-id", requires = "project_root")]
        project_id: Option<String>,
        /// Canonical project root used to resolve the code-index scope.
        #[arg(long = "project-root", requires = "project_id")]
        project_root: Option<String>,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Create a complete checksummed profile backup under a quiesced exclusive lease.
    #[command(name = "backup-profile")]
    BackupProfile {
        /// Backup parent outside the TraceDecay profile.
        #[arg(long)]
        to: String,
        /// Stable backup directory name.
        #[arg(long = "backup-id")]
        backup_id: String,
    },
    /// Restore and verify a complete backup in an isolated destination.
    #[command(name = "rehearse-profile-backup")]
    RehearseProfileBackup {
        /// Complete backup directory containing `backup-manifest.json`.
        #[arg(long)]
        backup: String,
        /// New isolated restore directory.
        #[arg(long)]
        restore: String,
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
    /// Configure daemon auto-tracking of open PR branches
    Autotrack {
        #[command(subcommand)]
        action: BranchAutotrackAction,
    },
}

#[derive(Subcommand)]
pub enum BranchAutotrackAction {
    /// Show whether PR auto-tracking is enabled and list tracked PR branches
    Status {
        /// Project path (default: current directory)
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Enable PR auto-tracking for this project (daemon restart picks it up)
    Enable {
        /// Poll interval in seconds (minimum 60; default keeps the current value)
        #[arg(long)]
        poll_secs: Option<u64>,
        /// Project path (default: current directory)
        #[arg(short, long)]
        path: Option<String>,
    },
    /// Disable PR auto-tracking for this project
    Disable {
        /// Project path (default: current directory)
        #[arg(short, long)]
        path: Option<String>,
    },
}

#[cfg(test)]
mod parse_tests;

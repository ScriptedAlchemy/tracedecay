use clap::{builder::PossibleValuesParser, Parser, Subcommand};

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

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize a new TraceDecay project (full index)
    Init {
        /// Project path (default: current directory)
        path: Option<String>,
        /// Folders to skip during indexing (can be repeated)
        #[arg(long = "skip-folder", num_args = 1..)]
        skip_folders: Vec<String>,
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
    },
    /// Refresh settings for all already-installed agents
    Reinstall,
    /// Refresh generated plugin code/assets for detected installs without
    /// touching agent config files.
    ///
    /// Rewrites only tracedecay-generated artifacts — the Hermes plugin
    /// (.py files, schemas.json, dashboard page) for every detected profile,
    /// the Cursor plugin bundle, and the Kiro managed agent — re-baking the
    /// current binary path and version. Config files (Hermes config.yaml and
    /// its project_root pin, mcp.json, settings, prompt rules) are left
    /// byte-for-byte intact; use `tracedecay reinstall` to refresh those.
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
    /// Download and install the latest version from GitHub
    Upgrade,
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
pub enum SessionsAction {
    /// Ingest Cursor and/or Codex transcript JSONL files into the global DB
    Ingest {
        /// Provider to ingest: cursor, codex, or all
        #[arg(long)]
        provider: Option<String>,
    },
    /// Search previously ingested session messages
    Search {
        /// Full-text query to search for
        query: String,
        /// Provider to search: cursor, codex, or all
        #[arg(long)]
        provider: Option<String>,
        /// Maximum number of matches per provider
        #[arg(long, default_value_t = 10)]
        limit: usize,
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
        #[arg(long = "from-profile")]
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
    use super::{BranchAction, Cli, Commands, MemoryAction, MigrateAction, SessionsAction};
    use clap::{error::ErrorKind, Parser};

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
                json,
                short,
                details,
                runtime,
            }) if path.as_deref() == Some("/tmp/project")
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
                action: MemoryAction::Status { json, path }
            }) if json && path.as_deref() == Some("/tmp/project")
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
                action: SessionsAction::Ingest { provider },
            }) => assert_eq!(provider.as_deref(), Some("cursor")),
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
                    },
            }) => {
                assert_eq!(query, "needle");
                assert_eq!(provider.as_deref(), Some("codex"));
                assert_eq!(limit, 5);
            }
            _ => panic!("expected sessions search command"),
        }
    }
}

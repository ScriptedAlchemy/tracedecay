//! Long help (`long_about`) and example trailers (`after_help`) for every
//! visible CLI subcommand.
//!
//! The CLI is the MCP-parity surface: agents that have shell access but no
//! MCP client must be able to discover and drive every workflow from
//! `--help` output alone. Each subcommand therefore ships a purpose
//! paragraph (what it does and when to reach for it) plus an `Examples:`
//! trailer with real flag combinations and related-command pointers.
//! `cli::parse_tests::every_visible_top_level_subcommand_ships_rich_help`
//! pins this contract so new subcommands cannot ship bare help.

pub(crate) const TOP_LEVEL_AFTER_HELP: &str = "\
Quick start:
  tracedecay init                       Index the current repo (once per project)
  tracedecay sync                       Refresh the index after changes
  tracedecay tool                       List every MCP tool callable from the CLI
  tracedecay tool search \"parse config\" --limit 5
  tracedecay status --json              Machine-readable project statistics
  tracedecay doctor                     Diagnose installation + agent integration

MCP tool discovery flow:
  1. `tracedecay tool` lists every MCP tool with one-line summaries.
  2. `tracedecay tool <name> --help` prints that tool's parameters.
  3. `tracedecay tool <name> --key value [--json]` invokes it; use
     --args '<json>' or --args @file.json for complex payloads.

Most read commands accept --json for machine-readable output. Project-scoped
commands resolve the nearest initialised project walking up from the current
directory; pass a path argument or --project/--path/--project-id/--project-path
flags to target another project.

For more help on a command: tracedecay <command> --help";

pub(crate) const INIT_LONG_ABOUT: &str = "\
Walks the project tree, parses sources across the supported languages, and \
writes the code graph plus memory/session stores under .tracedecay/. Run once \
per repository; afterwards `tracedecay sync` keeps the index fresh \
incrementally. Respects .gitignore by default (see `tracedecay gitignore`).";

pub(crate) const INIT_AFTER_HELP: &str = "\
Examples:
  tracedecay init                                Index the current directory
  tracedecay init /path/to/repo                  Index another repository
  tracedecay init --skip-folder vendor --skip-folder dist
  tracedecay init --include-folder dist/generated

Related: tracedecay sync (incremental refresh), tracedecay status,
tracedecay gitignore, tracedecay wipe (delete local stores).";

pub(crate) const SYNC_LONG_ABOUT: &str = "\
Re-parses only files that changed since the last index and updates the code \
graph in place. Use after editing, switching branches, or pulling; agent \
hooks usually run it automatically. `--force` rebuilds from scratch when the \
index looks wrong; `--doctor`/`--verbose` explain what a sync actually did.";

pub(crate) const SYNC_AFTER_HELP: &str = "\
Examples:
  tracedecay sync                                Incremental refresh from cwd
  tracedecay sync --force                        Full re-index
  tracedecay sync --doctor                       List added/modified/removed files
  tracedecay sync --verbose                      Per-phase timings for slow syncs

Related: tracedecay init (first index), tracedecay status (freshness check).";

pub(crate) const STATUS_LONG_ABOUT: &str = "\
Reports node/edge/file counts, database size, index freshness, active branch, \
and tokens saved for the resolved project. Reach for it first when deciding \
whether the index is stale or when an agent needs project statistics; \
`--json` emits the same data machine-readably.";

pub(crate) const STATUS_AFTER_HELP: &str = "\
Examples:
  tracedecay status                              Human-readable project stats
  tracedecay status --json                       Machine-readable output
  tracedecay status --short                      Header only (version, tokens, sync)
  tracedecay status --details                    Node-kind breakdown
  tracedecay status --runtime                    PID/RSS/CPU/DB-size snapshot
  tracedecay status --project-id proj_123 --json Inspect another registered project

Related: tracedecay sync (refresh a stale index), tracedecay projects
(registry lookup), tracedecay doctor (installation health).";

pub(crate) const TOOL_LONG_ABOUT: &str = "\
Invoke any MCP tool from the shell — the full MCP surface with the same \
arguments and the same payloads, no MCP client required. This is the fallback \
path when an MCP transport fails and the primary path for scripts, hooks, and \
subagents that only have shell access.

Discovery flow: `tracedecay tool` (no name) lists every tool grouped by \
category; `tracedecay tool <name> --help` prints that tool's parameters; then \
invoke with alternating `--key value` flags.";

pub(crate) const TOOL_AFTER_HELP: &str = "\
Examples:
  tracedecay tool                                    List every tool, grouped
  tracedecay tool search --help                      One tool's parameters
  tracedecay tool search \"parse config\" --limit 5    Invoke with flags
  tracedecay tool context \"how does auth work\" --json
  tracedecay tool find_exact_symbol --name handle_tool_call
  tracedecay tool str_replace --path src/lib.rs --old-str @old.txt --new-str @new.txt
  tracedecay tool multi_str_replace --args @edits.json

Notes:
  - Tool names work with or without the tracedecay_ prefix; dashes and
    underscores are interchangeable (dead-code == dead_code).
  - --json prints the raw JSON payload instead of the human text rendering.
  - Any value starting with @ is read from that file. --args @file.json passes
    an entire JSON argument object — required for array/object parameters and
    for payloads larger than the ~128 KiB per-argument shell limit.
  - --project <path> targets another project; the default is the nearest
    initialised project walking up from the current directory.
  - Exit code is non-zero on unknown tools, bad arguments, or handler errors.

Related: tracedecay serve (same tools over MCP stdio), tracedecay status.";

pub(crate) const LSP_LONG_ABOUT: &str = "\
Shows which language servers the dashboard's code-diagnostics panel can use \
on this machine: detected binaries, versions, and install hints for missing \
ones. Purely informational — nothing is installed or started.";

pub(crate) const LSP_AFTER_HELP: &str = "\
Examples:
  tracedecay lsp servers                         Table of supported servers
  tracedecay lsp servers --json                  Machine-readable output

Related: tracedecay dashboard (uses these servers for diagnostics).";

pub(crate) const INSTALL_LONG_ABOUT: &str = "\
Writes the MCP server registration, permissions, hooks, and prompt rules for \
an agent host (Cursor, Codex, Claude Code, Hermes, Kiro, and others). \
Auto-detects installed agents when --agent is omitted. Safe to re-run; use it \
after installing a new agent or moving the tracedecay binary.";

pub(crate) const INSTALL_AFTER_HELP: &str = "\
Examples:
  tracedecay install                             Configure every detected agent
  tracedecay install --agent cursor              One agent only
  tracedecay install --agent codex --automation  Also enable the automation loop
  tracedecay install --agent hermes --profile dev
  tracedecay install --local                     Project-local config in cwd

Related: tracedecay uninstall, tracedecay reinstall (refresh settings),
tracedecay update-plugin (refresh generated assets only), tracedecay doctor.";

pub(crate) const REINSTALL_LONG_ABOUT: &str = "\
Re-runs the installer for every agent that already has tracedecay configured, \
rewriting MCP registrations, hooks, and prompt rules with current settings. \
Use after upgrading the binary manually or when agent config drifted; it \
never adds integration to agents that were not installed before.";

pub(crate) const REINSTALL_AFTER_HELP: &str = "\
Examples:
  tracedecay reinstall                           Refresh all installed agents

Related: tracedecay install (add an agent), tracedecay update-plugin
(refresh generated plugin assets without touching config files).";

pub(crate) const UPDATE_PLUGIN_AFTER_HELP: &str = "\
Examples:
  tracedecay update-plugin                       Refresh generated plugin assets

Related: tracedecay reinstall (also rewrites agent config files),
tracedecay update (binary + plugins + daemon + health pass).";

pub(crate) const UNINSTALL_LONG_ABOUT: &str = "\
Removes tracedecay's MCP server registration, permissions, hooks, and prompt \
rules from agent configuration. Removes every detected agent's integration \
when --agent is omitted. Project indexes under .tracedecay/ are left intact — \
use `tracedecay wipe` to delete data.";

pub(crate) const UNINSTALL_AFTER_HELP: &str = "\
Examples:
  tracedecay uninstall                           Remove from every agent
  tracedecay uninstall --agent cursor            Remove from one agent
  tracedecay uninstall --agent hermes --profile dev

Related: tracedecay install, tracedecay wipe (delete project stores).";

pub(crate) const DASHBOARD_LONG_ABOUT: &str = "\
Starts the local web dashboard: holographic memory curation, LCM session \
explorer, code-graph browser, analytics, and automation review UI. Binds to \
127.0.0.1 by default and prints the URL; leave it running while you work. \
Agents can start the same server via the tracedecay_dashboard MCP tool.";

pub(crate) const DASHBOARD_AFTER_HELP: &str = "\
Examples:
  tracedecay dashboard                           Serve on the default port
  tracedecay dashboard --open                    Also open it in the browser
  tracedecay dashboard --port 8788               Fixed port (0 picks a free one)
  tracedecay dashboard --path /path/to/repo      Serve another project

Related: tracedecay memory (curation without the dashboard),
tracedecay status --runtime (server resource snapshot).";

pub(crate) const SERVE_LONG_ABOUT: &str = "\
Runs the MCP server on stdin/stdout for a single client. This is the command \
agent hosts execute from their MCP configuration — you rarely run it by hand \
except to debug the protocol. For ad-hoc tool calls from a shell, use \
`tracedecay tool` instead; both dispatch the same tool registry.";

pub(crate) const SERVE_AFTER_HELP: &str = "\
Examples:
  tracedecay serve                               Stdio MCP server for the cwd project
  tracedecay serve --path /path/to/repo          Pin the project explicitly
  tracedecay serve --timings                     Annotate responses with handler time

Related: tracedecay tool (same tools from the shell), tracedecay install
(writes this command into agent MCP config), tracedecay daemon.";

pub(crate) const DAEMON_LONG_ABOUT: &str = "\
Manages the shared background daemon that MCP clients and `tracedecay tool` \
connect to over a Unix socket, so repeated calls skip per-process startup. \
Usually installed as a user service; check `daemon status` first when tool \
calls hang or version-mismatch errors appear.";

pub(crate) const DAEMON_AFTER_HELP: &str = "\
Examples:
  tracedecay daemon status                       Service and socket state
  tracedecay daemon install-service              Install + start the user service
  tracedecay daemon restart                      Restart after a version mismatch
  tracedecay daemon run --socket /tmp/td.sock    Foreground run (debugging)

Related: tracedecay doctor (detects daemon problems), tracedecay serve.";

pub(crate) const UPGRADE_AFTER_HELP: &str = "\
Examples:
  tracedecay upgrade                             Install the newest release
  tracedecay upgrade --no-heal                   Skip the post-update health pass

Related: tracedecay update (refresh even when current), tracedecay channel
(switch stable/beta).";

pub(crate) const UPDATE_AFTER_HELP: &str = "\
Examples:
  tracedecay update                              Upgrade if needed, then refresh
  tracedecay update --no-heal                    Skip the post-update health pass

Related: tracedecay upgrade (refresh only after a real install),
tracedecay update-plugin (plugins only), tracedecay channel.";

pub(crate) const CHANNEL_LONG_ABOUT: &str = "\
Shows the release channel used by `tracedecay upgrade`/`update`, or switches \
between `stable` and `beta`. Beta receives releases earlier; switching back \
to stable installs the newest stable build on the next upgrade.";

pub(crate) const CHANNEL_AFTER_HELP: &str = "\
Examples:
  tracedecay channel                             Show the current channel
  tracedecay channel beta                        Opt into beta releases
  tracedecay channel stable                      Return to stable

Related: tracedecay upgrade, tracedecay update.";

pub(crate) const CURRENT_COUNTER_LONG_ABOUT: &str = "\
Prints the project-local resettable token counter — tokens tracedecay saved \
this project since the last `reset-counter`. Useful for before/after \
comparisons of a working session; the permanent ledger lives in \
`tracedecay gain`.";

pub(crate) const CURRENT_COUNTER_AFTER_HELP: &str = "\
Examples:
  tracedecay current-counter                     Counter for the cwd project
  tracedecay current-counter --path /path/to/repo

Related: tracedecay reset-counter, tracedecay gain (persistent ledger),
tracedecay monitor (live view).";

pub(crate) const RESET_COUNTER_LONG_ABOUT: &str = "\
Zeroes the project-local token counter shown by `current-counter`, marking \
the start of a new measurement window. Does not touch the persistent global \
savings ledger used by `tracedecay gain`.";

pub(crate) const RESET_COUNTER_AFTER_HELP: &str = "\
Examples:
  tracedecay reset-counter                       Reset the cwd project counter
  tracedecay reset-counter --path /path/to/repo

Related: tracedecay current-counter, tracedecay gain.";

pub(crate) const DISABLE_UPLOAD_COUNTER_LONG_ABOUT: &str = "\
Opts this machine out of contributing anonymous token-savings counts to the \
public worldwide counter. Only aggregate numbers are ever uploaded — never \
code, paths, or queries — but uploading is entirely optional.";

pub(crate) const DISABLE_UPLOAD_COUNTER_AFTER_HELP: &str = "\
Examples:
  tracedecay disable-upload-counter              Stop contributing counts

Related: tracedecay enable-upload-counter, tracedecay gain (local ledger
is unaffected).";

pub(crate) const ENABLE_UPLOAD_COUNTER_LONG_ABOUT: &str = "\
Re-enables contributing anonymous token-savings counts to the public \
worldwide counter after a previous `disable-upload-counter`. Only aggregate \
numbers are uploaded — never code, paths, or queries.";

pub(crate) const ENABLE_UPLOAD_COUNTER_AFTER_HELP: &str = "\
Examples:
  tracedecay enable-upload-counter               Resume contributing counts

Related: tracedecay disable-upload-counter, tracedecay gain.";

pub(crate) const GITIGNORE_LONG_ABOUT: &str = "\
Shows or toggles whether indexing respects .gitignore rules for this project. \
Turning it off indexes ignored folders too (generated code, vendored deps); \
re-run `tracedecay sync --force` afterwards so the change takes effect. \
Prefer --include-folder on init/sync to whitelist single folders instead.";

pub(crate) const GITIGNORE_AFTER_HELP: &str = "\
Examples:
  tracedecay gitignore                           Show the current setting
  tracedecay gitignore off                       Index ignored files too
  tracedecay gitignore on                        Respect .gitignore again

Related: tracedecay sync --force (apply the change), tracedecay init
--include-folder (targeted alternative).";

pub(crate) const DOCTOR_LONG_ABOUT: &str = "\
Checks the binary, PATH, daemon service, project index, and every agent \
integration, printing actionable fixes for anything broken. Run it first \
when MCP tools are missing from an agent, tool calls fail, or after an \
upgrade behaves unexpectedly.";

pub(crate) const DOCTOR_AFTER_HELP: &str = "\
Examples:
  tracedecay doctor                              Check everything
  tracedecay doctor --agent cursor               Check one agent integration

Related: tracedecay install (fix missing integration), tracedecay daemon
status, tracedecay status (index health).";

pub(crate) const COST_LONG_ABOUT: &str = "\
Summarises token spend from local Claude Code session transcripts: totals, \
per-model and per-task-category breakdowns, and CSV/JSON export. Reads only \
local files; nothing is uploaded.";

pub(crate) const COST_AFTER_HELP: &str = "\
Examples:
  tracedecay cost                                Last 7 days
  tracedecay cost 30d --by-model                 Per-model breakdown
  tracedecay cost month --by-task                Per-task-category breakdown
  tracedecay cost all --export csv               Export the full history

Related: tracedecay gain (savings ledger), tracedecay monitor.";

pub(crate) const BENCH_LONG_ABOUT: &str = "\
Runs a reproducible retrieval benchmark (a fixed query set against the \
current project's index) and reports latency and result quality. Use it to \
compare index configurations or verify a tracedecay upgrade did not regress \
retrieval.";

pub(crate) const BENCH_AFTER_HELP: &str = "\
Examples:
  tracedecay bench                               Shipped default query set
  tracedecay bench --json                        Machine-readable results
  tracedecay bench --queries my-queries.toml --max-nodes 10

Related: tracedecay status (index size context).";

pub(crate) const GAIN_LONG_ABOUT: &str = "\
Reports token savings (and dollar estimates) recorded in the persistent \
global ledger — the long-term view, unlike the resettable per-project \
counter. Defaults to the current project over the last 30 days.";

pub(crate) const GAIN_AFTER_HELP: &str = "\
Examples:
  tracedecay gain                                Current project, last 30 days
  tracedecay gain --all --range all              Every project, all time
  tracedecay gain --history --json               Per-day history, machine-readable

Related: tracedecay current-counter (resettable window), tracedecay monitor
(live view), tracedecay cost (spend from transcripts).";

pub(crate) const MONITOR_LONG_ABOUT: &str = "\
Interactive full-screen view of token savings updating live across all \
projects as agents use tracedecay. Press Ctrl+C to exit. For scriptable \
numbers use `tracedecay gain --json` instead.";

pub(crate) const MONITOR_AFTER_HELP: &str = "\
Examples:
  tracedecay monitor                             Live savings across projects

Related: tracedecay gain --json (scriptable equivalent).";

pub(crate) const SESSIONS_LONG_ABOUT: &str = "\
Ingests agent session transcripts (Cursor, Codex, Claude Code, and other \
supported providers) into the project session store and searches them with \
full-text queries. The MCP twin of search is tracedecay_message_search; \
ingest usually runs automatically via agent hooks.";

pub(crate) const SESSIONS_AFTER_HELP: &str = "\
Examples:
  tracedecay sessions ingest                     Sweep all supported providers
  tracedecay sessions search \"auth refactor\"     Full-text transcript search
  tracedecay sessions search \"bug\" --limit 5 --provider cursor
  tracedecay sessions search \"plan\" --project-path /path/to/repo

Related: tracedecay tool message_search (MCP twin), tracedecay tool
lcm_grep (scoped/time-filtered recall), tracedecay memory.";

pub(crate) const PROJECTS_LONG_ABOUT: &str = "\
Queries the global registry of every initialised tracedecay project on this \
machine: list, search by id/path/alias/remote/branch, or resolve one \
project's full context. Use it to find the right --project-id/--project-path \
value for cross-project commands.";

pub(crate) const PROJECTS_AFTER_HELP: &str = "\
Examples:
  tracedecay projects list                       Registered projects
  tracedecay projects search my-repo --json      Find a project id
  tracedecay projects context proj_123           One project's registry context

Related: tracedecay status --project-id, tracedecay list (path-relative
view), tracedecay tool project_search (MCP twin).";

pub(crate) const BRANCH_LONG_ABOUT: &str = "\
Manages per-branch code-graph databases so queries reflect the branch you \
are on. Adding a branch copies the nearest ancestor's DB and syncs \
incrementally; gc removes DBs for branches deleted from git. Cross-branch \
queries are served by the branch_search/branch_diff MCP tools.";

pub(crate) const BRANCH_AFTER_HELP: &str = "\
Examples:
  tracedecay branch list                         Tracked branches and DB sizes
  tracedecay branch add feature/login            Track a branch explicitly
  tracedecay branch gc                           Drop DBs for deleted branches
  tracedecay branch remove feature/login

Related: tracedecay tool branch_search / branch_diff / branch_list
(cross-branch queries without switching checkout).";

pub(crate) const MEMORY_LONG_ABOUT: &str = "\
Inspects and curates the holographic memory store from the terminal — health \
status plus similarity-dedup curation with an explicit LLM review loop — \
without running the dashboard. Curation defaults to a dry-run preview; \
nothing is deleted without --apply.";

pub(crate) const MEMORY_AFTER_HELP: &str = "\
Examples:
  tracedecay memory status --json                Memory health and counts
  tracedecay memory curate                       Dry-run dedup preview
  tracedecay memory curate --apply               Apply proposed deletions
  tracedecay memory curate --llm > review.json   Emit the LLM review request
  tracedecay memory curate --llm-ops ops.json --apply

Related: tracedecay dashboard (visual curation), tracedecay tool fact_store
(read/write individual facts), tracedecay sessions (transcript recall).";

pub(crate) const AUTOMATION_LONG_ABOUT: &str = "\
Configures and drives the self-improvement automation loop (memory curator, \
session reflector, skill writer): sidecar config, manual dry-runs, run \
history with durable artifacts, managed-skill lifecycle, and fact-proposal \
review. Read-only MCP twins: tracedecay_skill_list, tracedecay_skill_view, \
tracedecay_automation_run_artifact_view.";

pub(crate) const AUTOMATION_AFTER_HELP: &str = "\
Examples:
  tracedecay automation config get --json        Effective config
  tracedecay automation config enable            Turn the loop on
  tracedecay automation run skill-writing        Manual dry-run
  tracedecay automation runs list --json         Run history
  tracedecay automation runs artifact run-123 codex_handoff --json
  tracedecay automation skills list              Managed skills
  tracedecay automation skills approve my-skill  Lifecycle changes
  tracedecay automation facts list               Pending fact proposals

Related: tracedecay install --agent codex --automation (enable at install),
tracedecay dashboard (review UI), tracedecay memory curate.";

pub(crate) const MIGRATE_LONG_ABOUT: &str = "\
Plans and executes profile-storage migrations: inventory scans, manifest \
plans, staged copy + cutover, verification, rollback, and registry cleanup. \
Mutating steps require the confirmation token printed by `migrate plan`; \
start with plan/verify, which never touch source stores.";

pub(crate) const MIGRATE_AFTER_HELP: &str = "\
Examples:
  tracedecay migrate plan --json                 Readonly inventory of stores
  tracedecay migrate plan --manifest plan.json   Write a manifest plan
  tracedecay migrate verify --manifest plan.json Check without mutating
  tracedecay migrate apply --manifest plan.json --confirm-token <token>
  tracedecay migrate registry-gc                 Dry-run stale-registry cleanup

Related: tracedecay projects (registry view), tracedecay wipe.";

pub(crate) const WIPE_LONG_ABOUT: &str = "\
Deletes .tracedecay stores (code graph, memory, sessions) for the current \
folder, its parents, and its children — or every tracked project with --all. \
Destructive and unrecoverable; re-create indexes with `tracedecay init`. \
Agent integration config is untouched (see `tracedecay uninstall`).";

pub(crate) const WIPE_AFTER_HELP: &str = "\
Examples:
  tracedecay wipe                                Wipe stores around the cwd
  tracedecay wipe --all                          Wipe every tracked project

Related: tracedecay list (see what would be affected), tracedecay init
(re-index afterwards), tracedecay uninstall (remove agent config instead).";

pub(crate) const LIST_LONG_ABOUT: &str = "\
Lists tracedecay projects relative to the current directory (itself, \
parents, and children) with their store locations — the quick \"what is \
indexed around here?\" check. Use `tracedecay projects` for the global \
registry with search.";

pub(crate) const LIST_AFTER_HELP: &str = "\
Examples:
  tracedecay list                                Projects around the cwd
  tracedecay list --all                          Every tracked project

Related: tracedecay projects list/search (global registry), tracedecay
status (one project's statistics).";

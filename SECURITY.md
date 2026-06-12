# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in tracedecay, please report it responsibly:

- **Email:** enzinol@gmail.com
- **GitHub:** Open a [private security advisory](https://github.com/ScriptedAlchemy/tokensave/security/advisories/new)

Please do **not** open a public issue for security vulnerabilities. We aim to acknowledge reports within 48 hours and provide a fix or mitigation plan within 7 days.

## Supported Versions

Only the current major release line is supported. All minor and patch versions within it receive security fixes.

| Version | Supported |
|---------|-----------|
| 6.x (current) | Yes — all minor and patch releases |
| < 6 | No |

**No vulnerabilities have been reported or discovered to date.**

When a vulnerability is found, the fix is shipped as a new release — there are no backports to older major versions. Fixes are not applied in place to existing binaries. **If you run tracedecay in production automation (CI pipelines, scheduled agents, server-side MCP deployments), keep it updated to the latest release** so any future fix reaches you immediately via `tracedecay upgrade`.

## Security Model

### What tracedecay stores

tracedecay builds a **local** code graph stored in a SQLite (libSQL) database (`.tracedecay/tracedecay.db`) inside your project directory (a legacy `.tokensave/` data directory from before the rename is still honored as a fallback). The database contains:

- Symbol names, signatures, and docstrings
- File paths, sizes, and content hashes
- Call relationships and dependency edges
- FTS5 search index
- Cross-session memory: durable facts, named entities, code-area notes, decisions, and feedback events in the holographic fact store. Those rows are local-only project data.
- A response cache for `tracedecay_read` (`read_cache` table): the rendered output served to the agent, stored as a BLOB keyed by file path, mode, and arguments. For full/line-range reads this rendered output contains source text. Rows are freshness-gated by file mtime and swept after a period of inactivity.

Aside from the `read_cache`, the graph itself does **not** persist raw source code — it stores structural metadata only. The database is local-only — there is no cloud sync, remote database, or server-side storage.

The user-level `~/.tracedecay/global.db` tracks indexed projects, aggregate token-saved counts, and cost accounting data parsed from Claude Code session transcripts. Project-local Cursor transcript search is stored in the repository's `.tracedecay/sessions.db`, which contains ingested Cursor user/assistant message text plus transcript paths and metadata for that project. Both databases remain local-only and are not synced to a remote service.

### Network access

tracedecay makes **no inbound network connections**. It never binds a port or listens for traffic. The MCP server communicates exclusively over stdio.

Outbound connections are limited to:

| Destination | Purpose | Auth | Failure mode |
|-------------|---------|------|-------------|
| `api.github.com` | Check for new releases | None (public API) | Silently ignored |
| `github.com` | Download binary during `tracedecay upgrade` | None (public releases) | Error shown to user |
| `tokensave-counter.enzinol.workers.dev` | Aggregate token-saved counter (endpoint keeps its pre-rename name) | None | Silently ignored |
| `raw.githubusercontent.com` | Fetch model pricing from [LiteLLM](https://github.com/BerriAI/litellm) | None (public file) | Falls back to embedded pricing |

All best-effort network calls use short timeouts (1-5 seconds) and never block the CLI or MCP server. The pricing fetch (5s timeout) only runs during `tracedecay cost` and is cached for 24 hours at `~/.tracedecay/pricing.json`.

### No credentials or secrets

tracedecay does not require, store, or transmit any credentials, API keys, tokens, or passwords. All external API calls target public, unauthenticated endpoints.

### MCP server tools

The MCP server exposes **more than 70 tools** (one fewer when the optional `ast-grep` binary is not on `PATH`). The large majority are **read-only** analysis and query operations marked `readOnlyHint: true`. A small set mutate local state and are marked `readOnlyHint: false`:

**File-editing tools** (modify source files in your project):

- `tracedecay_str_replace`, `tracedecay_multi_str_replace` — anchored string replacement
- `tracedecay_insert_at`, `tracedecay_insert_at_symbol` — anchored insertion
- `tracedecay_replace_symbol` — replace a symbol's body
- `tracedecay_ast_grep_rewrite` — structural rewrite via the external `ast-grep` binary

**Local-state tools** (write only inside `.tracedecay/`, never your source):

- `tracedecay_session_start`, `tracedecay_session_end` — health-metric baselines
- `tracedecay_fact_store`, `tracedecay_fact_feedback`, `tracedecay_memory_status` — store fact text, entity names, feedback events, trust-score inputs, and memory-bank repair state in the local project database.

**Test execution:**

- `tracedecay_run_affected_tests` — compiles and runs the project's own test suite via a `cargo` subprocess (bounded by a configurable wall-clock timeout, default 300 s, and a per-invocation test cap)

The edit tools target a single file with a unique anchor and re-index in place. They never run shell commands you didn't supply, and the server still **cannot** access the network on behalf of the AI agent. Every editing and state-mutating tool is single-file or single-record scoped — there is no bulk-delete or recursive-write primitive.

> Note: file edits are applied by the agent on your behalf through your agent's own tool-approval flow. Treat tracedecay's edit tools with the same caution as your agent's built-in file-write tools.

### Self-update integrity

`tracedecay upgrade` downloads pre-built binaries from [GitHub Releases](https://github.com/ScriptedAlchemy/tokensave/releases). The upgrade process:

- Downloads from the same release channel (stable/beta) currently installed
- Replaces the running binary in place via `self-replace`
- Re-registers agent integrations on the next launch when the version bump is minor or major

**macOS / Linux:** Release artifacts are not cryptographically signed. The integrity guarantee relies on HTTPS transport security and GitHub's release infrastructure.

**Windows:** Authenticode code signing via the [SignPath.io Foundation](https://signpath.io/foundation) program is being rolled out so Windows binaries are signed as part of the release workflow (addresses the Smart App Control block reported in #79). Until that lands in a published release, Windows binaries remain unsigned.

### No background daemon

tracedecay runs **no background daemon, system service, or autostart process**. The standalone `tracedecay daemon` command and its launchd/systemd/Windows-Service autostart were removed in 6.0.0. Index freshness is maintained entirely on demand: an on-demand staleness check on each MCP tool call (30-second cooldown) plus a catch-up sync when the MCP server connects. The server lives only for the lifetime of the attached agent and runs with **standard user privileges** — it never requests elevation.

### Subprocess-isolated extraction

Tree-sitter grammars are compiled C/C++ and can crash the process in ways Rust cannot catch. Each file is parsed inside a short-lived worker subprocess (the hidden `extract-worker` subcommand). The worker authenticates against its parent with a 256-bit per-spawn token supplied via the `TRACEDECAY_WORKER_TOKEN` environment variable; a user invoking `tracedecay extract-worker` directly fails immediately. Opt out with `TRACEDECAY_DISABLE_SUBPROCESS=1`.

### Unsafe code

The codebase contains minimal `unsafe`, used in two cross-platform places:

- **Memory-mapped monitor ring buffer** (`src/monitor.rs`) — `memmap2` maps `~/.tracedecay/monitor.mmap`, the shared buffer the `tracedecay monitor` TUI reads
- **Tree-sitter FFI** (`src/extraction/ts_provider.rs`) — constructing the bundled WGSL grammar from its raw C entry point

The Windows-elevation `unsafe` documented in earlier versions was removed alongside the daemon in 6.0.0.

## Best Practices

- Add `.tracedecay/` (and, for projects indexed before the rename, `.tokensave/`) to your `.gitignore` to avoid committing the local database.
- If your project contains sensitive code, be aware that the database stores symbol names and signatures, and the `read_cache` table can hold rendered source text from `tracedecay_read` responses. Adding `.tracedecay/` to `.gitignore` keeps both out of version control.
- Keep tracedecay updated (`tracedecay upgrade`) to receive security fixes.
- Review the [CHANGELOG](CHANGELOG.md) before upgrading to understand what changed.

## Scope

The following are **not** security issues:

- The aggregate token counter sending a count to the public Cloudflare Worker endpoint (this is documented behavior and contains no identifying information beyond an approximate country derived from IP by Cloudflare)
- The database containing symbol names or file paths from your project (this is core functionality)
- The MCP edit tools modifying files (this is opt-in functionality your AI agent invokes through its own tool-approval flow)
- `tracedecay_run_affected_tests` compiling and running your project's own test suite (this is the tool's documented purpose)

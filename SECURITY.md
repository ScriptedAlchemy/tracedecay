# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in tracedecay, please report it responsibly:

- **Email:** enzinol@gmail.com
- **GitHub:** Open a [private security advisory](https://github.com/ScriptedAlchemy/tracedecay/security/advisories/new)

Please do **not** open a public issue for security vulnerabilities. We aim to acknowledge reports within 48 hours and provide a fix or mitigation plan within 7 days.

## Supported Versions

Only the current major release line is supported. All minor and patch versions within it receive security fixes.

| Version | Supported |
|---------|-----------|
| 6.x (current) | Yes — all minor and patch releases |
| < 6 | No |

When a vulnerability is found, the fix is shipped as a new release — there are no backports to older major versions. Fixes are not applied in place to existing binaries. **If you run tracedecay in production automation (CI pipelines, scheduled agents, server-side MCP deployments), keep it updated to the latest release** so any future fix reaches you immediately via `tracedecay upgrade`.

## Security Model

### What tracedecay stores

tracedecay builds a **local** code graph stored in the active project store. Repo-local projects use `.tracedecay/tracedecay.db`; legacy `.tracedecay/` data directories are still honored. Profile-backed projects keep graph data in a private user profile shard such as `~/.tracedecay/projects/<project_id>/`, while the repository may contain only an enrollment marker plus project config. The database contains:

- Symbol names, signatures, and docstrings
- File paths, sizes, and content hashes
- Call relationships and dependency edges
- FTS5 search index
- Cross-session memory: durable facts, named entities, code-area notes, decisions, and feedback events in the holographic fact store. Those rows are local-only project data.
- A response cache for `tracedecay_read` (`read_cache` table): the rendered output served to the agent, stored as a BLOB keyed by file path, mode, and arguments. For full/line-range reads this rendered output contains source text. Rows are freshness-gated by file mtime and swept after a period of inactivity.

Aside from the `read_cache`, the graph itself does **not** persist raw source code — it stores structural metadata only. The active project store is local-only — there is no cloud sync, remote database, or server-side storage.

The user-level `~/.tracedecay/global.db` tracks indexed projects, aggregate tracedecayd counts, and cost accounting data parsed from Claude Code session transcripts. Cursor transcript search is stored in the active project's session store (`.tracedecay/sessions.db` for repo-local projects), which contains ingested Cursor user/assistant message text plus transcript paths and metadata for that project. Both stores remain local-only and are not synced to a remote service.

### Network access

The MCP process communicates with its host over stdio, but TraceDecay also has
explicitly local listeners:

- `tracedecay dashboard` and the `tracedecay_dashboard` MCP tool can start an
  HTTP dashboard on `127.0.0.1`, `localhost`, or `[::1]`. Requests must carry
  a loopback `Host` naming the bound port. Browser requests that include an
  `Origin` must use the same dashboard origin. The dashboard has no remote-user
  authentication and is intended only for a trusted, single-user local
  environment. HTTP clients cannot enable automation pre-run shell commands;
  `allow_job_commands` requires explicit local operator configuration.
- The daemon uses an owner-only Unix socket where supported. Its TCP fallback
  is restricted to a loopback address and requires the daemon authentication
  preface. The daemon's application HTTP adapter binds an ephemeral
  `127.0.0.1` port and requires both its bearer token and exact local origin.

These controls limit network reachability; they do not isolate TraceDecay from
other processes running as the same operating-system user.

Outbound connections are limited to:

| Destination | Purpose | Auth | Failure mode |
|-------------|---------|------|-------------|
| `api.github.com` | Check for releases and, for explicitly configured review sources, verify and perform repository reads | Public requests by default; optional read-only credential from the OS keyring | Public checks are best effort; configured private access fails closed when credentials or permissions cannot be verified |
| `github.com` | Download binary during `tracedecay upgrade` | None (public releases) | Error shown to user |
| `huggingface.co` and Hugging Face artifact hosts | Download missing, revision-pinned semantic-model artifacts when semantic auto-download is enabled | None | Semantic retrieval reports model acquisition state or failure; exact, lexical, and graph retrieval remain available |
| `tracedecay-counter.enzinol.workers.dev` | Aggregate tracedecayd counter (endpoint keeps its pre-rename name) | None | Silently ignored |

Provider usage and pricing do not add an outbound connection. `tracedecay cost`
reads immutable provider-native usage observations and the deterministic bundled
all-provider pricing table, identified by its content digest. Reads are
side-effect-free: there is no request-triggered network refresh, home-directory
pricing cache, or pricing environment override. Missing, unknown, or unavailable
evidence remains typed rather than becoming a zero or a stale estimate. Semantic
model downloads use a private TraceDecay cache, verify catalog-pinned lengths and
SHA-256 digests before publication, and can be disabled with `HF_HUB_OFFLINE`.

### Credentials and secrets

TraceDecay does not require credentials for its default local and public
repository behavior. A user may explicitly configure a private GitHub review
source with `access = "os_keyring"` and keyring service/account locators. The
secret remains in the operating-system keyring; configuration stores only its
locator. The daemon reads it into zeroizing memory, sends it only to GitHub
over HTTPS, and mounts the source only after verifying an exact read-only
permission set. Missing, ambiguous, write-capable, or unverifiable credentials
fail closed.

### MCP server tools

The MCP server exposes **more than 70 tools** (one fewer when the optional `ast-grep` binary is not on `PATH`). The large majority are **read-only** analysis and query operations marked `readOnlyHint: true`. A small set mutate local state and are marked `readOnlyHint: false`:

**File-editing tools** (modify source files in your project):

- `tracedecay_str_replace`, `tracedecay_multi_str_replace` — anchored string replacement
- `tracedecay_insert_at`, `tracedecay_insert_at_symbol` — anchored insertion
- `tracedecay_replace_symbol` — replace a symbol's body
- `tracedecay_ast_grep_rewrite` — structural rewrite via the external `ast-grep` binary

**Local-state tools** (write only inside the active TraceDecay store, never your source):

- `tracedecay_fact_store_add`, `tracedecay_fact_store_update`, `tracedecay_fact_store_remove`, and `tracedecay_fact_feedback` — store or remove fact text, entity names, feedback events, and trust-score inputs in the local project database. The other exact `tracedecay_fact_store_*` routes and `tracedecay_memory_status` are read-only; repair is daemon-owned background work.

### Support bundles and storage diagnostics

Storage status, doctor, quota, and support-bundle output must report the active project, store class (`project_local`, `profile_sharded`, or global/accounting), and final-shape admission state without exposing sensitive payloads by default. A redacted support bundle may include aggregate counts, lock/dirty/quota state, and error codes; it must exclude source code, rendered `read_cache` bodies, transcript text, memory fact content, payload bodies, and response-handle bodies.

Also redact credential-bearing git remotes, database overrides such as `TRACEDECAY_GLOBAL_DB`, private adapter config paths, response-handle identifiers that could retrieve plaintext, and error strings that embed local paths or secrets. Full paths or payload excerpts require an explicit opt-in flag and sensitive labeling. See [docs/PROFILE-STORAGE-SUPPORT.md](docs/PROFILE-STORAGE-SUPPORT.md) for the support-bundle privacy boundary, and [the V2 operating model](docs/V2-OPERATING-MODEL.md) for final storage authority and reset behavior.

**Test execution:**

- `tracedecay_run_affected_tests` — compiles and runs the project's own test suite via a `cargo` subprocess (bounded by a configurable wall-clock timeout, default 300 s, and a per-invocation test cap)

The edit tools target a single file with a unique anchor and re-index in place.
They never run shell commands you did not supply. Network-capable operations
are limited to the documented release, pricing, semantic-model, telemetry, and
configured GitHub review paths above. Every editing and state-mutating tool is
single-file or single-record scoped — there is no bulk-delete or
recursive-write primitive.

> Note: file edits are applied by the agent on your behalf through your agent's own tool-approval flow. Treat tracedecay's edit tools with the same caution as your agent's built-in file-write tools.

### Self-update integrity

`tracedecay upgrade` downloads pre-built binaries from [GitHub Releases](https://github.com/ScriptedAlchemy/tracedecay/releases). The upgrade process:

- Downloads from the same release channel (stable/beta) currently installed
- Replaces the running binary in place via `self-replace`
- Re-registers agent integrations on the next launch when the version bump is minor or major

**macOS / Linux:** Release artifacts are not cryptographically signed. The integrity guarantee relies on HTTPS transport security and GitHub's release infrastructure.

**Windows:** Authenticode code signing via the [SignPath.io Foundation](https://signpath.io/foundation) program is being rolled out so Windows binaries are signed as part of the release workflow (addresses the Smart App Control block reported in #79). Until that lands in a published release, Windows binaries remain unsigned.

### Opt-in background daemon

tracedecay installs **no background daemon, system service, or autostart process by default**. Users can explicitly opt in with `tracedecay daemon install-service`, which installs a per-user systemd service on Linux or a per-user LaunchAgent on macOS. The daemon runs with **standard user privileges** and never requests elevation. Index freshness still relies on on-demand staleness checks, catch-up syncs when MCP clients connect, and bounded hook notifications; the daemon provides shared MCP process/socket reuse and scheduled automation for projects that connect to it.

### Unsafe code

The codebase contains minimal `unsafe`, used in two cross-platform places:

- **Memory-mapped monitor ring buffer** (`src/monitor.rs`) — `memmap2` maps `~/.tracedecay/monitor.mmap`, the shared buffer the `tracedecay monitor` TUI reads
- **Tree-sitter FFI** (`src/extraction/ts_provider.rs`) — constructing the bundled WGSL grammar from its raw C entry point

The Windows-elevation `unsafe` documented in earlier versions was removed alongside the daemon in 6.0.0.

## Best Practices

- Add `.tracedecay/` (and, for projects indexed before the rename, `.tracedecay/`) to your `.gitignore` to avoid committing local store markers or repo-local databases.
- If your project contains sensitive code, be aware that the database stores symbol names and signatures, and the `read_cache` table can hold rendered source text from `tracedecay_read` responses. Keeping repo-local store directories ignored and treating profile-sharded stores as private user data keeps both out of version control.
- Keep tracedecay updated (`tracedecay upgrade`) to receive security fixes.
- Review the [CHANGELOG](CHANGELOG.md) before upgrading to understand what changed.

## Scope

The following are **not** security issues:

- The aggregate token counter sending a count to the public Cloudflare Worker endpoint (this is documented behavior and contains no identifying information beyond an approximate country derived from IP by Cloudflare)
- The database containing symbol names or file paths from your project (this is core functionality)
- The MCP edit tools modifying files (this is opt-in functionality your AI agent invokes through its own tool-approval flow)
- `tracedecay_run_affected_tests` compiling and running your project's own test suite (this is the tool's documented purpose)

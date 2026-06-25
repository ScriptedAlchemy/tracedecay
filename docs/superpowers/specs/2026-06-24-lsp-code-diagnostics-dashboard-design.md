# LSP Code Diagnostics Dashboard Design

## Summary

Phase 1 adds a dedicated **Code Diagnostics** dashboard surface powered by a TraceDecay-owned, LSP-first diagnostics broker. The broker starts supported language servers when available, keeps them warm while the dashboard process is alive, caches diagnostics, and exposes status/results only through dashboard APIs. Hooks, prompt hints, MCP auto-context, and model-visible summaries stay out of Phase 1 and remain Phase 2 extension points.

The platform is language-generic, not Rust-only. Rust is an important validation path because `rust-analyzer` highlights the cold `cargo check` problem, but the broker, adapter registry, and dashboard controls must work for every supported language server.

Built-in adapters should cover the practical, low-friction language servers first:

- Rust via `rust-analyzer`
- TypeScript and JavaScript via `typescript-language-server`
- Python via `pyright-langserver`
- Go via `gopls`
- C, C++, and Objective-C via `clangd`
- Zig via `zls`
- Lua via `lua-language-server`
- PHP via `intelephense`

The registry must also allow project-configured custom adapters so languages without a built-in adapter can still participate when a user knows the server command, language id, file extensions, and root markers.

Each language can be enabled or disabled from the dashboard. Disabled languages stop their LSP worker, clear pending refresh work, and remain visible as disabled in the engine status table.

## Goals

- Prefer warm LSP diagnostics over repeated batch tool invocations.
- Surface code diagnostics in the dashboard only.
- Let users enable or disable LSP diagnostics per language from the dashboard.
- Support a broad language-server registry, not a Rust-specialized path.
- Allow project-configured custom LSP adapters for languages beyond built-ins.
- Keep the design fail-open: missing LSP binaries, broken initialization, or server crashes should not break the dashboard.
- Preserve the current `tracedecay_diagnostics` MCP tool as an explicit one-shot diagnostics path.
- Leave hook/model-context surfacing designed but unimplemented.

## Non-Goals

- Do not inject diagnostics into Codex/Cursor/Kiro hooks.
- Do not add model-visible hints or prompt context.
- Do not attach to editor-owned LSP instances in Phase 1.
- Do not replace the existing `tracedecay_diagnostics` MCP tool.
- Do not run batch compiler checks automatically as the dashboard default when LSP is unavailable.
- Do not make LSP diagnostics an autostart system daemon.

## Product Surface

The dashboard gains a dedicated **Code Diagnostics** tab or plugin area. It stays separate from the existing Savings & Cost `Diagnostics` view, which reports TraceDecay hook/tool/prompt telemetry rather than compiler or type diagnostics.

The Code Diagnostics UI includes:

- Summary stats: total errors, total warnings, pending refreshes, last refresh age.
- Engine status table:
  - language
  - adapter/server binary
  - enabled/disabled toggle
  - state: unavailable, disabled, starting, indexing, ready, refreshing, crashed
  - last error
  - last diagnostic update time
- File-grouped diagnostics table:
  - file
  - line range
  - severity
  - code/rule
  - message
  - language/driver
  - enclosing TraceDecay node when available
- Controls:
  - Refresh all enabled languages
  - Refresh one language
  - Enable/disable one language
  - Restart one language server
  - Enable/disable idle whole-project backfill

Dashboard toggles are persisted in the active project store so the setting survives dashboard restarts and is branch/worktree scoped with the rest of the active TraceDecay store.

## Backend Architecture

Add a diagnostics broker module under `src/diagnostics/lsp/`:

```text
src/diagnostics/lsp/
    mod.rs          public broker types and module exports
    broker.rs       per-project orchestration, cache, refresh queue, status
    client.rs       stdio JSON-RPC LSP client
    protocol.rs     minimal LSP request/notification/diagnostic structs
    adapters.rs     adapter trait, built-in adapters, custom adapter loader
    settings.rs     project-persisted language enablement
```

Dashboard state owns the broker; hidden autostart infrastructure does not. When `tracedecay dashboard` starts, it builds one `DiagnosticBroker` for the active project. The broker starts language servers lazily when the Code Diagnostics UI asks for status, diagnostics, or refresh, so users who never open the tab do not get surprise background work.

The broker stores:

- project root
- active store/dashboard sidecar root
- per-language settings
- per-language LSP client handle
- per-language engine state
- cached diagnostics
- refresh queue state
- idle backfill queue state
- last refresh timestamps and errors

## Adapter Registry and Language Coverage

The LSP broker uses an adapter registry. Each adapter declares:

- which TraceDecay languages it handles
- LSP language id for each file type
- binary names to probe
- root markers and manifests
- spawn command and arguments
- initialization options/settings
- supported file extensions
- whether diagnostics are push, pull, or both

Built-in adapters:

| TraceDecay language(s) | LSP server | Binary | Root/manifest signal | Notes |
| --- | --- | --- | --- | --- |
| Rust | rust-analyzer | `rust-analyzer` | `Cargo.toml` | Primary validation path for warm diagnostics. |
| TypeScript, JavaScript | typescript-language-server | `typescript-language-server` | `tsconfig.json`, `jsconfig.json`, or indexed TS/JS files | Handles `.ts`, `.tsx`, `.js`, `.jsx`. |
| Python | pyright-langserver | `pyright-langserver` | `pyrightconfig.json`, `pyproject.toml`, or indexed Python files | Prefer project config when present to reduce import noise. |
| Go | gopls | `gopls` | `go.mod` or indexed Go files | Falls back to workspace root when no module exists. |
| C, C++, Objective-C | clangd | `clangd` | `compile_commands.json` optional | One shared server adapter handles C-family languages. |
| Zig | zls | `zls` | `build.zig` optional | Useful even for single-file projects. |
| Lua | lua-language-server | `lua-language-server` | `.luarc.json` optional | Workspace scan can be expensive; lazy start matters. |
| PHP | intelephense | `intelephense` | `composer.json` optional | Definition/diagnostics support varies by configuration. |

Custom adapters are configured in `tracedecay.toml` or the active project store:

```toml
[[lsp.custom]]
language = "ruby"
language_id = "ruby"
command = "ruby-lsp"
args = []
extensions = ["rb"]
root_markers = ["Gemfile", ".ruby-version"]
diagnostics = "push"
```

The dashboard should list built-in and custom adapters together. Unsupported languages should show an “Add custom LSP adapter” affordance rather than imply TraceDecay has no path forward.

## External Implementation Notes

The design should reuse proven LSP-client shapes from existing projects rather than invent protocol machinery from scratch.

Relevant findings:

- The Codex repository had a closed PR for a `rust-analyzer-lsp-timing` sample skill: <https://github.com/openai/codex/pull/15370>. The PR added helper scripts for one long-lived `rust-analyzer` process, timing from `textDocument/didChange` to `textDocument/publishDiagnostics`, and a tiny UNIX-socket control wrapper. The useful design lessons are persistent process reuse, explicit health probes, workspace-keyed control state, text-document version tracking, and waiting for publish-diagnostics events instead of rerunning cargo manually.
- The same Codex commit (`7b230fc`) shows a minimal stdio client shape: spawn `rust-analyzer`, send `Content-Length` framed JSON-RPC, issue `initialize`/`initialized`, send `didOpen` and full-document `didChange`, and collect `textDocument/publishDiagnostics`.
- `codive-lsp` documents an agent-oriented Rust LSP architecture with modules for server definitions/spawning, JSON-RPC client, lazy facade/caching, file-extension language mapping, and support for rust-analyzer, TypeScript, Pyright, and gopls: <https://docs.rs/codive-lsp/latest/codive_lsp/>.
- `tokio-lsp` is a lightweight async-first Rust LSP client crate with transport abstraction and serde-based typed messages: <https://docs.rs/tokio-lsp/latest/tokio_lsp/>.
- `lsp-types` provides shared Rust structs for LSP messages and should be preferred over hand-written request/diagnostic structs where it fits: <https://docs.rs/lsp-types/>.
- `bacon-ls` is not a replacement for rust-analyzer, but it is a useful Rust diagnostics reference. It exposes `textDocument/diagnostic` and `workspace/diagnostic`, supports partial diagnostic publishes during long cargo runs, cancellation of running checks, manual retrigger commands, and backends for direct cargo or already-running Bacon: <https://github.com/crisidev/bacon-ls>.

Phase 1 should use these notes as implementation guidance:

- Use `lsp-types` for protocol data structures unless a required server extension is missing.
- Keep the transport small and explicit: stdio JSON-RPC with `Content-Length` framing, one reader task, one writer path, and a pending request map.
- Track `textDocument` versions per opened file. Send full-document `didChange` first; incremental range changes can wait until there is evidence the full-text path is too expensive.
- Treat `textDocument/publishDiagnostics` as the primary diagnostic source. Add `textDocument/diagnostic` or `workspace/diagnostic` only after capability detection proves a server supports pull diagnostics.
- Keep health/control state inside the dashboard-owned broker rather than a separate autostart daemon. A reconnectable local socket is a useful future shape, but Phase 1 does not need it.
- For Rust, measure and expose diagnostic latency in status fields. The Codex sample was explicitly about timing edit-to-diagnostic latency, and that signal will help tune debounce and refresh behavior.

## LSP Lifecycle

For each supported language, an adapter provides:

- language id used by LSP
- binary names to detect
- project manifest detection
- spawn command
- initialization options
- optional workspace folders
- file extensions to open
- diagnostic capability support

TraceDecay starts stdio LSP servers itself in Phase 1. It does not attach to Cursor, VS Code, or other editor-owned LSP sessions because the LSP lifecycle is client-owned and not exposed through a standard cross-editor discovery API.

The active lifecycle is:

1. Dashboard asks for Code Diagnostics.
2. Broker loads per-language settings.
3. For each enabled language, broker detects whether the LSP binary and project shape are available.
4. On refresh, broker starts missing enabled clients.
5. Broker sends `initialize` and `initialized`.
6. Broker opens relevant project files using `textDocument/didOpen`.
7. Broker collects diagnostics from `textDocument/publishDiagnostics`.
8. When supported, broker can also issue `textDocument/diagnostic` or `workspace/diagnostic`.
9. Broker maps diagnostics to project-relative files and enriches them with enclosing graph nodes.
10. Dashboard reads the cached snapshot.

The passive lifecycle is:

1. Once the Code Diagnostics dashboard has been opened, the broker may begin idle backfill for enabled languages.
2. The broker builds a per-language queue from TraceDecay's indexed files.
3. The broker opens files in small batches while the dashboard process is otherwise idle.
4. Each LSP server publishes diagnostics for files it can analyze.
5. The broker updates coverage counters so the UI can distinguish "whole project covered" from "only recently opened/refreshed files covered."

Server crashes are converted into engine status and last error fields. The UI remains usable, and other languages keep running.

## Refresh Model

Phase 1 uses explicit dashboard refresh and dashboard-owned idle backfill, not edit hooks.

Refresh requests are debounced per language:

- If a refresh is already running, a new request marks the language as pending.
- When the current refresh finishes, one pending refresh may run.
- Repeated clicks do not create unbounded work.

Refresh scopes:

- `all`: refresh every enabled language.
- `language`: refresh one enabled language.

The broker should avoid a full project file walk on every refresh when possible. It can use TraceDecay’s indexed file list and language/file-extension mapping to find candidate files. If the index is stale, the dashboard should show the index freshness status instead of silently forcing a sync.

## Idle Whole-Project Backfill

Phase 1 passively collects diagnostics for files the user has not touched, but only through dashboard-owned idle work. This gives the dashboard a project-wide type-error view without slowing hooks or prompt submission.

Idle backfill behavior:

- Starts only after the Code Diagnostics dashboard surface is opened or the user explicitly enables Code Diagnostics for the project.
- Runs only for enabled languages.
- Uses TraceDecay's indexed file list to avoid a fresh filesystem walk.
- Processes files in small batches per language.
- Yields to explicit refresh/restart/toggle requests.
- Pauses when an LSP server reports indexing/busy status or when refresh work is active.
- Stops immediately when a language is disabled.
- Records progress per language: queued files, opened files, files with diagnostics, last completed sweep.

Backfill modes:

- `off`: no passive project sweep.
- `idle`: default. Backfill only when the dashboard process is idle and no explicit refresh is active.

The Phase 1 default is `idle`, which gives broad coverage without surprising CPU use. A repeating `continuous` sweep can be considered later, but it is not part of Phase 1. The dashboard should expose the setting and show current backfill progress.

Whole-project coverage is best-effort. Some LSP servers publish diagnostics for the entire workspace after initialization; others only publish for opened files. The broker should support both:

- For servers that support `workspace/diagnostic`, request workspace diagnostics and cache the result.
- For servers that support only push diagnostics, open files in bounded batches and wait for `textDocument/publishDiagnostics`.
- For servers that only diagnose visible/open files reliably, mark coverage as partial rather than pretending the project is fully checked.

## Diagnostics Cache

Define a normalized diagnostic record shared by the broker and dashboard API:

```rust
pub struct CodeDiagnostic {
    pub language: String,
    pub source: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub character_start: Option<u32>,
    pub character_end: Option<u32>,
    pub severity: String,
    pub code: String,
    pub message: String,
    pub enclosing: Option<String>,
}
```

Define per-language engine state:

```rust
pub struct DiagnosticEngineStatus {
    pub language: String,
    pub server: String,
    pub enabled: bool,
    pub available: bool,
    pub state: String,
    pub diagnostic_count: usize,
    pub error_count: usize,
    pub warning_count: usize,
    pub indexed_file_count: usize,
    pub covered_file_count: usize,
    pub backfill_state: String,
    pub backfill_queued: usize,
    pub backfill_completed: usize,
    pub last_started_at: Option<i64>,
    pub last_updated_at: Option<i64>,
    pub last_backfill_completed_at: Option<i64>,
    pub last_error: Option<String>,
}
```

The first implementation can keep the cache in memory while the dashboard runs. Persisting the latest snapshot to a dashboard sidecar table is allowed in Phase 1 for reload behavior, but the UI must clearly distinguish cached/stale data from fresh data.

## Dashboard API

Add a new API module, for example `src/dashboard/code_diagnostics_api.rs`, mounted under `/api/plugins/code-diagnostics`.

Endpoints:

- `GET /api/plugins/code-diagnostics/overview`
  - Returns enabled languages, engine statuses, totals, and last update metadata.
- `GET /api/plugins/code-diagnostics/diagnostics`
  - Returns cached diagnostics with optional query params:
    - `language`
    - `severity`
    - `file`
    - `limit`
    - `offset`
- `POST /api/plugins/code-diagnostics/refresh`
  - Body: `{ "language": "rust" }` or `{ "language": "all" }`
  - Enqueues refresh and returns current status immediately.
- `POST /api/plugins/code-diagnostics/settings`
  - Body: `{ "language": "rust", "enabled": false }` or `{ "idle_backfill": "idle" }`
  - Persists language and backfill settings. Disabling a language shuts down its client.
- `POST /api/plugins/code-diagnostics/restart`
  - Body: `{ "language": "rust" }`
  - Restarts an enabled language client and enqueues refresh.

The API should return plain JSON and never expose diagnostics to hooks or MCP model-context paths.

## Dashboard UI

Add a dedicated frontend package or panel matching existing dashboard plugin patterns. The UI should be quiet, operational, and scan-friendly:

- Top summary band with counts and freshness.
- Engine table with compact language controls.
- Diagnostics table grouped by file.
- Filters for language and severity.
- Refresh/restart controls using existing button and table primitives.
- Idle backfill control with progress for project-wide coverage.

The UI should distinguish disabled and unavailable languages:

- Disabled: user intentionally turned it off.
- Unavailable: enabled, but binary or project manifest is missing.
- Crashed: server started but failed.

Toggle behavior:

- Turning a language off immediately calls the settings endpoint and stops the server.
- Turning it on calls the settings endpoint, then enqueues a refresh.
- The UI should not block while refresh runs; it shows starting/indexing/refreshing status.
- The idle backfill control changes only broker/dashboard behavior. It must not install hooks or alter model-facing context.

## Built-In Adapter Details

All built-in adapters share the same broker/client/cache path. The sections below call out language-specific behavior, but none of them should fork the architecture.

## Rust Adapter

The Rust adapter uses `rust-analyzer`.

Detection:

- Project has `Cargo.toml`.
- `rust-analyzer` is available on `PATH`.

Initialization:

- Root URI is the project root.
- Configure Cargo target dir to avoid contention with the user’s interactive builds when rust-analyzer supports that setting.
- Prefer diagnostics from rust-analyzer’s normal diagnostic publication path.

Rust-specific notes:

- rust-analyzer may run its own background cargo check/flycheck. That is acceptable because it is warm, debounced, and integrated with the LSP session.
- The current `cargo check` diagnostics driver remains available for explicit fresh MCP checks.

## TypeScript Adapter

The TypeScript adapter uses `typescript-language-server`.

Detection:

- Project has `tsconfig.json` or TypeScript/JavaScript files.
- `typescript-language-server` is available on `PATH`.

Initialization:

- Root URI is the project root.
- Open indexed `.ts`, `.tsx`, `.js`, and `.jsx` files as needed.

TypeScript-specific notes:

- Phase 1 should not silently fall back to `tsc --watch`.
- If the language server is unavailable, dashboard status says unavailable.

## Python Adapter

The Python adapter uses `pyright-langserver`.

Detection:

- Project has `pyrightconfig.json` or `pyproject.toml`.
- `pyright-langserver` is available on `PATH`.

Initialization:

- Root URI is the project root.
- Open indexed `.py` files as needed.

Python-specific notes:

- Phase 1 uses the LSP server path rather than `pyright --watch`.
- The current `pyright --outputjson` batch driver remains available through explicit MCP diagnostics.

## Go Adapter

The Go adapter uses `gopls`.

Detection:

- Project has `go.mod` or indexed `.go` files.
- `gopls` is available on `PATH`.

Initialization:

- Root URI is the module root when `go.mod` exists, otherwise the project root.
- Open indexed `.go` files as needed.

## C-Family Adapter

The C-family adapter uses `clangd` for C, C++, and Objective-C.

Detection:

- Project has indexed `.c`, `.h`, `.cc`, `.cpp`, `.cxx`, `.hpp`, `.m`, or `.mm` files.
- `clangd` is available on `PATH`.

Initialization:

- Root URI is the project root.
- Prefer `compile_commands.json` when present.
- Surface degraded status when `compile_commands.json` is missing and diagnostics may be incomplete.

## Zig Adapter

The Zig adapter uses `zls`.

Detection:

- Project has `build.zig` or indexed `.zig` files.
- `zls` is available on `PATH`.

Initialization:

- Root URI is the project root.
- Open indexed `.zig` files as needed.

## Lua Adapter

The Lua adapter uses `lua-language-server`.

Detection:

- Project has indexed `.lua` files.
- `lua-language-server` is available on `PATH`.

Initialization:

- Root URI is the project root.
- Respect `.luarc.json` or `.luarc.jsonc` when present.
- Avoid eager full-workspace file opening; start with files requested by refresh.

## PHP Adapter

The PHP adapter uses `intelephense`.

Detection:

- Project has `composer.json` or indexed `.php` files.
- `intelephense` is available on `PATH`.

Initialization:

- Root URI is the project root.
- Open indexed `.php` files as needed.

## Error Handling

All LSP diagnostics work is fail-open:

- Missing server binary: language state becomes unavailable.
- Initialization timeout: language state becomes crashed with last error.
- Server exits unexpectedly: language state becomes crashed, diagnostics remain cached but stale.
- Malformed LSP diagnostics: malformed entries are dropped and counted in engine status.
- Graph enrichment failure: diagnostics are still returned with `enclosing: null`.

One broken language server must never panic the dashboard or prevent it from loading.

## Testing Strategy

Backend tests:

- Adapter registry returns all built-in adapters and merges project custom adapters.
- Custom adapter config rejects missing language id, command, or extensions with a clear error.
- Settings persistence toggles a language on/off.
- Idle backfill setting persists as `off` or `idle`.
- Disabling a language stops its engine and prevents refresh enqueue.
- Refresh request for unavailable language returns status without spawning work.
- Idle backfill uses indexed files and does not perform a fresh filesystem walk.
- Idle backfill opens files in bounded batches and yields to explicit refresh.
- Idle backfill stops when a language is disabled.
- LSP diagnostic normalization maps severity, code, file, ranges, and message.
- Enclosing node enrichment handles known and unknown files.
- API overview returns per-language states and totals.
- API diagnostics supports language/severity/file filters.

Frontend tests:

- Adapter list renders built-in and custom languages together.
- Engine status table distinguishes disabled, unavailable, ready, refreshing, and crashed.
- Toggle off calls settings endpoint and removes refresh controls for that language.
- Toggle on calls settings endpoint and refresh endpoint.
- Idle backfill control calls settings endpoint and shows queue/progress status.
- Diagnostics table groups by file and renders line/severity/code/message.
- Empty state explains when no enabled LSP engines are available.

Manual verification:

- Start dashboard in a Rust project with `rust-analyzer` installed.
- Open Code Diagnostics.
- Enable Rust.
- Refresh.
- Confirm diagnostics appear or “no diagnostics” is shown with ready status.
- Enable idle backfill.
- Confirm coverage progress increases for files not manually refreshed.
- Disable Rust.
- Confirm rust-analyzer process stops and dashboard status changes to disabled.
- Repeat one refresh in a TypeScript or Python project when the matching language server is installed.
- Add a local custom adapter entry for a non-built-in language and confirm it appears in the engine table.

## Phase 2 Design, Not Implemented

Phase 2 can reuse the broker cache and settings, but must be a separate implementation step.

Possible Phase 2 additions:

- Edit hooks enqueue language/file refreshes after code changes.
- Prompt/session hooks inject a compact diagnostic summary into model context.
- MCP `tracedecay_diagnostics` accepts `mode: "cached" | "fresh" | "wait"`.
- Tool hints can suggest Code Diagnostics when a user asks about type errors.

Phase 1 must not wire any of these into hooks or model-facing output.

## Open Decisions Resolved

- Dashboard placement: a dedicated Code Diagnostics surface, not the existing Savings diagnostics panel.
- Runtime model: TraceDecay-owned LSP clients, not editor-owned LSP reuse.
- Refresh model: explicit dashboard refresh plus dashboard-owned idle whole-project backfill.
- Per-language control: dashboard enable/disable persisted per active project store.
- Batch fallback: explicit MCP/manual fallback only, not automatic dashboard fallback.

# Runtime blocking-I/O audit

Synchronous work reachable from an `async fn` on the Tokio runtime, without
`spawn_blocking` or `block_in_place`, in the daemon's request surfaces: MCP
dispatch, hook-runtime admission and ingest, LSP, and the HTTP dashboard.

## Why this audit exists

On a real macOS profile the daemon's unix accept loop starved because every
runtime worker sat in synchronous `gix::discover` and loose-ref `open()` inside
connection admission. The defect is not the limit that fired; it is that an
unbounded synchronous span ran on a worker that the runtime needed for
everything else. `tokio::time::timeout` can only preempt at an await point, so
an inline blocking span also makes the carried request deadline unenforceable.

This audit covers the same defect class in the rest of the workspace. The
admission path itself (`daemon/core_admission.rs`, `daemon/project_routing.rs`,
`daemon/project_open_orchestration.rs`) is owned elsewhere and is out of scope
here.

## Severity model

Blast radius, not site count:

- **per-connection / per-frame** — runs on every accepted connection, hook
  event, or ingested session frame. One slow span multiplied by ingest
  concurrency takes every worker.
- **per-request** — runs once per invocation or serving read.
- **per-open** — runs once per project or store open, then caches.

Span cost matters as much as frequency. An unbounded traversal (repository
open, loose refs, the Git index, a workspace walk) is starvation-class. A
single bounded syscall (`canonicalize`, one `stat`, a small JSON write) is
real but not starvation-class, and moving it off the worker costs a task hop
per call.

## Audit table

| Site | Async path | Reachability | Radius | Disposition |
|---|---|---|---|---|
| `tracedecay-sessions/src/observation.rs` `prepare_capture` | `capture_observation` → `prepare_capture` → `prepare_capture_with_sanitizer` → `RepositoryProvenanceAdmissionContext::capture_after_sanitization` → `capture_snapshot` → `capture_snapshot_uncached` → `capture_repository_provenance` → `gix::open` / `gix::discover`, loose refs, Git index | Confirmed. `capture_observation` awaits the sync prepare inline; the sibling `prepare_batch_captures` already offloads the identical call | per-frame (cursor JSONL loop, composer, Claude, Hermes, snapshot) | **Fixed** — offloaded to `spawn_blocking` under the existing background-CPU permit, matching the batch path |
| `tracedecay-mcp/src/handlers/git/shell.rs` `open_project_repository` + `git_*_controlled` | `handle_diff_context`, `handle_changelog`, `handle_commit_context`, `handle_pr_context` | Confirmed off-runtime | per-tool-call | **Not a problem** — every caller runs inside `blocking_git_span` / `blocking_git_span_controlled`, which carries request cancellation and the dispatch deadline |
| `tracedecay-lsp/src/compile_diagnostics/fingerprint.rs` `workspace_diagnostics_input_paths` (`walkdir` over the project root) | `DiagnosticsFingerprint::capture` → `diagnostics_input_paths` | Confirmed off-runtime | per-diagnostics-request | **Not a problem** — `capture` already wraps the whole walk in `spawn_blocking` |
| `tracedecay-code-index/src/source_walk.rs` `source_walk` (`ignore::WalkBuilder` over the whole project) → `search_tree_with_cancel`, `search_tree_scoped_with_cancel` | `tracedecay_grep` and `tracedecay_ast_grep_search` handlers, and the application lexical/AST grep primitives | Confirmed off-runtime | per-tool-call | **Not a problem** — every caller goes through `run_bounded_search`, which combines a concurrency semaphore, a scan deadline, `spawn_blocking`, and cancel-on-drop |
| `tracedecay-code-index-runtime/.../freshness_witness.rs` `worktree_stat_sweep` (`gix::open` + gix status classification + an O(candidates) stat walk) and `tracked_files_match_after_clean_filters` (file reads through the gix filter pipeline) | `freshness_probe_verdict`, `request_fresh_for_query_background`, retained reconcile | Confirmed off-runtime | per-serving-read | **Not a problem** — every serving caller runs inside `spawn_blocking`. The comment at `registry/serving_reads.rs:585` records the incident that moved the remedy to the background worker: a live `tracedecay_context` call sat on an inline `ensure_fresh_for_query` for 900 seconds. `ensure_fresh_for_query` is now `#[cfg(test)]` only |
| `tracedecay-code-index-runtime/.../reconcile.rs` `git_authority_available` (`gix::open`) | Serving reads, semantic-evaluation generation reads | Confirmed off-runtime | per-serving-read | **Not a problem** — the deliberate O(1) fail-closed authority probe, and it too runs inside the serving `spawn_blocking`. Serving retained bytes under an unconfirmable identity is the one thing the old inline reconcile fail-closed on, so the probe stays |
| `tracedecay-lcm/src/compression.rs` `compress` and friends | LCM maintenance | Confirmed | background | **Not a problem** — LCM "compression" is message compaction over the store, not byte codec work; no MB-scale encode/decode runs on a worker |
| `tracedecay-session-memory/src/fact_store/commit_barrier.rs` `wait_after_durable_fact_commit` (`fs::rename`, `fs::write`) | Durable fact commit | Confirmed | test-only | **Not a problem** — compiled only under `test-transport`; two tiny FS ops, and the park itself is asynchronous |
| `tracedecay-daemon-service/src/invocation/dispatch.rs:227` `root.canonicalize()` | `DaemonInvocationDispatch::dispatch` | Confirmed inline | per-request, miss path only (a pre-admitted lease skips it) | **Deferred** — one bounded `realpath`, not an unbounded traversal; a task hop per invocation is likely a net loss. Revisit if a profile shows it |
| `tracedecay-code-index-runtime/.../registry/serving_reads.rs:40,110,135,206` `project_root.canonicalize()` | `serving_code_scope`, `mounted_code_scope`, `latest_generation_id`, `diagnostics_change_generation` | Confirmed inline | per-serving-read | **Deferred** — same bounded-`realpath` reasoning. The right fix is to resolve identity once at mount and key the map on it, which is a routing change, not an offload |
| `tracedecay-daemon-service/src/invocation/lsp.rs:101` `project_root.canonicalize()` | `lsp_owner` | Confirmed inline, only after a registered-root miss | per-LSP-request, miss path | **Deferred** — bounded, and the hit path never reaches it |
| `tracedecay-store-runtime/src/store_locator_resolver.rs:672,676` `fs::symlink_metadata` + `fs::canonicalize` | `LocalStoreRuntimeResolverV1::resolve` | Confirmed inline | per-store-open | **Deferred** — a bounded stat pair on a path that already caches per store |
| `tracedecay-agent-hosts/src/hooks/codex.rs:326,329,332` `read_to_string` + `create_dir_all` + `write` | `record_codex_subagent_start` | Confirmed inline | per Codex `SubagentStart` event | **Deferred** — a small per-session JSON counter file. Worth offloading if the counter map grows; it is not traversal-class today |
| `tracedecay-automation-runtime/src/automation/managed_skills.rs:55,61,66` `read_dir` + per-entry `read` | `migrate_managed_skill_routing`, `list_managed_skills`, `save_managed_skill` | Confirmed inline | per-automation-run | **Deferred** — automation runs as bounded background work, off the request path |
| `tracedecay-application/src/advisory/ci_runtime/stores.rs:663` `read_to_string` + `ContentDigest::of_bytes` | Advisory CI code-evidence hydration | Confirmed inline | per-failed-annotation | **Deferred** — one source file read plus its digest; batch it with the annotation loop if advisory hydration ever shows up in a profile |
| `tracedecay/src/mcp/server/requests.rs:878` `db_path.metadata()` | `read_resource_branches` | Confirmed inline | per-resource-read, one `stat` per branch | **Deferred** — bounded and proportional to branch count |
| `tracedecay/src/daemon/http_application_router.rs:80` `registered_root.canonicalize()` | Cold HTTP application router resolve | Confirmed inline | per-project, cached after the cold resolve | **Deferred** — runs once per project router, not per HTTP request |
| `tracedecay/src/daemon/core_admission.rs:712` `canonicalize` | Connection admission | Confirmed | per-connection | **Out of scope** — owned by the admission lane that is fixing the originating incident |

## Method

`spawn_blocking` or `block_in_place` already appears in 23 of the 52 crates, so
the workspace is broadly disciplined and a raw pattern grep is mostly noise
(2216 raw candidates, ~95% of them inline `#[cfg(test)]` modules, in-memory
`.metadata()` on generation manifests, or `tokio::process::Command`). Two
passes narrowed it:

1. A brace-tracking scan for blocking calls lexically inside `async fn` bodies
   and `async` blocks, excluding `spawn_blocking` / `block_in_place` closures
   and `#[cfg(test)]` modules — 82 production candidates.
2. `ripwire --callers` on each blocking leaf (`open_project_repository`,
   `diagnostics_input_paths`, `capture_repository_provenance`,
   `capture_snapshot`, `prepare_capture_with_sanitizer`, `worktree_stat_sweep`,
   `source_walk`) to walk the chain up to an async frame, since the incident's
   own span sat several sync hops below the async fn and no lexical scan can
   see that.

Every row above was confirmed by reading the async call chain, not by the scan
alone.

The outcome is worth stating plainly: the traversal-class spans in the request
surfaces are, with one exception, already behind a blocking lane, and several
carry comments describing the incident that put them there. The exception was
single-record observation preparation, whose own sibling batch path was already
offloading the identical call.

## Standing rule

A synchronous span whose cost scales with repository size, file count, or
payload size does not belong on a runtime worker. Route it through the blocking
lane its neighbours already use — `blocking_git_span_controlled` for gix work
under MCP git dispatch, `spawn_blocking` under the background-CPU permit for
observation preparation — rather than adding another executor abstraction.

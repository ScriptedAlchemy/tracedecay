# `tracedecay-agent-hosts` split seams

Catalog produced by the one-shot crate split (`docs/superpowers/plans/
2026-07-31-one-shot-crate-split.md`) when `src/agents/` and `src/automation/`
(~60K lines) moved into this crate, then updated by the fix-to-green pass.

The mover's 194 unresolved `crate::<root module>` paths are down to **28**. The
remainder are not this crate's to fix — each is blocked on a sibling that is
still red or on the root's own `src/application/` move. "Root wiring this crate
now owes" and "Still blocked" below are the two lists the landing needs.

## Why the two trees moved as one unit

They are mutually recursive: 364 `crate::agents::…` references live in
`automation/` and 158 `crate::automation::…` references live in `agents/`
(`agents::install_managed_skill_prompt_index` calls
`automation::skill_targets`; `automation::runner` drives host discovery through
`agents::host_bundle_registry`). Splitting them would need an upward
dependency, which the plan forbids. Because both modules kept their names at
this crate's root, all 522 of those intra-references still resolve verbatim —
zero rewrites were needed for the agents↔automation direction.

## Root compatibility shims

`src/agents.rs` and `src/automation.rs` in the root crate are
`pub use tracedecay_agent_hosts::{agents,automation}::*;`. A glob re-export
carries public modules as well as leaf items, so every previously public path
(`tracedecay::agents::claude::…`, `tracedecay::automation::runner::…`) still
resolves.

A glob cannot carry non-`pub` items, so the 15 declarations below — all named
by root call sites — were promoted from `pub(crate)`/`pub(super)` to `pub`.
This is the only visibility change the move made; it widens this crate's API
surface by exactly the set the root crate already depended on.

| Item | Root call sites |
|---|---|
| `agents::CLI_FALLBACK_PROMPT_RULES` | `src/hooks/steering.rs:141` |
| `agents::context_scout_owner::lookup_registered_context_scout_owners` | `src/daemon/service/invocation.rs:2862` |
| `agents::cursor::cursor_plugin_install_dir` | `src/hooks/memory_inject.rs` (4 sites) |
| `agents::hermes::read_config_pinned_project_root` (+ its `hermes/profile_config.rs` definition) | `src/migrate/hermes.rs`, `src/migrate/hermes/resolution.rs`, `src/sessions/hermes/ingest.rs` |
| `automation::config_error` | `src/mcp/tools/handlers/hook_runtime.rs:8` |
| `automation::run_ledger::read_published_artifact_manifest` | `src/dashboard/automation_run_api.rs:19` |
| `automation::runner::registered_project_automation_retrieval` (+ its `runner/retrieval.rs` definition) | `src/daemon/scheduler.rs:1260,1475` |
| `automation::runner::run_user_session_automation_with_backend` | `src/mcp/tools/handlers/hook_runtime.rs:2020` |
| `automation::scheduler::load_session_activity` | `src/dashboard/automation_scheduler_api.rs:15`, `src/application/host_admission.rs:2578` |
| `automation::skill_usage::analytics_import_key_for_request` (+ its `skill_usage/analytics.rs` definition) | `src/mcp/tools/handlers/skills.rs:16` |
| `automation::skill_usage::ingest_project_analytics_events` (+ its `skill_usage/analytics.rs` definition) | `src/dashboard/automation_skills_api.rs:19`, `src/mcp/tools/handlers/skills.rs:16` |

## Embedded assets (all verified resolving)

Every `include_bytes!`/`include_str!` literal was rebased for the new depth and
then checked against the filesystem: 21 direct literals plus the 47 sources the
`plugin_file!` macro concatenates all resolve.

| Site | Old prefix | New prefix |
|---|---|---|
| `agents/host_bundle_v2.rs` (7 packaged host events + 1 transcript-golden manifest) | `../../tests/` | `../../../../tests/` |
| `agents/context_scout_ports.rs:1059` (Claude post-tool-use fixture) | `../../tests/` | `../../../../tests/` |
| `agents/opencode.rs:26`, `agents/host_bundle_registry.rs:634,638`, `agents/plugin_bundle.rs:111,378` | `../../plugin/` | `../../../../plugin/` |
| `agents/hermes/dashboard_wrapper.rs:24,26,28` | `../../../dashboard/` | `../../../../../dashboard/` |
| `agents/hermes/templates.rs:309,327,329` | `templates/…` (sibling-relative) | unchanged |

`agents/plugin_bundle.rs:117` includes `$OUT_DIR/plugin_bundle_generated.rs`.
That generator moved out of the root `build.rs` (its former
`is_probably_utf8_text` / `append_plugin_files` / `CanonicalAgent` /
`parse_agent_source` / `quoted_string` / `append_generated_plugin_files` /
`generate_plugin_bundle` block, plus the `main()` call) into
`crates/tracedecay-agent-hosts/build.rs`. `collect_files_relative` stays in the
root `build.rs` because the dashboard manifest still uses it — it is the one
duplicated helper.

`build.rs` also bakes `TRACEDECAY_GIT_SHA`, which
`agents/hermes/templates.rs` stamps into every generated Hermes plugin file as
a provenance header. `env!` resolves per compiled crate, so the root build
script's stamp is not visible here. Rather than a second probe, this script
`#[path]`-includes the same `src/version/build_identity.rs` the root does, and
points it at the repository root — a crate subdirectory is never its own git
worktree top level, so `resolve` would otherwise report an empty identity by
design. Verified: the emitted stamp tracks HEAD.

The generator's paths are rebased the same way: `plugin_root` is
`CARGO_MANIFEST_DIR/../../plugin`, the `rerun-if-changed` watches are
`../../plugin/…`, and the `include_str!` it emits is
`concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugin/…")`. Verified: the
generated manifest carries 4 `GENERATED_*` consts and 50 `PluginFile` entries
with the rebased prefix.

### Packaging seam (open)

Those include paths now escape the crate directory. `cargo package` only ships
files under the package root, so publishing `tracedecay-agent-hosts`
standalone would lose `plugin/`, `tests/fixtures/packaged_host_events/`,
`tests/fixtures/transcript_golden/cline_like/`, and
`dashboard/hermes-wrapper/`. The root `Cargo.toml` `include` list still names
all four (lines ~77–113) because the root package used to own them. Deciding
between (a) making this crate `publish = false`, (b) copying the trees into the
crate, or (c) having `build.rs` stage them into `OUT_DIR` is an aftermath-queue
item, not a mover decision.

## Root-module couplings

The mover left **194** unresolved `crate::<root module>` paths. The fix-to-green
pass took that to **28**, all of them in `automation/` and all blocked on other
movers (see "Still blocked" below). Trajectory:

| After | Errors | What changed |
|---|---|---|
| (mover) | 194 | — |
| kernel repoint | 69 | `tracedecay-runtime-core` aliased into this crate's root |
| agents-side ports | 45 | `ports::{context,configuration,mcp_tools,hook_runtime,pricing}`, `TRACEDECAY_GIT_SHA` |
| analytics + user config | 37 | `analytics` moved down; profile-config seams narrowed |
| session evidence | 32 | `ports::{session_evidence,codex_app_server}` |
| profile session store | 28 | `ports::session_store`; hook receipts repointed |

### Dependencies added

Exactly one: `tracedecay-runtime-core`. Verified acyclic with `cargo metadata`
— nothing this crate depends on depends back on it, and in particular no
`agent-hosts ↔ dashboard-api` edge exists in either direction.

`tracedecay-sessions` and `tracedecay-global-db` were **deliberately not**
added even though the plan's target map points at them: both are still red
(220 and 279 errors, the latter through `tracedecay-migrate`), so an edge would
make this crate's own gate unsatisfiable. The seams that would have used them
are ports instead, and each row below names the edge that should replace the
port once the sibling is green.

### Kernel repoint

`src/lib.rs` aliases `tracedecay_runtime_core::{branch, config, db, errors,
lifecycle_lease, memory, privacy, runtime_identity, serde_util, storage, store,
timeutil, worktree}` into this crate's root, so every historical
`crate::<module>::…` path in the moved code resolves verbatim. This is the same
shape the root crate uses on its side of the split (`src/errors.rs` et al.), and
it collapsed 125 of the 194 errors on its own. `crate::tracedecay` is a
hand-written module carrying only the kernel's `current_timestamp`; the
`TraceDecay` façade itself is a port row below.

## Root wiring this crate still owes

Everything below is **required before the root crate compiles against this
one**. Registered ports degrade to a documented inert answer when unregistered,
so this crate's own gate passes without them — but the product does not behave
correctly until they are wired.

### Registered ports (root calls `register` at startup)

The MCP tool catalog and format-capable name ports are wired by
`src/agents.rs::register_mcp_tool_catalog_ports`, which the product binary calls
before command dispatch.

| Port | Root registers | Inert answer if unwired |
|---|---|---|
| `ports::hook_runtime::register_daemon_tool_invoker` | `hooks::daemon_tool_json` | daemon reported unavailable |
| `ports::hook_runtime::register_memory_injection_gate` | `hooks::memory_inject::memory_injection_enabled` | injection disabled |
| `ports::hook_runtime::register_cursor_catch_up_ingest_max_bytes` | `hooks::CURSOR_CATCH_UP_INGEST_MAX_BYTES` | `u64::MAX` → doctor stays silent |
| `ports::pricing::register` | `accounting::pricing::cost_of_turn` | `0.0` (same as an unpriced model) |
| `ports::codex_app_server::register` | adapter over `sessions::codex_app_server::run_prompt_with_codex_app_server` | backend unavailable |
| `ports::session_store::register_canonical_project_key` | `RegisteredGlobalDb::canonical_project_key` | lossy path string |

### Implemented ports (root `impl`s the trait)

| Trait | Root implements for |
|---|---|
| `agents::InstalledAgentsConfig` | `user_config::UserConfig` |
| `ports::session_store::AutomationSessionStore` | `global_db::RegisteredGlobalDb` (converting the analytics query/record field-for-field) |

### Downward moves (root deletes its copy and re-exports)

These are **not** ports: the value's identity must be shared across the
boundary, so two definitions would be a correctness bug, not just duplication.
Until each row lands, the definition is duplicated in both crates.

| Moved here | Root shim to write |
|---|---|
| `agents::cursor_diagnostics::DEGRADED_SERVE_STDERR_MARKER` | `src/serve.rs` re-exports |

### Canonical compatibility shims

| Historical path retained here | Canonical owner |
|---|---|
| `ports::context::{CancellationToken, MonotonicDeadline}` | `tracedecay_runtime_core::cancellation` |
| `ports::configuration::ConfigurationCurrentStateV1` | `tracedecay_usecases::configuration` |

### Conversions at the boundary

| This crate's type | Root converts from |
|---|---|
| `ports::session_evidence::{LcmScope, LcmGrepSort, LcmGrepHit}` | `sessions::lcm::{…}` — identical serde shapes, field-for-field |
| `ports::codex_app_server::{SummaryConfig, Summary}` | `sessions::codex_app_server::{CodexAppServerSummaryConfig, CodexAppServerSummary}` |
| `ports::session_store::{AnalyticsEventQuery, AnalyticsEventRecord}` | `global_db::{…}` |
| `agents::cursor::BranchAddOutcome` (private) | decoded from the `tracedecay_admin_branch_add` JSON `outcome` string; the root's `branch::BranchAddOutcome` is the producer. The **wire strings are the contract** — the two enums must keep the same variant set. |

## Still blocked (28 errors, 8 files)

These are not this crate's to fix. Every remaining error is one of four
clusters, and each resolves when another mover lands.

| Cluster | Files | Blocked on |
|---|---|---|
| `crate::application::{memory, session, context}` | `fact_proposals`, `memory_digest`, `outcomes`, `session_reflector`, `staged_notice`, `runner`, `runner::retrieval`, `runner::session_reflector`, `memory_curator` | The root's `src/application/` tree never moved into `tracedecay-application` — the two are **divergent parallel trees**, not a shim. `MemoryApplication`, `SessionRetrievalService`, `RequestBudgets`, `ResolvedSessionIdentity` and friends exist only in the root. |
| `crate::global_db::{RegisteredGlobalDb, session_temporal::…}` | `lifecycle`, `memory_curator`, `runner`, `runner::retrieval`, `runner::session_reflector` | `tracedecay-global-db` is red (through `tracedecay-migrate`, 279 errors). `ports::session_store` already covers the reads; these sites additionally need `Arc<RegisteredGlobalDb>` as a constructed handle. |
| `crate::daemon::{profile_identity, store_runtime::session_registry}` | `memory_curator`, `runner`, `runner::retrieval`, `runner::session_reflector`, `lifecycle` | `tracedecay-daemon` does not exist yet. |
| `crate::tracedecay::TraceDecay`, `crate::dashboard::memory_curate`, `crate::request_identity`, `crate::memory::user::open_user_memory_db` | `memory_curator`, `runner`, `runner::retrieval`, `runner::session_reflector` | Root façade + `tracedecay-dashboard-api` (red, 210 errors). |

**Sequencing note.** The `application` cluster is the true critical path: five
of the eight blocked files (`fact_proposals`, `memory_digest`, `outcomes`,
`session_reflector`, `staged_notice`) are blocked on `application::memory`
*alone*, so moving `src/application/memory` into `tracedecay-application` — or
inverting `MemoryApplication` into a `FactProposalMemory` port here — clears
them in one step. The other three files (`runner`, `runner::retrieval`,
`runner::session_reflector`, `memory_curator`) need all four clusters at once
and should be sequenced last.

Inverting `application::session` from *below* is the wrong shape and was
deliberately not attempted: `runner::retrieval` imports twelve types from the
root's request-context and session-retrieval layers, so a port would mean
re-declaring that entire layer inside this crate and undoing it when
`src/application/**` completes its own move.

### Corrections to this catalog

- **`crate::dashboard::assets` is not a code coupling.** The one occurrence
  (`agents/hermes/dashboard_wrapper.rs:13`) is prose inside a `//!` doc comment
  describing where the embedded dashboard build comes from. It never failed to
  compile, so no `DashboardAssetSource` trait was defined — an unimplemented,
  uncalled trait would be dead code. The real `crate::dashboard` coupling is
  `dashboard::memory_curate` in `automation/memory_curator.rs`, which now lives
  in `tracedecay-dashboard-api` (red) and is listed as blocked above.
- The mover's count of 192 was 194 as measured; the two extra were the
  `TRACEDECAY_GIT_SHA` `env!` failures in `agents/hermes/templates.rs`, now
  fixed by this crate's own build script (below).

## Root-crate references that name the old file paths

Not code couplings, but they break with the move and are outside this crate:

- `tests/agent_suite/{agent_misc_test,cli_args_contract_test,agent_hermes_test,
  plugin_skill_contract_test,claude_plugin_bundle_test}.rs` and
  `tests/hermes_suite/lcm_bridge.rs` read or `include_str!` files under
  `../../src/agents/…`; they now need `../../crates/tracedecay-agent-hosts/src/
  agents/…`.
- `tests/request_context_boundary.rs:59` lists `"src/automation"` as a scanned
  boundary directory.
- `CONTRIBUTING.md:130` and `eval/hermetic/README.md` name `src/agents/`.
- `dashboard/hermes-wrapper/plugin_api.py:68` and
  `dashboard/stories/fixtures/data.ts` carry stale `src/agents|automation`
  strings (already stale in places — they name files that no longer exist).

## Feature forwarding

This crate declares `token-counting` (→ `dep:tiktoken-rs`) and `test-transport`
to match the gates inside the moved code. The root `Cargo.toml` forwards both
(`token-counting = [..., "tracedecay-agent-hosts/token-counting"]`,
`test-transport = [..., "tracedecay-agent-hosts/test-transport"]`), so
`--features production` and `--all-features` keep the same shape.
`test-transport` here is a bare marker: the moved code only reads
`cfg(feature = "test-transport")` and does not re-forward it to
`tracedecay-rusqlite-runtime`, which the root still owns.

# `tracedecay-agent-hosts` split seams

Catalog produced by the one-shot crate split (`docs/superpowers/plans/
2026-07-31-one-shot-crate-split.md`) when `src/agents/` and `src/automation/`
(~60K lines) moved into this crate. Per the plan's execution model this crate
does **not** compile on its own yet: the couplings below resolve only once the
sibling movers (`tracedecay-runtime-core`, `tracedecay-daemon`,
`tracedecay-mcp`, `tracedecay-sessions`, `tracedecay-global-db`,
`tracedecay-dashboard-api`) land. Everything here is input to the lead's
fix-to-green campaign, not work this mover left half-done.

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

## Root-module couplings (the compile blockers)

`cargo check -p tracedecay-agent-hosts` reports **192 errors**, every one an
unresolved `crate::<root module>` path (107 × E0433, 85 × E0432). No unlinked
external crates and no include failures remain. Mapping to the plan's target
map:

| `crate::…` | Refs | Lands in |
|---|---|---|
| `errors` | 55 files | `tracedecay-runtime-core` |
| `storage`, `db`, `store`, `memory`, `branch`, `privacy`, `timeutil`, `serde_util`, `lifecycle_lease`, `worktree`, `runtime_identity` | 9/13/6/8/1/2/1/1/1/2 files | `tracedecay-runtime-core` |
| `application` | 16 files | `tracedecay-application` / `tracedecay-api` |
| `daemon` | 8 files | `tracedecay-daemon` |
| `sessions` | 6 files | `tracedecay-sessions` |
| `global_db` | 8 files | `tracedecay-global-db` |
| `mcp` | 3 files | `tracedecay-mcp` |
| `hooks` | 2 files | `tracedecay-hooks` |
| `dashboard` | 2 files | `tracedecay-dashboard-api` (`dashboard::assets` stays root) |
| `tracedecay`, `config`, `user_config`, `analytics`, `accounting`, `serve`, `request_identity` | 15/3/2/3/1/1/1 files | root-retained; need a downward move or an injected port |

Detailed per-module path lists (every distinct `crate::…` path and the files
reaching it) are reproducible with:

```
rg -o 'crate::[a-z_][a-z0-9_]*(::[A-Za-z_][A-Za-z0-9_]*)*' crates/tracedecay-agent-hosts/src
```

Notable shapes the fix campaign should expect:

- `crate::errors::{Result, TraceDecayError, TraceDecayError::Config}` — the
  single widest seam (55 of this crate's files). One kernel import fixes it.
- `crate::tracedecay::current_timestamp` (13 sites) and
  `crate::tracedecay::TraceDecay` — the root façade type. These are the only
  couplings that point *up* at the root binary crate's own façade rather than
  at a subsystem, so they need a port or a downward move of
  `current_timestamp`.
- `crate::dashboard::assets` (`agents/hermes/dashboard_wrapper.rs`) — the plan
  keeps `dashboard/assets.rs` in the root for its OUT_DIR embed, so this one
  cannot be satisfied by `tracedecay-dashboard-api` and needs an inversion.
- `crate::serve::DEGRADED_SERVE_STDERR_MARKER`
  (`agents/cursor_diagnostics.rs`) — a lone const; move it down.
- `crate::hooks::{CURSOR_PLUGIN_SKILLS, CURSOR_CATCH_UP_INGEST_MAX_BYTES,
  daemon_tool_json, run_with_test_env_lock, memory_inject::…}` — `hooks/`
  daemon-side handlers stay root per the plan, so these need the constants
  moved into `tracedecay-hooks` or a port.

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

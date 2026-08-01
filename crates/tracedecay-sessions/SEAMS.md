# tracedecay-sessions root seams

The one-shot crate split moved the whole former `src/sessions/` tree into
`crates/tracedecay-sessions/src/runtime/` without resolving references back
into the root binary crate. That aftermath is now **closed for the library
target**:

```
cargo check -p tracedecay-sessions --all-features   # 0 errors
cargo check -p tracedecay-sessions                  # 0 errors
```

Every `crate::<root module>` path that used to appear here is gone from the
compiled surface. What remains is the **root wiring list** below: work the
composition root must do so the two crates agree again, plus the `#[cfg(test)]`
seams that `cargo check` does not compile.

## How the seams were closed

| Former seam | Resolution |
| --- | --- |
| `crate::{db, errors, privacy, storage, worktree, git, timeutil, serde_util, os_str_bytes, text, sqlite_read_snapshot, tracedecay}` | Repointed at `tracedecay_runtime_core::<module>` (new dependency). |
| `crate::config::{discover_project_root, has_project_database}` | Repointed at `tracedecay_runtime_core::config`. |
| `crate::application::host_admission` (disposition values, wire helpers, bounds) | Moved down into `crate::admission`. |
| `crate::application::host_admission::HostAdmissionFacade` | Inverted behind the dyn-safe `crate::admission::HostAdmission` port. |
| `crate::application::observation` | Moved down into `crate::observation`. |
| `crate::repository_provenance` | Moved down into `crate::repository_provenance`. |
| `crate::application::session::compatibility` | Moved down into `crate::compatibility`. |
| `crate::application::session::lcm::contracts` | Repointed at `crate::lcm::contracts` (root already re-exported from here). |
| `crate::store::TranscriptIngestStore` | Moved down into `crate::runtime::store_port`. |
| `crate::store::GlobalDbGitCorrelationStore` | Inverted behind `runtime::git_correlation::GitCorrelationSessionStore`. |
| `crate::store::GlobalDbWorkflowStore` | Inverted behind `runtime::workflow_index::WorkflowIngestSink`. |
| `crate::store::GlobalDbTranscriptStore`, `crate::global_db::RegisteredGlobalDb` (ingest) | Inverted behind `runtime::ingest::SessionIngestAuthority`. |
| `crate::global_db::RegisteredGlobalDb` (backfill) | Inverted behind `runtime::store_port::SessionStoreAuthority`. |
| `crate::agents::{vscode_data_dir, kiro_data_dir}` | Owned as pure layout helpers in `crate::host_ports`. |
| `crate::accounting::parser::parse_timestamp` | Owned as `crate::host_ports::parse_timestamp` over the kernel RFC 3339 parser. |
| `crate::context::read_cache::digest_bytes` | Owned as a private helper in `runtime::git_correlation`. |
| `crate::agents::hermes::read_config_pinned_project_root` | Inverted behind `host_ports::hermes_profile_pin`. |
| `crate::hooks::schedule_user_session_review` | Inverted behind `host_ports::session_review`. |
| `crate::user_config::UserConfig` (LCM redaction) | Inverted behind `host_ports::lcm_redaction`. |
| `HostAdmissionAuthorities::unregistered_*` | Inverted behind `host_ports::unregistered_admission`. |

### Why `tracedecay-global-db` is *not* a dependency

`crate::global_db::RegisteredGlobalDb` was the obvious candidate for a plain
`tracedecay-global-db` dependency edge. That crate does not currently build:
`tracedecay-migrate`'s `root_seam` module is deliberately empty pending its own
landing, so `cargo check -p tracedecay-global-db` fails with 279 errors inside
`tracedecay-migrate`. Adding the edge would make this crate uncheckable, so the
registered database is inverted behind `SessionStoreAuthority` and
`SessionIngestAuthority` instead. Those ports are strictly narrower than the
concrete type and can stay after `tracedecay-global-db` is fixed.

## Root wiring needed

Nothing below is optional: without it the root crate cannot re-wire to this
crate.

### 1. Drop the duplicated definitions and re-export from here

The moved-down modules were **copied**, not deleted from the root, because the
lane that produced them owns only `crates/tracedecay-sessions/**`. The root
still has its own definitions; the two will be nominally distinct types until
the root deletes its copies and re-exports:

| Root file | Action |
| --- | --- |
| `src/application/observation.rs` | Delete; `pub use tracedecay_sessions::observation::*;` (keep `observation_test.rs` against the re-export). |
| `src/repository_provenance.rs` | Delete; `pub use tracedecay_sessions::repository_provenance::*;`. |
| `src/application/session/compatibility.rs` | Delete; `pub use tracedecay_sessions::compatibility::*;`. |
| `src/application/host_admission/disposition.rs` | Delete; re-export `tracedecay_sessions::admission::{HostAdmissionStatus, HostAdmissionTelemetryDisposition, HostAdmissionDispositionClass, is_bounded_reason_code}`. |
| `src/application/host_admission/wire.rs` | Delete; re-export `tracedecay_sessions::admission::wire::*`. |
| `src/application/host_admission/spool/bounds.rs` | Keep `SpoolBounds`; take the `DEFAULT_MAX_*` consts from `tracedecay_sessions::admission::bounds`. |
| `src/application/host_admission.rs` | Take `HostAdmissionOutcome`, `HostProjectionDrainOutcome`, `HostAdmissionScope` from `tracedecay_sessions::admission`; delete the local copies and the two `*AdmissionPort` traits. |
| `src/store/mod.rs` | Delete the local `TranscriptIngestStore` trait; `pub use tracedecay_sessions::runtime::store_port::TranscriptIngestStore;`. |

### 2. Implement the inverted ports

| Port | Implement for |
| --- | --- |
| `tracedecay_sessions::admission::HostAdmission` | `HostAdmissionFacade<'_>` — one boxed-future forward per method, including `has_session_message`. |
| `runtime::store_port::SessionWriteTxn` | `RegisteredGlobalDbWriteTransaction<'_>` (`commit`, `rollback`). |
| `runtime::store_port::SessionStoreAuthority` | `RegisteredGlobalDb` (`shard_id`, `db_path`, `read_snapshot`, `begin_write_transaction`). |
| `runtime::git_correlation::GitCorrelationSessionStore` | `GlobalDbGitCorrelationStore<'_>` in `src/store/git_correlation.rs`. |
| `runtime::workflow_index::WorkflowIngestSink` | `GlobalDbWorkflowStore<'_>` in `src/store/workflow.rs`. |
| `runtime::ingest::SessionIngestAuthority` | `RegisteredGlobalDb`, composing the `src/store/` adapters and `HostAdmissionAuthorities::{for_project, for_profile}`. Its `registered_project_roots` folds `try_list_project_paths`, `try_list_code_project_paths(usize::MAX)`, and `try_list_project_alias_paths`. |

### 3. Register the process-global host ports at startup

`crate::host_ports` slots default to "do nothing" so an unwired process still
runs. Every one of these must be installed before transcript ingest:

| Slot | Root implementation |
| --- | --- |
| `host_ports::hermes_profile_pin::register` | `tracedecay_agent_hosts::agents::hermes::read_config_pinned_project_root` |
| `host_ports::session_review::register` | `crate::hooks::schedule_user_session_review` |
| `host_ports::lcm_redaction::register` | Read `UserConfig::load()` into `LcmRedactionPolicy { enabled: lcm_sensitive_redaction_enabled, patterns: lcm_sensitive_redaction_patterns }` |
| `host_ports::unregistered_admission::register` | `HostAdmissionFacade::new(HostAdmissionAuthorities::unregistered_for_{project,profile}(..))` boxed |

Until `unregistered_admission` is registered, the two standalone Codex entry
points (`try_admit_codex_jsonl_observations_for_{project,profile}`) return an
empty progress instead of walking the rollout.

### 4. Re-home the composition-root modules staged under `root-wiring/`

Three modules were pure composition-root code: they build registered databases,
daemon runtime registries, and root application services. They are preserved
verbatim under `crates/tracedecay-sessions/root-wiring/` (not part of any
target) and must move into `src/` in the root crate:

| Staged file | Notes |
| --- | --- |
| `root-wiring/session_temporal_benchmark.rs` | 1500 lines. Needs root `application::{context, session}`, `daemon::{profile_identity, store_runtime::session_registry}`, `config::lock_user_data_dir_test_env`, `RegisteredGlobalDb`, `GlobalDbSessionTemporalStore`. `benches/session_temporal.rs` and `tests/session_suite/temporal_benchmark.rs` reach it through `tracedecay::sessions::session_temporal_benchmark`, so the root module must keep that path. |
| `root-wiring/transcript_backfill_test_runtimes.rs` | `TranscriptFactsBackfillTestRuntimeV1` and `StructuredBackfillTestRuntimeV1`, the two `#[doc(hidden)]` registered-`ProjectSessions` fixtures. They wrap `HostAdmissionTestRuntimeV1` and call the (now generic) `runtime::transcript_backfill` entry points. |

### 5. Layout helpers that now exist twice

`host_ports::{vscode_data_dir, kiro_data_dir}` duplicate
`tracedecay_agent_hosts::agents::{vscode_data_dir, kiro_data_dir}`. The
duplication is deliberate — `tracedecay-agent-hosts` does not currently build
(194 errors, its own lane) and the VS Code/Kiro layout is fixed by those
products — but the two should converge on one owner once that crate lands.

## Remaining `#[cfg(test)]` seams

`cargo check` does not compile these, so they do not block the crate. They will
break `cargo test -p tracedecay-sessions` until the root wiring above lands and
the fixtures move.

| File | Root path still referenced |
| --- | --- |
| `src/runtime/lcm/dashboard_fixes_tests.rs` | `crate::application::configuration::ProductionUserSettingsDaemonClient`, `crate::daemon::{profile_identity::load_or_create, store_runtime::session_registry::DaemonSessionRuntimeRegistryV1}`, `crate::dashboard::scope::resolve_dashboard_scope`, `crate::config::RetentionConfig`, `crate::admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1}`, `RegisteredGlobalDb` |
| `src/runtime/workflow_ingest/tests.rs` | `crate::daemon::{profile_identity::load_or_create, store_runtime::session_registry::DaemonSessionRuntimeRegistryV1}`, `RegisteredGlobalDb`, `GlobalDbWorkflowStore` |
| `src/runtime/claude_observation.rs` (`mod tests`) | `crate::config::PinnedUserDataDir`, `crate::admission::HostAdmissionTestRuntimeV1`, `RegisteredGlobalDb`; also defines two spies against the retired `*AdmissionPort` trait pair that should collapse into one `impl HostAdmission`. |
| `src/runtime/claude_observation_benchmark/baseline.rs` | `crate::hooks::…` |
| `src/runtime/lcm/raw.rs` (`mod ingest_protection_defaults_tests`) | `crate::user_config::UserConfig` — should build `host_ports::LcmRedactionPolicy` directly and call `IngestProtectionDefaults::from_policy`. |
| `src/runtime/{cline_like,kiro,cursor,cursor_composer,hermes,ingest,transcript_backfill}` test modules | `crate::admission::HostAdmissionTestRuntimeV1` and `RegisteredGlobalDb` fixtures |

## Non-code references to the old path

These name `src/sessions/…` as data or prose and were deliberately left alone;
the benchmark manifests additionally pin SHA-256 digests over the harness files,
so they need a re-seal rather than a path edit.

- `benchmarks/pr5-observation/workload-v1.json` and
  `benchmarks/pr5-observation/result-2026-07-26-dc17dd73.json`
- `benchmarks/pr8-temporal/workload-v1.json` and
  `benchmarks/pr8-temporal/result-provisional.json`
- `tests/fixtures/transcript_golden/cline_like/manifest.json` and
  `tests/fixtures/transcript_golden/cline_like/expected/parser_provenance.json`
- `tests/fixtures/provider_normalization/codex/README.md`
- Twelve `docs/**` files (LCM, memory, and plan documents) that cite the old
  module paths in prose.

The `include_str!`/`include_bytes!` fixture paths inside the moved code were
repointed at the repo root and do resolve for workspace builds. They now escape
the crate directory, so `cargo package`/`cargo publish` for this crate cannot see
them; the fixtures need to move under `crates/tracedecay-sessions/tests/` before
the crate is publishable.

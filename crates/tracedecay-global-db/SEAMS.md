# `tracedecay-global-db` root seams

The one-shot crate split moved `src/global_db{.rs,/}` into
`crates/tracedecay-global-db` as-is, leaving 601 references spelled as
root-crate paths (`crate::db::…`, `crate::errors::…`, `crate::sessions::…`, …).
That aftermath is now **closed for the library target**:

```
cargo check -p tracedecay-global-db                  # 0 errors
cargo check -p tracedecay-global-db --all-features   # 0 errors
```

Trajectory: **499 → 49 → 12 → 8 → 0**.

Not a single `crate::<root module>` path remains in the compiled surface. What
follows is the **root wiring list** — work the composition root must do so the
two sides agree again — plus the `#[cfg(test)]` seams `cargo check` does not
compile.

## Dependencies added

| Edge | Why | Cycle proof |
| --- | --- | --- |
| `tracedecay-runtime-core` | `db`, `errors`, `storage`, `config`, `worktree`, `lifecycle_lease`, `os_str_bytes`, `tracedecay`, `store_runtime` | `cargo tree -p tracedecay-runtime-core -e normal` contains no `tracedecay-global-db` |
| `tracedecay-sessions` | every `crate::sessions::…` site, plus `lcm::contracts` and `compatibility` | `cargo tree -p tracedecay-sessions -e normal` contains no `tracedecay-global-db` |
| `tracedecay-semantic` | `SemanticResourceCeilings`, `DEFAULT_FASTEMBED_MODEL_ID` behind the semantic setting | `cargo tree -p tracedecay-semantic -e normal` contains no `tracedecay-global-db` |

## How the seams were closed

| Former seam | Sites | Resolution |
| --- | ---: | --- |
| `crate::db::…` | 252 | Repointed at `tracedecay_runtime_core::db`. |
| `crate::errors::…` | 194 | Repointed at `tracedecay_runtime_core::errors`. |
| `crate::storage::…` | 13 | Repointed at `tracedecay_runtime_core::storage`. |
| `crate::{worktree, lifecycle_lease, os_str_bytes, tracedecay}::…` | 7 | Repointed at the matching `tracedecay_runtime_core` module. |
| `crate::config::user_data_dir` | 1 | Repointed at `tracedecay_runtime_core::config`. |
| `crate::sessions::…` | 51 | Repointed at `tracedecay_sessions::runtime::…` (the sessions mover nested the transcript runtime under `runtime/`). |
| `crate::sessions::workflow_index::WorkflowScopeFilter` (`lib.rs:55` re-export) | 1 | Repointed at `tracedecay_sessions::runtime::workflow_index`. |
| `crate::application::session::compatibility` | 9 | Repointed at `tracedecay_sessions::compatibility` (the sessions mover already owns that body). |
| `crate::application::session::lcm::contracts` | 5 | Repointed at `tracedecay_sessions::lcm::contracts` (root's file is a one-line re-export shim). |
| `crate::config::{SEMANTIC_RUNTIME_SETTING_KEY, DEFAULT_FASTEMBED_MODEL_ID, SemanticResourceCeilings}` | 10 | Repointed at `tracedecay_domain::configuration` / `tracedecay_semantic`, which already own them. |
| `crate::project_registry::{ReapEntryKind, RegistryReapEntry, RegistryReapPlan, RetainedRegistryEntry, alias_key_path, ephemeral_root_rejection}` | 4 | **Moved down** into `crate::project_registry`, beside `plan_registry_reap` — its only producer. |
| `crate::application::session::{ports, SessionDataFreshness}` | 6 | **Moved down** into `session_temporal::execution`; this crate holds the only `SessionTemporalExecutionPort` impl. |
| `crate::application::session::lcm::render` | 4 | **Moved down** into `session_temporal::render`; its only callers were the registered adapters here. |
| `crate::application::configuration::{types, ports}` | 3 | **Moved down** into `configuration::contracts`; this crate holds the only `ConfigurationControlStore` impl. |
| `crate::config::{registry, resolver, legacy_decoder}` | 10 | **Moved down** into `configuration::{registry, resolver, legacy_decoder}`; the configuration store is their only caller, and `legacy_decoder` already reached back into `crate::global_db::configuration::migration`. |
| `crate::config::SemanticConfig` | 4 | **Moved down** into `configuration::semantic`; the registry that defaults and validates the setting now lives here. |
| `crate::config::brand_env` | 2 | Owned as a private `lib.rs` helper — one `std::env::var` call with a branded prefix. |
| `crate::context::read_modes::estimate_tokens` | 2 | Owned as `crate::estimate_tokens`; `context::read_modes` is an MCP read handler that drags the whole root graph database. |
| `crate::application::{evidence_assembly, external_source_store}`, `crate::store::observation`, `crate::daemon::work_runtime` | 8 | **Inverted**: the four factories that returned root-owned adapters are gone; `RegisteredGlobalDb::{runtime, authority}` expose the ingredients so root builds what root owns. |
| `crate::retention::…` | 8 | **Inverted**: `prune_global_retention` / `global_retention_report` are gone; both were three lines over the public transaction API. |
| `crate::daemon::{store_runtime::session_registry, profile_identity}` | 4 | **Inverted** behind `host_ports::profile_sessions`. |
| `crate::doctor::heal` | 1 | Prose comment only; left as a cross-reference. |

### Visibility widened outside this crate

Pure widenings, no behavior change:

- `tracedecay_sessions::compatibility::{is_inventory_text, dedupe_related_message_copies,
  rerank_fetch_limit, RelatedMessageCopyIdentity}` — `pub(crate)` → `pub`. The
  registered message-search reader is the only new caller.

Inside this crate, the four `AuthorizedTemporalExecutionRequest` helpers root's
`session::retrieval` drives (`new`, `with_direct_anchor`, `into_kernel_request`,
`validates_report`) were `pub(crate)` in the root binary and are `pub` in the
moved copy, because the callers are now across a crate boundary.

## Root wiring needed

Nothing below is optional.

### 1. Install the profile-sessions opener

`host_ports::profile_sessions::register` must be called during daemon startup
(and by the root test harness) with an opener that performs
`daemon::profile_identity::load_or_create` and
`daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open`,
returning a handle whose `mount()` calls `profile_sessions()`. Unwired, the
registered test harnesses panic with the message in
`tests::harness::UNWIRED_PROFILE_SESSIONS`; nothing in production consumes the
port, because production callers already hold a registry.

Note the ordering change: the harness enters the daemon database scope *before*
opening the runtime, so the opener creates the profile identity inside the
scope rather than just ahead of it.

### 2. Build the root-owned adapters at the call site

These four `RegisteredGlobalDb` methods were deleted. Each is a one-liner over
the public accessors:

| Deleted method | Root replacement |
| --- | --- |
| `observation_store()` | `store::observation::GlobalDbObservationStore::with_runtime(registered.runtime(), registered.authority())` |
| `evidence_assembly_store()` | `application::evidence_assembly::RuntimeEvidenceAssemblyStore::new(registered.binding().shard_id.profile_id.clone(), registered.runtime().clone(), registered.authority().clone())` |
| `external_source_store()` | `application::external_source_store::RuntimeExternalSourceStore::new(registered.runtime().clone(), registered.authority().clone())` |
| `work_runtime(authority, config, digest, root)` | `daemon::work_runtime::DaemonWorkRuntimeV1::new(authority, registered.work_storage()?, config, digest, Arc::clone(&registered), root)` |

### 3. Drive retention from root

`prune_global_retention` / `global_retention_report` were deleted. Root wiring:

```rust
let tx = registered.begin_write_transaction().await?;
let report = retention::prune_global_tables(&tx, config, mode, now).await?;
tx.commit().await?;             // RetentionMode::Apply
// tx.rollback().await?;        // RetentionMode::DryRun
```

Restore them here once `retention` and `config::RetentionConfig` land below the
composition root.

### 4. Drop the duplicated definitions and re-export from here

Root still defines its own copies of everything in the "moved down" rows above.
Delete them and re-export, otherwise the two definitions drift:

| Root file | Re-export |
| --- | --- |
| `src/project_registry.rs` (reap contract, `alias_key_path`, `ephemeral_root_rejection`, `is_ephemeral_path`, `GIT_COMMON_DIR_ALIAS_PREFIX`) | `pub use tracedecay_global_db::{…};` |
| `src/application/session/ports.rs`, `SessionDataFreshness` in `types.rs` | `pub use tracedecay_global_db::session_temporal::execution::{…};` |
| `src/application/session/lcm/render.rs` | `pub use tracedecay_global_db::session_temporal::render::{…};` |
| `src/application/configuration/{types.rs, ports.rs}` | `pub use tracedecay_global_db::configuration::contracts::{…};` |
| `src/config/{registry.rs, resolver.rs, legacy_decoder.rs}` | `pub use tracedecay_global_db::configuration::{registry, resolver, …};` |
| `SemanticConfig`, `SemanticProfileSelection` in `src/config.rs` | `pub use tracedecay_global_db::configuration::semantic::{…};` |

One deliberate exception: `configuration::registry::LegacyProjectDefaults`
carries the ~20 scalar defaults root's `TraceDecayConfig` / `SyncConfig` /
`TelemetryConfig` contribute to the legacy transition bridge. Root owns the
`config.json` file schema (and its serde/env decoding), so the structs
themselves stay there; only the default *values* were duplicated. Keep the two
in step until the legacy scalar surface is retired.

### 5. Root feature forwarding

The root `production` / `test-transport` / `lite` / `full` feature sets still
forward nothing to `tracedecay-global-db`.

## Still blocked: the `#[cfg(test)]` surface

`cargo check -p tracedecay-global-db --all-features --all-targets` reports **55
errors**, none of them in compiled code. Every one is a test-only seam whose fix
belongs to another crate:

| Count | Seam | Blocked on |
| ---: | --- | --- |
| 35 | `tracedecay_runtime_core::db::engine::TestConnection` | Gated `#[cfg(test)]` in `crates/tracedecay-runtime-core/src/db/engine/mod.rs:23`, **not** on the crate's `test-transport` feature, so no dependent test build can reach it. Needs the kernel to widen the gate to `any(test, feature = "test-transport")`, exactly as this crate did for `mod tests`. |
| 15 | `crate::{sessions, db, errors, global_db}` inside `tests/session_suite/lcm_schema/` | The repo-root integration fixture is `#[path]`-included from `session_temporal`; it is root test code that has to be re-homed or repointed by the lead. |
| 8 | `crate::application::host_admission::{HostAdmissionScope, HostAdmissionTestRuntimeV1}` | `src/application/` is staying at the top of the stack (`tracedecay-application/SEAMS.md`). The sessions mover moved the *production* half into `tracedecay_sessions::admission`; the test runtime did not come with it. |
| 3 | `crate::application::{context, session}` value types in `session_temporal/tests/{application,privacy}.rs` | Same blocker. |
| 3 | `tracedecay_sessions::runtime::{lcm::payload::write_external_payload, cursor_composer::normalize_cursor_composer_observation*}` | `#[cfg(test)]` inside `tracedecay-sessions`; unreachable across a crate boundary without a `test-helpers`-style feature there. |
| 1 | `crate::store::session::GlobalDbSessionTemporalStore` | Root adapter (`src/store/session.rs`) implementing `tracedecay_store` session traits over this crate. Move-down candidate once `src/store/` is re-homed; its only remaining root couplings are `crate::daemon::store_runtime` and `crate::db`, both now in the kernel. |

## Aftermath queue (unchanged)

- Docs still naming `src/global_db/…` paths: `docs/LCM-PAYLOAD-LIFECYCLE.md`,
  `docs/THERMO-NUCLEAR-REVIEW.md`,
  `docs/REBRAND-COMPATIBILITY-FOLLOW-UP-CHECKLIST.md`,
  `docs/plans/tracedecay-v2/{13,15,23}-*.md`,
  `docs/superpowers/plans/2026-06-29-tracedecay-followups.md`.
- `Cargo.lock` was regenerated by the checks in this worktree.

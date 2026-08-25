# Root-crate extraction: dependency analysis

Status: analysis. Produced to unblock the work parked on
`codex/relocate-tracedecay-root-package`. Nothing here relocates a module; the
only code change shipped alongside this document is the provably-nominal cut
described in [§3](#3-already-cut).

Measured on `codex/tracedecay-total-redesign-plan-reopened` @ `9a526da4f`.

---

## Executive summary

The premise under which this analysis was commissioned — *`src/daemon/` and
`src/mcp/` are circularly dependent, so break that cycle and one side becomes
extractable* — **does not hold**. It is true that the two modules reference each
other. It is not true that this is what blocks extraction.

The root crate's top-level modules form **one strongly connected component of 26
modules, 381,480 lines, 673 files** — 94% of the root crate's lines and 88% of
its files. `daemon ↔ mcp` is one edge pair inside that component. Deleting it
outright changes nothing:

| counterfactual | resulting cycle |
|---|---|
| as-is | **1 SCC: 26 modules, 381,480 lines** |
| `daemon ↔ mcp` edges removed entirely | **1 SCC: 26 modules, 381,480 lines** (identical) |
| `daemon` + `mcp` removed as one unit | 2 SCCs: 9 modules / 27,108 lines, and 3 modules / 15,071 lines |

The third row is the finding that matters. `daemon` and `mcp` are **jointly**
extractable and **separately** are not — and extracting them jointly requires
cutting **25 file-edges / 114 references across 13 modules**, versus the **78
file-edges / 320 references** in the `daemon ↔ mcp` cycle itself.

**Recommendation: do not break `daemon ↔ mcp`. Extract them together as one
crate, and spend the effort on the 25 reverse edges instead.** That is roughly a
quarter of the work for the entire result.

---

## 1. The edge map

### 1.1 Corrections to the first-pass counts

The counts in the original brief are close but drift in three ways, all worth
recording because they change which edges look expensive.

* **`mcp → daemon` is 39 files, not 48.** The `48` comes from `grep crate::daemon`,
  which also matches the *separate* top-level modules `crate::daemon_client` and
  `crate::daemon_contract`. Those are real dependencies but they are not part of
  this cycle; `mcp → daemon_client` is 21 files / 93 refs and
  `mcp → daemon_contract` is 9 files / 51 refs, counted separately below.
* **The per-symbol tallies were reference counts spread over files, not distinct
  paths.** `McpServer` is 23 files (not 63 references to a single name); once
  associated functions are folded in (`McpServer::new`, `::new_with_context`) it
  reaches 26 files.
* **`ShutdownStatus` is 1 file, not 23.** It is `pub(crate) use`d at
  `src/daemon.rs:213` and named from exactly one MCP file
  (`src/mcp/server/connection.rs`). The high count came from matching the name
  inside `daemon` itself.

### 1.2 `daemon → mcp` — 45 files, 78 distinct paths

*(counts after the nominal cut in §3; before it, 48 files / 82 paths)*

| kind | what | weight | how it cuts |
|---|---|---|---|
| **Composition root** | `McpServer`, `server::McpServerConstructionContext`, `McpServerDaemonAuthority`, `McpServerDaemonCoreAuthority`, `McpServerDaemonDatabases`, `McpServerWriters` | 26 files | Does not cut. See §2.1 — `McpServer` is not a protocol server. |
| **Erased ports the daemon fills** | `DatabaseOwnerReconciler`, `RetainedProjectServerResolver`, `CodeGraphReadAdmissionPort`, `CodeIndexHookSink`, `CodeIndexReconcileSink`, `CodeIndexPublicationIdentityResolver`, `BackgroundRefreshWriter` | 7 files | Already `Arc<dyn …>` aliases in `mcp/server/construction.rs`. Move the alias to a shared crate; both sides keep compiling. **Cheap.** |
| **Response lifecycle / routing** | `ProjectServerResponseLifecycle`, `SelectedProjectResponseLease`, `RmcpSelectedProjectResponseAuthority`, `RmcpWorkDeliverySettlement`, `LiveTranscriptRefreshJoin` | 6 files | Data + small state machines. Move to a types crate. **Cheap.** |
| **Concrete transports** | `StdioTransport`, `ChannelTransport`, `ReplayTransport`, `write_wire_oversized_rejection`, `McpTransportReader/Writer`, `McpDuplexTransport` | 8 files | These are genuinely root-coupled (`tokio` types + this crate's host-admission frame bounds — see the module docs at `src/mcp/transport.rs:1`). They travel with whichever crate owns the connection. |
| **Handler entry points** | 20 free functions: `handle_tool_call_with_registry_options`, `handle_projectless_admin_cli`, `handle_projectless_hook_runtime`, `retained_mcp_operation`, `execute_profile_retained_mcp_tool`, `replay_projectless_hermes_host_admission`, `admit_hook_v2_envelope`, `hook_v2_pending_work_envelopes`, `shutdown_dashboard`, `dashboard_native_integration_status`, … | 11 files | The daemon *calls into* MCP tool dispatch. This is a real, correct, load-bearing dependency: the daemon hosts the tools. **Cannot cut without inverting it into a registry.** |
| **Pure data / views** | `ToolDefinition`, `SessionRefresh{Progress,Receipt,Frontier,Coverage}View`, `SessionRefreshCommand/Action/ServiceOutcome`, `HookV2AdmissionOutcomeV1`, `SourceEdit{,Reconciliation,Rollback}InvocationV1` | 6 files | Move to a types crate. **Cheap.** |
| **Traits** | `SessionRefreshServicePort` | 2 files | Move to a ports crate. **Cheap.** |
| **Test-only** | 11 of the 45 files (`daemon/tests/*`, `*_tests.rs`, `retained_test_support.rs`, `dashboard_configuration_test_runtime.rs`) | 11 files | Move to an integration-test crate that may depend on both. **Free.** |

### 1.3 `mcp → daemon` — 33 files, 67 distinct paths

*(after the nominal cut in §3; before it, 39 files / 76 paths)*

| kind | what | weight | how it cuts |
|---|---|---|---|
| **Already ports** | `session_retrieval::SessionApplicationRetrievalPortV1` (trait), `lcm_authority::MountedLcmAuthorityPort` (trait), `work_evidence_retrieval::WorkFederatedQueryAuthorityPortV1` (trait), `remote_protocol::RemoteOperationalStatusProviderV1` (`Arc<dyn Fn>`), `SessionApplicationRetrievalFutureV1`, `WorkFederatedQueryAuthorityFutureV1` | 12 files | **Move the trait definition to a ports crate; the daemon keeps its impl.** The call sites already hold `Arc<dyn Port>`. This is the single cheapest large win on this side. |
| **Pure data / contract** | `SessionRetrievalStoreScope`, `SessionRetrievalPageView`, `SessionRetrievalServiceOutcome`, `LcmDescribeServiceCommand/Outcome`, `ShutdownStatus`, `HookOrchestrationAdmissionV1`, `MemoryTargetAccessV1`, `AuthorityRegistrationV1`, `DAEMON_SHUTDOWN_DEADLINE`, `DAEMON_TOOL_RESPONSE_GRACE` | 8 files | Move to a types crate. **Cheap.** |
| **Concrete services needing a new trait** | `DaemonInvocationService`, `LocalProfileIdentityAuthorityV1`, `ProfileRetainedConnectionAuthorityV1`, `ProfileRetainedAuthoritiesV1`, `ProductionRetainedAuthoritiesV1`, `SessionTemporalRefreshWake`, `DaemonSessionRuntimeRegistryV1`, `DaemonSessionRetrievalService`/`Root`, `DaemonWorkEvidenceRetrievalV1`, `DaemonConfigurationRuntimeRegistrar`, `DaemonLifecycle`, `ParkableConnectionAdmission`, `DaemonHandshake`, `ProductionProjectCompositionHarnessV1` | 14 files | Each needs a trait written for it and every construction site rerouted. **Expensive: this is the bulk of the real cost.** |
| **Free functions** | 18: `profile_identity::load_or_create`, `project_open_owners::resolved_scope_for_project`, `admit_registered_hook_orchestration`, `mount_registered_lcm_authority`, `retained_surface_ports`, `execute_profile_retained_application`, `public_search_page`, `settle_owned_blocking_task`, `github_stack_hook_available`, `current_connection_admission`, … | 13 files | Each becomes a method on one of the traits above, or moves with its data. **Medium.** |
| **Test-only** | 6 of the 33 files | 6 files | **Free.** |

### 1.4 The edges the first pass did not look at

`mcp` and `daemon` do not only depend on each other. Their full root-module
dependency profile:

```
src/mcp                                  src/daemon
  155 refs / 80 files  crate::tracedecay   1017 refs /172 files  crate::daemon (internal)
  150 refs / 33 files  crate::daemon        233 refs / 94 files  crate::global_db
  109 refs / 64 files  crate::errors        191 refs / 68 files  crate::db
   93 refs / 21 files  crate::daemon_client 190 refs / 84 files  crate::errors
   71 refs / 37 files  crate::global_db     174 refs / 60 files  crate::tracedecay
   59 refs / 11 files  crate::dashboard     167 refs / 45 files  crate::mcp
   51 refs /  9 files  crate::daemon_contract 153 refs / 20 files crate::application_surface
   44 refs / 15 files  crate::application_surface 146 refs / 57 files crate::config
   … 32 more modules                        … 32 more modules
```

`mcp` touches 40 other root modules; `daemon` touches 40. Even a perfect
`daemon ↔ mcp` split leaves both sitting on `crate::tracedecay`, `crate::db`,
`crate::global_db`, `crate::config`, `crate::dashboard`,
`crate::application_surface` and the rest — all still in the root crate, and all
in the same SCC.

---

## 2. Why the layering-inversion hypothesis is wrong

### 2.1 `McpServer` is not a protocol server; it is the per-project composition root

The hypothesis was: *`daemon` holds transport/server plumbing it hosts, while
`mcp` reaches down into daemon domain services — a classic inversion, fixed by a
ports crate that `mcp` depends on and `daemon` implements.*

The direction is backwards, and the object at the centre is not what the name
suggests. `McpServer` (`src/mcp/server.rs:211`) is a ~60-field struct holding the
open code graph, six database leases, the LSP diagnostic broker, the token map,
and one `Option<…>` slot for every daemon-supplied authority:

```rust
pub struct McpServer {
    cg: Arc<tokio::sync::RwLock<Arc<TraceDecay>>>,
    global_db: Option<RegisteredGlobalDbLeaseV1>,
    session_db: Option<RegisteredGlobalDbLeaseV1>,
    user_session_db: Option<RegisteredGlobalDbLeaseV1>,
    profile_identity: Option<crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1>,
    profile_retained_authority: Option<crate::daemon::retained_owner::ProfileRetainedConnectionAuthorityV1>,
    project_session_refresh_wake: Option<crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake>,
    project_lcm_authority: Option<Arc<dyn crate::daemon::lcm_authority::MountedLcmAuthorityPort>>,
    remote_operational_status: Option<crate::daemon::remote_protocol::RemoteOperationalStatusProviderV1>,
    // … ~50 more
}
```

So the cycle is not two layers wired the wrong way round. It is **one object
whose type signature spans both modules.** The daemon constructs it (26 files);
its own field types are daemon types (that is most of `mcp → daemon`). Neither
module "reaches into" the other — they co-own a god-object that happens to live
under `src/mcp/`.

### 2.2 The ports pattern already exists — the traits are just filed in the wrong module

The optimistic half of the finding. `ToolCallRegistryOptions`
(`src/mcp/tools/handlers/mod.rs:231`) is the unbundled mirror of `McpServer`'s
fields — how handlers receive daemon capabilities without holding the server —
and it is already written as erased ports:

```rust
pub(crate) dashboard_session_retrieval_service:
    Option<Arc<dyn crate::daemon::session_retrieval::SessionApplicationRetrievalPortV1>>,
pub(crate) remote_operational_status:
    Option<crate::daemon::remote_protocol::RemoteOperationalStatusProviderV1>,
```

The codebase already does the right thing structurally — including for ports that
made it out, e.g. `tracedecay_usecases::graph::CodeGraphReadAdmissionPort`. What
is left is not a design problem but a **filing** problem: a set of traits sitting
in `crate::daemon` that should sit in a crate both sides can see.

Consequently only **1 of 142 files** under `src/mcp/tools/` names `McpServer` at
all (and that one is a test). The tool tree — 54,513 lines, the single biggest
block in `src/mcp/` — is **not** coupled to the server object.

### 2.3 The decisive counterfactual

Removing every `daemon ↔ mcp` edge and recomputing strongly connected components
over the root crate's 74 top-level modules yields **the identical 26-module,
381,480-line SCC**. The cycle survives through, among others:

```
tracedecay        -> daemon           (5 files, 12 refs)
daemon_client     -> daemon           (3 files, 30 refs)
host_admission    -> mcp / daemon     (2 files each)
dashboard         -> daemon / mcp
doctor            -> daemon / mcp
retention         -> daemon           (and daemon -> retention)
runtime_telemetry -> daemon
serve, monitor, analytics_bridge, agents, catalog_composition, daemon_contract -> daemon / mcp
```

Any plan that begins "first break the `daemon ↔ mcp` cycle" spends its whole
budget and extracts nothing.

---

## 3. Already cut

Shipped in this branch, verified with `cargo check --workspace --all-targets
--locked` clean.

**Class: nominal edges — a module reached a type through a neighbour that merely
re-exports it from a crate both already depend on.** Two instances, one in each
direction:

* `src/mcp/transport.rs:11` is `pub use tracedecay_jsonrpc::{ErrorCode,
  JsonRpcError, JsonRpcRequest, JsonRpcResponse, McpTransport};`. The daemon
  reached all five through `crate::mcp::`.
* `src/daemon/core_hooks.rs:13` is `pub use tracedecay_hooks::core_events::*;`,
  which supplies `DaemonHookEvent`, `HookAgent`, `HookRouteMetadata`,
  `HookTerminalReceipt`, `HOOK_EVENT_METHOD`. The MCP tree reached all five
  through `crate::daemon::`.

Every such path was rewritten to name the owning crate. Behaviour-preserving by
construction: each path resolves to the same item.

| | before | after |
|---|---|---|
| `daemon → mcp` | 48 files, 82 paths | **45 files, 78 paths** |
| `mcp → daemon` | 39 files, 76 paths | **33 files, 67 paths** |

Three daemon files and six MCP files lost their cross-module edge entirely.

**Other candidates of this class were searched for and are exhausted.** The
remaining re-exports on these edges are not nominal:
`mcp/server/construction.rs:34` re-exports
`tracedecay_dashboard_api::project_graph::RetainedProjectGraphRequest`, but its
one daemon consumer (`daemon/dashboard_automation.rs`) also uses
`RetainedProjectServerResolver`, which is genuinely MCP-owned; rewriting the one
path would not remove a file-edge. `src/daemon.rs:364` re-exports
`crate::daemon_contract::{…}` and already carries a comment telling new callers
to depend on the contract module directly.

---

## 4. The minimum cut

### 4.1 Scope the goal correctly

"Make `daemon` extractable" and "make `mcp` extractable" are not separable goals,
and neither is achievable on its own. The achievable goal is: **make
`daemon` + `mcp` jointly a leaf of the root crate's module graph**, then lift
them out as one crate.

That works because removing both as a unit collapses the 26-module SCC to two
small residual ones (§4.4).

### 4.2 The cut

Everything in the root crate that `daemon` + `mcp` transitively depend on, and
which points back at them. **13 modules, 25 file-edges, 114 references.**

| edge | files | refs | what crosses |
|---|---|---|---|
| `tracedecay → daemon` | 5 | 12 | `store_runtime::session_registry::DaemonSessionRuntimeRegistryV1`, `profile_identity::{load_or_create, LocalProfileIdentityAuthorityV1}` |
| `daemon_client → daemon` | 3 | 30 | `transport::{BrokerListener, BrokerStream, default_loopback_endpoint}`, `DaemonHandshake`, `DaemonConnection`, `write_daemon_preamble`, `DAEMON_TOOL_RESPONSE_GRACE` |
| `host_admission → mcp` | 2 | 12 | `tools::SessionAuthorities`, `server::McpServerConstructionContext`, `tools::{ToolCallRegistryOptions, handle_tool_call_with_registry_options}` |
| `host_admission → daemon` | 2 | 7 | `profile_identity::load_or_create`, `lcm_effects::DaemonLcmEffectService`, `session_registry`, `SessionTemporalRefreshTestAuthority` |
| `dashboard → daemon` | 1 | 13 | `store_runtime::*`, `DaemonInvocationService::with_code_index_schedulers`, `code_index_scheduler::CodeIndexSchedulerRegistryV1`, `dashboard_automation::*` |
| `doctor → daemon` | 1 | 9 | `DaemonHandshake::for_current_client`, `profile_identity::load_or_create`, `session_registry`, `call_default_tool_within`, `tool_json_payload`, `daemon_reachable` |
| `runtime_telemetry → daemon` | 1 | 7 | `store_runtime::telemetry::{RuntimeTelemetryProjection, ShardRuntimeTelemetry}`, `store_runtime::shard::ShardRuntimeHealth` |
| `serve → daemon` | 1 | 5 | `default_socket_path`, `should_proxy_serve_to_daemon`, `proxy_stdio_to_daemon`, `DaemonHandshake` |
| `retention → daemon` | 2 | 3 | `git_watch::store_maintenance::run_project_compaction`, `session_registry`, `profile_identity::load_or_create` |
| `analytics_bridge → daemon` | 1 | 3 | `DaemonHandshake::for_current_client`, `call_default_tool`, `tool_json_payload` |
| `monitor → daemon` | 1 | 3 | `DaemonHandshake`, `call_default_tool`, `tool_json_payload` |
| `agents → mcp` | 1 | 3 | `tools::{ToolDefinition, get_tool_definitions, format_capable_tool_names}` |
| `dashboard → mcp` | 1 | 2 | `tools::handlers::{DashboardLcmReadAdapter, DashboardGitCorrelationReadAdapter}` |
| `daemon_contract → daemon` | 1 | 2 | `service::invocation` |
| `catalog_composition → mcp` | 1 | 2 | `tools::explore_call_budget` |
| `doctor → mcp` | 1 | 1 | `tools::ast_grep_diagnostics_json` |

### 4.3 Four hub symbols carry most of it

Counting every root module that reaches into `daemon`/`mcp` (including modules
outside the blocking set, which must be cut eventually anyway):

| pulled in by | symbol | modules |
|---|---|---|
| **9 modules** | `daemon::store_runtime::session_registry` (`DaemonSessionRuntimeRegistryV1`) | dashboard, doctor, host_admission, host_admission_test, profile_registry_maintenance, retention, session_temporal_benchmark, sessions, tracedecay |
| **8 modules** | `daemon::profile_identity::{load_or_create, LocalProfileIdentityAuthorityV1, read_required, load_existing}` | dashboard, doctor, host_admission, host_admission_test, retention, session_temporal_benchmark, sessions, tracedecay |
| **8 modules** | `daemon::DaemonHandshake` | analytics_bridge, daemon_client, doctor, monitor, runtime_ports, serve, work_cli, workflow_cli |
| **4 modules** | `daemon::{call_default_tool, tool_json_payload}` | analytics_bridge, doctor, monitor, runtime_ports |

**None of these four is a daemon-server concern.** They are, respectively:
profile identity resolution, session-store registry, the client handshake, and a
client-side tool-call helper. They live under `src/daemon/` for historical
reasons and they are what actually holds the root crate together.

`src/daemon/profile_identity.rs` in particular is 521 lines whose *entire*
outward dependency set is `crate::db` (2 refs), `crate::errors` (1),
`crate::storage` (1), and `super::authority::canonical_identity_path` — where
`src/daemon/authority/` is a single 4-line Windows-ACL file. **It is already an
independent module wearing a `daemon::` prefix.**

### 4.4 What remains after the cut

With `daemon` + `mcp` gone, two residual SCCs stay in the root:

* **9 modules / 27,108 lines**: `tracedecay`, `host_admission`, `config`,
  `graph`, `branch`, `store`, `dashboard`, `analytics_bridge`, `semantic_code`
  — held together mainly by `tracedecay ↔ host_admission`.
* **3 modules / 15,071 lines**: `application_surface`, `daemon_contract`,
  `daemon_client` — held together by `daemon_contract → application_surface`
  (2 files, 189 refs) against `application_surface → daemon_contract` (9 files,
  105 refs).

Both are an order of magnitude smaller than what they replace, and both are
follow-on work, not blockers.

### 4.5 Crates to introduce

Only two are needed for the recommended sequence.

**`tracedecay-profile-identity`** — `src/daemon/profile_identity.rs` plus
`src/daemon/authority/`. Depends on `tracedecay-store` / the DB layer only.
Removes an 8-module hub edge and is close to a pure file move.

**`tracedecay-daemon-ports`** — the shared vocabulary between the daemon runtime
and its consumers:
* the traits that are already traits: `SessionApplicationRetrievalPortV1`,
  `MountedLcmAuthorityPort`, `WorkFederatedQueryAuthorityPortV1`,
  `SessionRefreshServicePort`;
* the erased aliases: `RemoteOperationalStatusProviderV1`,
  `DatabaseOwnerReconciler`, `RetainedProjectServerResolver`,
  `CodeIndexHookSink`, `CodeIndexReconcileSink`,
  `CodeIndexPublicationIdentityResolver`, `BackgroundRefreshWriter`;
* the contract data: `SessionRetrieval{StoreScope,PageView,ServiceOutcome}`,
  `LcmDescribeService{Command,Outcome}`, `ShutdownStatus`,
  `HookOrchestrationAdmissionV1`, `MemoryTargetAccessV1`,
  `SessionRefresh*View`, `SourceEdit*InvocationV1`,
  `DAEMON_SHUTDOWN_DEADLINE`, `DAEMON_TOOL_RESPONSE_GRACE`.

`tracedecay-usecases` already hosts 77 traits including
`graph::CodeGraphReadAdmissionPort`, so folding these in there rather than
creating a new crate is a reasonable alternative — the choice is filing
convenience, not architecture.

A `tracedecay-session-registry` crate for `DaemonSessionRuntimeRegistryV1` is
**not** recommended as an early step: unlike `profile_identity`,
`src/daemon/store_runtime/` (16,351 lines) is genuinely entangled with
`session_sync`, `branch_admin`, `remote_replay_transaction`,
`code_index_scheduler` and `transport`. Reach it through a narrow read port
instead.

---

## 5. Recommended sequence

1. **Done — nominal edges.** §3. Zero risk, mechanical, verified.
2. **Extract `profile_identity`.** Near-pure file move; kills the 8-module hub.
   Verify with `cargo check --workspace --all-targets --locked`.
3. **Move `DaemonHandshake` + `call_default_tool` + `tool_json_payload` into
   `daemon_client`.** These are client-side helpers that already have a home;
   `daemon_client → daemon` (30 refs, the heaviest reverse edge) is a large part
   of this. Kills a second 8-module hub and one 4-module hub.
4. **Introduce `tracedecay-daemon-ports`** and move the traits/aliases/data of
   §4.5 into it. Retarget `crate::daemon::…` paths in `src/mcp/` at it. This does
   *not* break `daemon ↔ mcp` and is not meant to — it shrinks the surface each
   side must carry, and 12 of the 33 `mcp → daemon` files stop naming the daemon
   at all.
5. **Introduce a read port for `DaemonSessionRuntimeRegistryV1`** and retarget
   `tracedecay`, `retention`, `dashboard`, `doctor`, `sessions` at it.
6. **Reroute the residual reverse edges** — `runtime_telemetry`, `serve`,
   `monitor`, `analytics_bridge`, `agents`, `catalog_composition`,
   `dashboard → mcp`, `doctor → mcp`, `host_admission`. All are 1–2 files each.
7. **Move the test-only edges** (11 daemon-side + 6 MCP-side files) to an
   integration-test crate that may depend on both.
8. **Lift `src/daemon/` + `src/mcp/` out together** as `tracedecay-daemon`.
   322,000 lines leave the root crate in one move.
9. *(Follow-on, not blocking)* break `application_surface ↔ daemon_contract` and
   `tracedecay ↔ host_admission`.

Steps 2, 3, 5 and 6 are independent of each other and can run in parallel.

---

## 6. What is genuinely hard

Stated plainly, because the plan above is optimistic about steps 2–7 and should
not be read as optimistic about everything.

* **`McpServer` cannot be split, only moved.** It is the per-project composition
  root. Any attempt to put `daemon` and `mcp` in *separate* crates has to
  dismantle a ~60-field god-object whose fields are the daemon's authorities and
  whose methods drive MCP dispatch. This analysis recommends not attempting it,
  and step 8 sidesteps it entirely by keeping both in one crate. If a future
  requirement forces the split, that is a redesign, not a refactor.

* **The daemon genuinely calls into MCP tool dispatch.** 20 handler entry points
  across 11 daemon files. This is not an inversion to be corrected — the daemon
  *hosts* the tools, and it is the correct direction. Inverting it means a
  runtime tool registry the daemon populates, which changes dispatch behaviour
  and is a design change, not a mechanical one. Out of scope for extraction.

* **`store_runtime` is a second god-module.** 16,351 lines across 26 files,
  reaching `session_sync`, `branch_admin`, `remote_replay_transaction`,
  `code_index_scheduler`, `transport`, `remote_protocol`, `store_writer_gate`.
  Nine root modules want one type out of it. A read port defers the problem
  correctly, but the module itself will need its own analysis eventually.

* **`daemon_contract → application_surface` is 189 references across 2 files.**
  The densest single edge measured anywhere in this analysis. It is deferred to
  step 9 and it will not be pleasant.

* **The reference counts here are syntactic.** They come from resolving `use`
  trees and `crate::…` paths over the source; they do not account for trait
  method resolution, macro expansion, or `#[cfg]`-gated arms that a given target
  does not compile. Treat them as a reliable *lower* bound on coupling, and
  re-verify each step with `cargo check --workspace --all-targets --locked`
  rather than trusting the table.

---

## Appendix: reproducing the measurements

Every number above comes from resolving `use` trees and inline `crate::…` paths
across `src/`, attributing each file to its top-level module, and running Tarjan
over the resulting module graph. Modules are the 74 entries of `src/*.rs` and
`src/*/` excluding `lib.rs`, `main.rs`, and `bin/`. Counterfactuals are the same
computation with selected edges or nodes deleted.

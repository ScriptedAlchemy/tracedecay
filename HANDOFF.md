# Eager graph opens handoff

## Worktree and branch

- Worktree: `/fast/projects/tracedecay-707-lazy-graphs`
- Handoff branch: `wip/eager-graph-opens-handoff`
- Implementation commit already present before this note: `dd9e6ac53 fix(graph-db): lazily retain non-serving graph engines`
- Integrated tip deployed for the live check: `1195a375956cb077ec4037a69712cd6bb861c3b1`

## Owners and open paths identified

### Session relation graph owners changed in this lane

All of these converge on `open_session_relation_owner_with_cancellation`, which previously resolved an eager registry owner:

- `crates/tracedecay-store-runtime/src/session_registry/code_graph/graph_attachment.rs:20` — ordinary session relation owner entry point.
- `crates/tracedecay-store-runtime/src/session_registry/code_graph/graph_attachment.rs:45` — task/cancellation-aware entry point.
- `crates/tracedecay-store-runtime/src/session_registry/code_graph/graph_attachment.rs:113` — now calls `resolve_lazy_owner_attachment`.
- `crates/tracedecay-store-runtime/src/session_registry/code_graph/memory_runtime.rs:153` — project-memory graph runtime caller.
- `crates/tracedecay-store-runtime/src/session_registry/mounts.rs:252` — mounted session graph caller.
- `crates/tracedecay-store-runtime/src/session_registry/code_graph.rs:2192` — retained code-graph runtime attachment caller.
- `crates/tracedecay-store-runtime/src/session_registry/remote_recovery/publication.rs:310` — remote-recovery publication caller.

The eager registry path is:

- `crates/tracedecay-graph-db/src/registry.rs:942` — `resolve_owner_attachment`.
- `crates/tracedecay-graph-db/src/registry/support.rs:138` — `GraphDb::open_with_store_state`.
- `crates/tracedecay-graph-db/src/runtime.rs:141` — validates and opens the graph.
- `crates/tracedecay-graph-db/src/runtime.rs:1652` — `GrafeoDB::with_config`.

### Direct sealed code-generation owners not changed

These are the strongest remaining startup-open candidates and were not made lazy by this lane:

- `crates/tracedecay-store-runtime/src/session_registry/code_graph.rs:1253` — recovers a verified sealed snapshot.
- `crates/tracedecay-store-runtime/src/session_registry/code_graph.rs:1595` — retained/replay recovery also attempts a verified sealed snapshot.
- `crates/tracedecay-graph-db/src/registry/publication.rs:95` — `recover_verified_sealed_snapshot`.
- `crates/tracedecay-graph-db/src/registry/publication.rs:143` — calls `open_direct_sealed_generation`.
- `crates/tracedecay-graph-db/src/sealed_store.rs:73` — direct sealed-generation open entry point.
- `crates/tracedecay-graph-db/src/sealed_store.rs:108` — eagerly invokes `GraphDb::open_with_store_state`, then reads the projection and proves the recovered digest before returning.

Other explicit eager production paths found but not established as startup owners:

- `crates/tracedecay-usecases/src/store/vector_generations/graph_adapter/evaluation_runtime.rs:274` — eager registry owner for vector evaluation.
- `crates/tracedecay-graph-db/src/sealed_store.rs:666`, `:767`, and `:1059` — sealed-store construction/adoption/verification opens.

## What changed and why

- Added `GraphDb::open_lazy_with_store_state` and changed the native database/state fields to optional resident state (`crates/tracedecay-graph-db/src/runtime.rs:184`).
- Added first-use opening through `ensure_opened` and lazy hibernation through `hibernate_if_lazy` (`crates/tracedecay-graph-db/src/runtime.rs:995`).
- Added `GraphDbRegistry::resolve_lazy_owner_attachment` (`crates/tracedecay-graph-db/src/registry.rs:955`) and a lazy support open.
- Changed session relation graph attachment to retain store/graph ownership without opening Grafeo (`graph_attachment.rs:113`).
- Changed the final operation-lease drop to hibernate a lazy graph (`crates/tracedecay-graph-db/src/owner.rs:59-73`).
- Deferred project-memory graph reconciliation until a snapshot/write needs it (`crates/tracedecay-runtime-core/src/store/memory/graph.rs:122`, `:333`, `:482`).
- Added `multi_scope_startup_retains_graph_authorities_without_opening_engines` (`crates/tracedecay-store-runtime/src/session_registry/verified_graph_runtime_port_contract_tests.rs:132`). It proves zero resident *session relation* engines after those authorities attach and proves first-use open/final-lease hibernation. It does not prove the requested exactly-one code-generation engine across the complete daemon startup journey.

The intent was to remove eager profile/project memory and session relation engines while preserving authority attachments and replay-binding behavior. The live result shows that this slice did not reach the actual acceptance target.

## Live deployment result

The release binary identified itself as:

`TraceDecay v0.1.0-beta.37+1195a375956cb077ec4037a69712cd6bb861c3b1`

The first `systemctl start` did not replace the already-running process; that process had started at 04:10 UTC. I then explicitly restarted the service with the installed binary. A probe from the worktree accidentally warmed the worktree scope, so I restarted again and measured the intended `/fast/projects/tracedecay` scope from the clean service start at 07:55:40 UTC.

Observed on PID 2857738:

- `VmRSS`: 26,842,984 KiB (25.60 GiB)
- `VmHWM`: 27,139,284 KiB (25.88 GiB)
- `VmSwap`: 16,600,108 KiB (15.83 GiB)
- Exact-current was not reached during 120 polls over 680.5 seconds.
- Status remained `code_index_freshness.status=warming`, `code_graph_serving.state=pending` (once `unavailable`), and `staleness_state=indexing`.

This falsifies the expected 7–9 GiB / one-census live outcome. It demonstrates that lazifying the session relation owner path is insufficient. It does not by itself prove whether the retained direct-sealed code-generation snapshots account for all resident memory, because the daemon never reached steady exact-current and the run did not capture a fresh Hotpath allocation report.

The service was stopped after this result and `systemctl --user is-active tracedecay.service` returned `inactive`.

## Not yet ruled out

1. `recover_verified_sealed_snapshot` may eagerly open one direct sealed Grafeo engine for every active and retained/replay-bound publication clone. This is the leading unmodified path.
2. The active generation rebuild may itself hold multiple full graph-sized states or large transient staging/sealed-copy states while status is `indexing`; exact-current was never observed.
3. Long-lived `VerifiedGraphSnapshot` / replay-binding leases may prevent the final-lease hibernation path from running even where the underlying registry owner is lazy.
4. Startup reconciliation or status requests may acquire session relation graph leases and legitimately open them before they later hibernate.
5. Existing sealed artifacts may still pay full LPG replay/property-index rebuild in `GrafeoDB::with_config`; marker hits skip the recovered-row digest proof, not necessarily native LPG/index reconstruction.
6. The direct-sealed path cannot simply use the current lazy constructor: it immediately reads `latest_projection` and verifies `sealed_copy_proof` before returning. A correct lazy design must defer both native open and digest proof until first read, while preserving fail-closed verification.
7. The retained historical publication clone regression fixed by `53a6a1d633` was not re-verified against a live lazy direct-sealed implementation, because that implementation was not completed.
8. The feature-off gate and targeted tests exercised the committed session-owner slice, but the full requested daemon-level exactly-one eager-engine acceptance test was not implemented.

## Commands and evidence used

Hotpath report query:

```bash
jq '.. | objects | select(.name? == "graph_db.generation.open.engine")' /fast/tmp/td-707-hotpath-alloc-report/hotpath-alloc.json
```

The authoritative report row was five calls, 29.9 GB allocated total, approximately 6.0 GB average per call.

Release build (after the operator directive, through the bare cargo-hauler shim):

```bash
hash -r
TRACEDECAY_SKIP_DASHBOARD_BUILD=1 \
TRACEDECAY_DASHBOARD_BUNDLE_SHA256=cc6617d59a5a3bd7b9be9000b7bad76b0a8fee96109d337069b0690edcf60cf1 \
cargo build --release -p tracedecay-cli --bin tracedecay --locked
```

Deployment and service evidence:

```bash
install -m 755 target/release/tracedecay "$HOME/.local/bin/tracedecay"
systemctl --user restart tracedecay.service
systemctl --user show tracedecay.service \
  -p MainPID -p ExecMainStartTimestamp -p ActiveState -p SubState \
  -p MemoryCurrent -p MemoryPeak
tracedecay status --json
journalctl --user-unit tracedecay.service \
  --since '2026-09-03 07:53:42' --no-pager -o short-iso
```

The journal showed `daemon_ready bootstrap_elapsed_ms=1745` on the first clean restart, followed by prolonged project indexing rather than exact-current.

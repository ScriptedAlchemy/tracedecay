# V2 RC reboot handoff

**Status:** active product delivery, intentionally wound down for an operator
reboot on 2026-08-09.

`00-plan-set-index.md` remains the sole roadmap and acceptance authority. This
file is the current operational handoff assembled from the final reports of
the active implementation and review lanes. Resume from this file and the
current branch; do not reconstruct intent from commit subjects alone.

## Resume invariants

- Branch: `codex/tracedecay-total-redesign-plan`.
- Preserve the current shared worktree. It contains substantial unfinished
  peer work. Do not run `git clean`, `git reset`, `git read-tree -u`, checkout
  paths from another revision, or otherwise sweep tracked or untracked files.
- Re-read every owned file immediately before editing. Use a fresh temporary
  index for each coherent commit and verify that `HEAD` has not advanced
  between seeding that index and committing.
- The required coordinator identity is intact at wind-down:
  `crates/tracedecay-usecases/src/stack_coordinator.rs` hashes to
  `e8dad46d599088b26847371cc8da96c6579b95ba` in both `HEAD` and the worktree.
- No repository file was mounted or marked immutable at wind-down. An earlier
  filename-focused BPF deletion monitor was stopped cleanly.
- The shared Git index is stale relative to temporary-index commits. After all
  agents are confirmed stopped, reconcile only the index against `HEAD`; never
  use that reconciliation to modify the worktree.
- npm trusted publishing/OIDC remains an external operator action. It is not a
  reason to hold local V2 implementation or verification work.

## Checkpoint integrity warnings

Three commits raced concurrent `HEAD` advances and are not isolated feature
commits. They preserve bytes but must be normalized path-by-path after reboot:

- `4e4715c09a` intended only the retained service tests, obsolete session-tool
  dispatch deletion, and two documentation updates. It also reversed the
  twelve-file TaskSession lower lane. The lower lane was restored afterward by
  `74ae0df268` from the verified `648bebde0c` checkpoint. Do not reapply or
  revert either commit wholesale.
- `7913905113` intended the generation-bound rename engine and graph projection
  mount but captured more than thirty concurrent peer paths. Safe predecessor
  commits are `14d61584a0` and `2e942bd813`. Preserve the current worktree
  `CodeGraphProjectionReadPort` injection and recover the intended source-edit
  paths without reverting legitimate peer changes.
- `9f34595b4e` intended
  `automation_fact_receipts_api.rs`, `automation_cli/facts.rs`,
  `automation_cli/mod.rs`, and `cli/help.rs`, but captured roughly fifty-three
  concurrent paths. Its route, library, and endpoint-test cutovers remain
  unfinished. Treat it as a reboot snapshot, not an isolated adapter commit.

`b9b200eb10` is a valid typed TaskSession application checkpoint, but its
`crates/tracedecay-application/src/lib.rs` hunk also contains concurrent
reexports. Verify those reexports against their canonical owners before using
the commit as attribution evidence.

## Durable checkpoints reached

- Work publication is atomic in `fc100bdb73`; Work product runtime,
  intelligence, routing, admission, and policy are checkpointed in
  `844512f240`, `6021d3f296`, `782486f988`, `184f98c803`, and `791d875011`.
- The TaskSession lower retrieval lane is verified in `648bebde0c` and restored
  in branch history by `74ae0df268`. Its domain test passed 1/1 and focused
  query tests passed 4/4. `b9b200eb10` adds the typed application boundary and
  `cargo check -p tracedecay-application --lib` passed at that checkpoint.
- Recovered daemon session retrieval authority is byte-exact in
  `7e5812db71`.
- Automatic fact application and receipts are checkpointed in `b1da03fbfc`,
  `1fe2d6d2eb`, and `786f2537c7`. Automatic managed-skill lifecycle and
  receipt-derived outcomes are in `eaeb3b98ad`, `ee700fae8d`, and
  `890cc12939`. Pinned daemon configuration, fact-constructor compile repairs,
  automatic policy guidance, and staged-notice deletion are in
  `7bc512d82e`, `d1da115773`, `7dc8c7923b`, and `e5bff28c7b`.
- Automatic-curation dashboard receipt UI is checkpointed in `91f9527f1b`;
  focused memory-config, RunHistory, and KnowledgeCuration tests passed 11/11.
- Typed terminal problem validation is checkpointed in `850265033c`.
- Retained transport adapters are checkpointed in `90e326c27e`; retained API
  types are included in `7b82a36ff9`.
- Duplicate receipt/revision identity is approved through `642b0221ae`.
- Native topology recovery is checkpointed in `5e3f72b738`, but its review is
  still rejected on the exact issues below.
- Remote transferred-frame quota enforcement already exists in `6642b45803`.
- The P0 GitHub stack coordinator and anchored corpus remain restored in
  `c70556fe38` and `ec35c90497`; native stack transition producers are in
  `e26461eac6`.

## Remaining work by lane

### Work and TaskSession retrieval

- Replace the removed Narrative/string compatibility path in
  `src/daemon/work_evidence_retrieval.rs` with the real
  `WorkTaskSessionPortV1` path. Construct the sealed `TaskSessionBindingV1`,
  call `SessionRetrievalService::execute_task_session`, and use the active
  `federated_authority_for` selector with reauthorization before selection,
  hydration, expansion, and continuation.
- Land the currently unfinished daemon module declaration,
  `McpServer::work_evidence_retrieval`, adapter `same_authority`, concrete
  evidence injection, and invocation adapter. Do not restore the old
  `.ok_or_else` optional-adapter assumption or the fabricated root session.
- Complete real MCP, HTTP, SDK, and dashboard journeys for task-to-session
  correlation, who worked on a task, provider-qualified session evidence,
  continuation, revocation, restart, stale graph, denied/unavailable, and
  current/as-of/evolution/forensic retrieval.
- Remove remaining public `WorkProjection` SDK uses from accept-task, replan,
  delta, and snapshot operations after all callers use the product authority.
- After the SQLite fact repair compiles, run the global-db/usecases checks and
  the rank-final revocation regression in addition to the focused query tests.

### Typed terminal problem propagation

- Keep the strict core from `850265033c`: ResetRequired is `Never + [Reset]`;
  PartialEffect is `Never + [Reconcile]` with a partial receipt and concrete
  commit proof; envelope construction is fallible and validated.
- Finish fallible-constructor propagation in feedback reads, operation stream,
  application JSON output, MCP tests, and every other caller still treating
  `ApplicationProblemEnvelope::new` as infallible.
- Add both terminal kinds to the remaining exhaustive API/SDK status,
  application surface, observability, and daemon-client matches.
- Stop downgrading reset-required results to Unavailable or InvalidRequest in
  application surface, daemon client, and multi-root paths.
- Make direct public `ApplicationProblem` serialization impossible for invalid
  values, or route it through validated canonical constructors.
- Regenerate the SDK wire authority so `reset`, PartialEffect,
  ResetRequired, and required-nullable `committed_receipt` are present.
- Re-run `cargo test -p tracedecay-application --lib result:: -- --nocapture`
  and then compile every recorded constructor consumer.

### Retained production surfaces

- Restore `src/daemon/retained_owner.rs` and its direct memory, session, and LCM
  children. Construct `RetainedSurfaceServiceV1` from real ports and map real
  receipts; the current production composition references a missing owner.
- Finish factory wiring against current server arguments and mount the same
  behavior through CLI, MCP, HTTP, and SDK.
- Restore behavior tests for exact identity, read/effect separation,
  cancellation, unavailable families, partial effects, reset-required,
  restart, and LCM production composition.
- Keep `MemoryStatus` classified as Read only if the mounted implementation is
  genuinely read-only. The old `memory_status_with_repair_v1` handler is
  administrative and cannot substantiate the new contract.
- The obsolete SessionStart/SessionEnd dispatch and documentation cleanup is
  present in the contaminated retained checkpoint; preserve its intended four
  blobs while normalizing history.

### Native topology and executable projection

- Restore or replace the missing `cleanup_recovery_roots` caller/owner contract
  so the daemon invocation compiles.
- Make topology retention match persisted authority: the store keeps one
  revision per scope-set ID, while the current projector retains multiple
  revisions and becomes stale after restart.
- Generate TypeScript/MCP projection schemas from the canonical Rust source;
  include scope-set ID/revision and `declared_revision`, and delete the manual
  duplicate schema.
- Mount real production callers/reexports for the native executable binding
  registry and exact topology readers; they remain contract/test-only.
- Extract the native MCP definition from the already oversized
  `src/mcp/tools/definitions.rs` rather than growing it.
- Add persisted registry plus session-sync restart evidence. Preserve the
  repaired blocking boundary and typed stale/denied/unavailable outcomes.

### Source edit and graph evidence

- Normalize the intended source-edit portion of `7913905113` without reverting
  peer work. Preserve generation-bound rename preview/apply, accepted-preview
  digest CAS, lexical and graph hazards, and the daemon
  `CodeGraphProjectionReadPort` injection.
- Reconcile `project_open_owners.rs` with the deferred automation config-scout
  hunk after the graph-port mount is stable.
- Compile the real daemon/MCP journey, run focused rename preview/apply and
  workspace tests, then regenerate Rust and TypeScript SDK contracts. The
  checked-in generated schema still describes the old text-only shape.

### Automatic curation, memory, and configuration

- Normalize only the intended adapter files from `9f34595b4e`, then finish the
  dashboard route/library/endpoint tests and CLI route for automatic fact
  receipts. No approval, proposal, pending, staged, or human-curation state may
  return.
- Commit the receipt-derived outcomes files if they are not already represented
  exactly by `ee700fae8d`; verify quarantined receipts never fabricate an
  applied projection.
- Finish the deferred pinned digest, dashboard, MCP-memory, and config-scout
  callers using the validated canonical configuration snapshot.
- Verify the automatic-fact module rename, migrations, host runner, policy,
  scheduler, dashboard, CLI, and MCP compile together. Run the focused terminal
  receipt tests and automatic managed-skill lifecycle tests.

### Observability and delivery

- Recover and checkpoint the existing Work lifecycle, retry/leak, blocked
  interval, native integration, fan-out, and reduced rollup emitters without
  duplicating authority. The blocked-interval focused integration had passed
  1/1 before wind-down.
- Reconstruct the delivery settlement checkpoint from tree
  `52f68b889751014e7946f73e6137f3b99b0a5595` only after Work and observability
  stabilize. Preserve RMCP disconnect settlement, durable hook ACK/replay
  identity, cancellation, and terminal CLI ACK behavior.
- Run execution-topology metrics, rollup, compaction, retry, cancellation, and
  restart journeys rather than contract inventories.

### Dashboard, SDK, hosts, and release

- Freeze Rust source first, then run the canonical contract generator and
  `contracts:check`; never hand-edit generated dashboard contracts.
- Finish the Work create/prepare/mutate/evidence UI against the regenerated
  product schemas. Re-run Automations DOM tests after scheduler schemas are
  regenerated. Basic browser usability remains required; screen-reader polish
  is not an RC priority.
- Regenerate SDK operations/types only after Work, TaskSession, terminal
  problems, source edit, retained surfaces, native topology, and automation are
  mounted and compile together.
- Build a fresh binary and rerun supported host install/update/doctor/stock
  journeys for Claude, Codex, Cursor agents/in-composer, Kimi, Kiro as currently
  scoped, and opencode. Do not add Cursor Cloud or expand Kiro scope.
- Complete the default package/install/start journey. npm OIDC setup is the
  explicit remaining operator-owned publication action.

### Backend performance and final verification

- Re-run the remote quota regression after rusqlite-runtime compiles; verify
  cumulative 4,096-event/64MiB transfer bounds, no partial insert on overflow,
  and idempotent replay.
- Refresh the stale performance evidence on the current tree. Run the
  same-host release `perf-gate.sh`, repair the session benchmark hash, execute
  the session temporal benchmark with meaningful repetitions, and add real
  Work rollup latency/throughput evidence. Backend performance is higher
  priority than frontend micro-optimization.
- Build the real dashboard first so `dashboard/app-dist` exists. Then run
  focused package checks followed by
  `cargo nextest run --workspace --all-features --no-fail-fast`, dashboard
  typecheck/tests/build, contract checks, SDK tests, host bundle/stock tests,
  commitlint, release drift, and packaging/install smoke tests.
- Treat zero-test filters, skipped suites, partial runs, timeouts, stale
  artifacts, or synthetic/contract-only evidence as unresolved.

## First actions after reboot

1. Confirm no agent/build process is active; inspect `git status`, the shared
   index, mounts, immutable attributes, `HEAD`, and the coordinator hash.
2. Reconcile the stale shared index to `HEAD` without touching the worktree.
3. Audit `4e4715c09a`, `7913905113`, `9f34595b4e`, and `b9b200eb10` by exact
   paths and patch IDs. Normalize their ownership in additive corrective
   commits; do not reset published history or blanket-revert peer work.
4. Compile `tracedecay-rusqlite-runtime`, then application, query, global-db,
   and usecases. Fix only the first real source failure before regenerating
   contracts.
5. Resume the Work/TaskSession daemon journey and retained owner restoration in
   disjoint lanes, followed by terminal propagation, native topology, source
   edit, automation adapters, observability/delivery, generated contracts, and
   the full verification matrix.

## RC completion condition

V2 RC is ready only when every advertised feature has a real mounted
production caller and direct journey; Work and TaskSession retrieval preserve
sealed identity and reauthorization; automatic skills and memory curation are
terminal and agent-managed without human approval; retained, source-edit,
native-topology, observability, delivery, host, and dashboard surfaces share
truthful typed states; current-tree tests and backend performance evidence pass;
and only external npm trusted-publishing setup remains.

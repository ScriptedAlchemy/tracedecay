# V2 RC reboot handoff

**Status:** active product delivery; local reboot recovery, the live Work
product journey, and the verified dashboard code-graph cutover are complete as
of 2026-08-09. Checkpoints below are current through the 2026-08-14 window,
including the typed delivery-evidence slice, external TLS listener
hardening, the runtime-identity ABA repair, the automation
scheduler/settlement hardening wave, the dashboard work-surface TaskSession
journey, markdown lite-index admission, and the cargo+npm unmounted-files
audit.

`00-plan-set-index.md` remains the sole roadmap and acceptance authority. This
file is the current operational handoff updated from direct branch, test, and
live-daemon evidence. Resume from this file and the current branch; do not
reconstruct intent from commit subjects alone.

## Resume invariants

- Branch: `codex/tracedecay-total-redesign-plan` is the current delivery
  branch. `codex/final-v2-closeout` briefly carried the tip on 2026-08-12 but
  work resumed on the redesign-plan branch; as of 2026-08-13 the closeout
  branch is a strict ancestor of the redesign-plan tip (38 commits behind, 0
  ahead) and must not be treated as current.
- Preserve the current shared worktree. It contains substantial unfinished
  peer work. Do not run `git clean`, `git reset`, `git read-tree -u`, checkout
  paths from another revision, or otherwise sweep tracked or untracked files.
- Re-read every owned file immediately before editing. Use a fresh temporary
  index for each coherent commit and verify that `HEAD` has not advanced
  between seeding that index and committing.
- The required coordinator identity:
  `crates/tracedecay-usecases/src/stack_coordinator.rs` matches `HEAD` in the
  worktree as of 2026-08-13; its most recent intentional change is the
  "finalize verified v2 authorities" retrieval cutover (2026-08-13). Verify
  with `git diff HEAD -- <path>` being empty and `git log -1 -- <path>`
  naming that change, not against a pinned content hash — an earlier pinned
  hash in this file predated that cutover and had already drifted.
- No repository file was mounted or marked immutable at wind-down. An earlier
  filename-focused BPF deletion monitor was stopped cleanly.
- Cargo build artifacts were reclaimed after wind-down with direct
  `cargo-reclaim cleanup --all --delete-target --yes /fast /home/zack /tmp`.
  It deleted 408 freshly revalidated target directories totaling
  611,153,508,016 bytes with zero failures, skips, or stale entries. The
  execution report is
  `/home/zack/.local/state/cargo-reclaim/reports/1786301785763-cleanup-execution-sha256-0cc2a4bff51ec62149b4ebea73acbdd1d9c7512f75c6f2230850703bcd4799ab.json`.
  The sccache server was then stopped and its validated local store at
  `/fast/cache/sccache/data` was emptied from 107,373,615,958 bytes to zero.
  All Rust verification after reboot is therefore a fully cold build; do not
  infer a regression merely from the first build's duration.
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
- Canonical mounted TaskSession hydration, four temporal modes, exact
  continuation, rank-final revocation, and typed stale/denied/unavailable
  outcomes are checkpointed in `4215d2009`. The concrete project-session
  evidence mount and exact-scope reuse are in `3574fa695`; verified first-task
  bootstrap and repaired Work invocation fixtures are in `42c4496ab`.
- Atomic provider admission now accepts the product policy revision and the
  registered daemon task-to-provider-session journey hydrates sealed
  TaskSession evidence in `d3b7fb959`. Registered topology equality and the
  repaired admission/synthesis/topology regressions are in `2b3ae407d`.
- The recovered TaskSession stack now compiles cleanly through
  `tracedecay-rusqlite-runtime`, application, query, global-db, and usecases.
  Focused TaskSession query coverage passed 5/5, the non-vacuous rank-final
  revocation regression passed 1/1, and mounted provider-session hydration,
  exact continuation, and registered Work invocation journeys passed 1/1
  each. The graph-db warnings exposed by the query/global-db checks are
  pre-existing cleanup work, not TaskSession failures.
- `RetrieveEvidence` is exercised through MCP/HTTP parity and the typed SDK
  route in `bb7555b1a`. The dashboard now retains the daemon-resolved scope and
  rebinds Work graph reads, commands, and evidence to its exact repository in
  `bbb979b00`; its focused Work suite passed 95/95 with typecheck and production
  build green.
- Live TaskId-rooted `RetrieveEvidence` now crosses both the daemon HTTP and
  dashboard mounts in `3cc0d17e5`. The two surfaces preserve the same verified
  Work root for current, as-of, evolution, and forensic requests, return typed
  stale for a well-formed mismatched graph identity, and conceal a missing task
  as not-found-or-not-authorized. The new assertions live in a focused child
  module instead of growing the already oversized route-conformance source;
  the exact live journey passed 1/1 and its integration target check passed.
- CLI-launched native providers now preserve their sealed provider-session
  identity through execution and settlement in `bff518403`. Recovered workflow
  admission in `76a5da86c` verifies the original command ID and input digest
  before resuming fan-out, rejects conflicting retries, and publishes complete
  non-truncated provider stdout as digest-addressed attempt evidence. The
  mounted advanced-workflow journey now drives a physical daemon process,
  exercises ordered success/failure/cancellation fan-out, synthesizes through
  the canonical Work graph mutation route, imports a real Claude transcript,
  restarts the daemon, and queries current, as-of, evolution, and forensic
  TaskSession evidence. Its live run passed 1/1 in 52.98 seconds; the focused
  provider-artifact regression passed 1/1, the native provider execution
  module passed 16/16, and the daemon integration target compiled. A fresh
  project without an activated evaluated query profile truthfully returns
  typed `Unavailable` in all four modes while retaining the exact attempt,
  provider-session, and artifact receipts; it does not substantiate live
  TaskSession hydration.
- The dashboard Work surface now answers who worked on a task on both
  published mounts in `a68470647`. The named journey
  `the_dashboard_work_surface_answers_who_worked_on_a_task_on_both_published_mounts`
  (`tests/work_route_exposure_conformance.rs:992`, body in
  `tests/work_route_exposure_conformance/work_task_session.rs`) drives a real
  pinned provider, imports the transcript, and grades provider-qualified
  session evidence in all four temporal modes on the daemon and dashboard
  mounts, including after physical daemon restart. The conformance suite
  passed 3/3. Exact continuation and rank-final revocation at the dashboard
  mount remain gated on an activated evaluated federated query authority;
  the `Anchor` continuation variant is unconditionally `Unavailable`.
- The dashboard board and commands now consume only the product graph authority
  in `d0d9f7dcb`; its complete focused Work suite passed 202/202 with typecheck
  and production build green. The public `WorkProjection` snapshot, delta,
  replan-dependencies, and accept-task operations are removed from HTTP,
  dashboard, MCP, CLI, daemon dispatch, Rust/TypeScript SDKs, and generated
  contracts in `1be58ce45`. The root library, dashboard contract drift check,
  SDK codegen drift check, dashboard route tests, and migrated live-journey
  source compile passed.
- The complete 403-binding default application profile is admitted under the
  reviewed 448-binding budget in `a4ce95c0b`. Established tool primitives and
  Observatory pair CLI with MCP, the missing GitHub stack binding is restored,
  retained HTTP bindings publish their real routes, and fresh init uses the
  admission-only status path. The complete catalog composition contract passed
  10/10 and the serialized default discovery payload remains under its measured
  640 KiB regression ceiling.
- The real two-attempt Work product journey is restored in `e51e57469` with
  exact repository selection, one immutable run deadline, canonical product
  events, typed stale CAS, accepted-task closure even when runtime coverage is
  unavailable, and observation-bounded graph reads. The live daemon journey
  passed 1/1; Work route conformance passed 2/2; the policy suite, product graph
  authority suite, generated dashboard contract check, Rust formatting, and
  diff hygiene are green. Product publication remains distinct from executor
  topology and cannot fabricate attempt hydration.
- Pinned Work proposal routing is now exercised through registered daemon
  dispatch in `2e2daa8b0`. The non-vacuous core lifecycle journey passed 1/1
  and proves generation binds the exact current Work graph plus the admitted
  configuration digest/revision. An empty pinned route set returns an
  explained `NoEligibleRoutes` decision with no ranked candidates; it never
  derives a provider route from executable capability alone.
- The obsolete `test-transport` SQLite graph-projection fixture is retired in
  `8b3900960`. It no longer imports the private daemon service, calls deleted
  relational graph reads, or constructs a scheduler without the process
  resident-memory authority. The standalone dashboard composition now
  withholds graph readiness until a verified production projection is mounted.
  The normal feature-gated Work integration journey compiles and passed 1/1
  through the real daemon in 44.38 seconds.
- Obsolete dashboard test helpers for the removed `index_all` and SQLite
  writer lane are deleted in `77b70befa`, and the automatic-fact owner reuse
  is repaired.
- Dashboard graph, structure, and Explorer code reads now open the canonical
  exact-project admitted `CodeGraphProjectionReadPort` directly in
  `8a039e9af`. The shadow `DashboardGraphReadPortV1`, its SQLite adapter and
  query modules, its separate interactive resolver, and all nine relational
  graph fixture writes are deleted. The fixture publishes a real hermetic code
  generation with files, symbol lineage, chunks, and semantic edges. The graph
  HTTP journeys passed 5/5; the generated-contract authority and drift check
  passed 1/1 each; dashboard typecheck and production build passed; focused
  graph-contract, Code workspace, and Explorer suites passed 1/1, 10/10, and
  35/35. Unpublished documentation, columns, edge lines, and file sizes remain
  explicit null/absent evidence rather than relational or zero-valued backfill.
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
- The automatic-curation, memory, and configuration lane is complete on the
  current tree. `203d7cd0b` moves automatic facts and terminal receipts onto
  the canonical store and finishes the dashboard, CLI, and MCP receipt
  adapters; applied, quarantined, replayed, and paraphrased receipt journeys
  preserve immutable outcomes and never project a quarantined fact. The
  focused host-runner suite passed 7/7, receipt terminal suites passed 3/3 and
  1/1, the dashboard receipt endpoints passed 2/2, CLI parsing and RPC passed
  2/2 and 1/1, and the MCP admin route passed 1/1. `6356a78f5` makes the native
  curator consume canonical string identities, removes retired mirror reads
  from current/as-of/delete and bounded vector convergence, preserves the full
  initial-plus-repair backend attempt history, and keeps application error
  source chains. Curator, scheduler, lock, merge/hydration, session-reflector,
  and skill-writer focused suites are green. `5df70d10e` mounts the real daemon
  configuration grant and mutation service in the dashboard endpoint fixture;
  the 2/2 route journey proves pinned revision advancement, stale CAS,
  reread-after-settlement, and typed JSON rejection of all retired policy
  fields. Automatic managed-skill transaction, materialization, and lifecycle
  suites passed 8/8, 1/1, and 16/16; dashboard typecheck and the focused
  Automations receipt tests are green.
- Typed terminal problem validation is checkpointed in `850265033c`.
- Reset-required propagation is no longer downgraded at the Workflow CLI,
  multi-root MCP, generic daemon client, registered HTTP/application,
  configuration, or retained MCP boundaries in `db26adbfa`. Every mapped reset
  now preserves `Never + [Reset]`; the multiline repository scan found no
  remaining `ResetRequired` arm that constructs `Unavailable` or
  `InvalidRequest`. Five focused mapper tests passed 1/1 each.
- Direct `ApplicationProblem` serialization now validates before emitting wire
  data in `a6a016428`, so an invalid admitted terminal cannot bypass envelope
  construction. The generated TypeScript SDK now includes `reset`,
  `partial_effect`, `reset_required`, and required-nullable
  `committed_receipt`, exposes distinct terminal error classes, validates the
  terminal invariants and canonical HTTP statuses, and is byte-idempotent under
  the canonical generator. Application result tests passed 5/5, the focused
  TypeScript terminal journey passed 1/1, TypeScript typecheck passed, the SDK
  codegen drift check passed, and the focused Rust SDK status test passed 1/1.
  The full TypeScript client file remains 27/28 because ten unrelated
  application bindings truthfully regenerate as `schema_unavailable`.
- Explicit Rust SDK trust roots are eagerly parsed as exactly one PEM
  certificate in `71dd669c7`; reqwest's rustls-backed deferred `from_pem` path
  can no longer accept a non-PEM blob as an empty root set. The focused
  regression passed 1/1 and the full Rust SDK library passed 6/6.
- Invocation observations distinguish reset-required from ordinary
  unavailability in `7e31c7a28` for both canonical application problems and
  retained legacy daemon outcomes. Its focused root test passed 1/1 after the
  affected workspace dependencies compiled.
- Retained transport adapters are checkpointed in `90e326c27e`; retained API
  types are included in `7b82a36ff9`.
- The retained daemon owner, direct memory/session/LCM ports, project-open and
  profile factories, receipt projection, and session-refresh effects are
  present in the production composition through `e98ac761f` and `be4581001`.
  The stale session-refresh wire fixture is corrected in `fea89b562`: the
  complete application retained suite passed 26/26 and the daemon-owner suite
  passed 8/8. HTTP owner/API checks passed 1/1 and 2/2; the non-vacuous CLI
  suite passed 11/11.
- Advertised retained MCP translators now verify every exact action binding in
  `0899d7e87`; the full advertised-dispatch regression passed 1/1. Memory
  status is consistently a pure read in the direct dispatch catalog, MCP
  discovery, Cursor allowlist, and operator guidance in `6f6833cdb`; its
  focused catalog, discovery, and canonical-effect regressions passed 1/1
  each. Repair remains a separate daemon-owned background effect.
- Obsolete SessionStart/SessionEnd dispatch and operator documentation are
  absent from the current tree; the intended retained cleanup from the
  contaminated `4e4715c09` checkpoint is preserved without its TaskSession
  reversions.
- Duplicate receipt/revision identity is approved through `642b0221ae`.
- Native topology recovery now matches its persisted authority. `11f77208f`
  replaces all projected rows for one project/repository/scope-set identity,
  so a newer scope-set revision makes the prior exact read stale instead of
  retaining parallel revisions. `426c235ab` deletes the hand-written native
  MCP request schemas and generates all six from the canonical Rust request
  types already used by the TypeScript SDK. `166e8212a` mounts the exact
  branch-revision and worktree-occupancy readers in session-sync restart
  validation and adds a real registered-store close/reopen journey. The exact
  replacement regression passed 1/1, the linked-worktree topology suite passed
  6/6, MCP schema parity and exact-selection checks passed 1/1 each, and the
  complete session-sync focused module passed 12/12. The cleanup recovery
  owner/caller, executable registry reexport/SDK mount, and extracted native
  MCP modules were verified present and compiling rather than recreated.
- Remote transferred-frame quota enforcement is current through `dad2c2ed0`.
  The registered defaults are pinned at 4,096 events and 64 MiB; both event
  and ciphertext overflow refuse the second frame without a partial insert,
  while replay of the accepted frame remains idempotent. The exact remote
  transfer module passed 2/2 after retained rusqlite integration tests were
  aligned with current Work projection, provider-session, mutation, and
  handoff contracts and the retired persisted-routing test was removed.
- The P0 GitHub stack coordinator and anchored corpus remain restored in
  `c70556fe38` and `ec35c90497`; native stack transition producers are in
  `e26461eac6`.
- Generation-bound rename preview/apply is normalized in `738776d2f`. The
  daemon project owner now grants every canonical primitive-read capability,
  exact graph lookup replaces ranked-search identity discovery, internal MCP
  metadata is removed before strict request decoding, and apply preserves the
  caller's accepted preview while the task-local plan authority intercepts
  publication without changing bytes. The production MCP suite passed 6/6
  serialized journeys covering preview digest CAS, accepted apply and
  idempotent replay, stale-source refusal, lexical collisions, unresolved
  cross-module graph hazards, and publication rollback. The exact
  rename-preview route passed 1/1, the root source-edit module passed 35/35,
  application effect/SDK schemas passed 7/7, SDK codegen remained byte-clean,
  and TypeScript typecheck passed. `b66f0009a` additionally makes multi-root
  and Workflow settlement use their executable registries' canonical dispatch
  deadlines; the complete MCP binding suite passed 11/11.
- The deferred production LSP surface rerun is green on the current integrated
  tree. All 13 `lsp_gateway_protocol_test` journeys passed, covering negotiated
  and incompatible context revisions, async correlation, deadline and explicit
  cancellation, shutdown cancellation, expansion namespacing/currentness,
  stale open-document content, equal-generation identity replacement and
  subscription invalidation, same-generation feedback revision notification,
  session-local unsaved edits, and retained standard LSP behavior.
- The session-temporal diagnostic benchmark now runs through the optimized
  bench profile on Linux and macOS while checked-in contract publication
  remains Linux-only. The root-wide fixture binds retrieval to the canonical
  observation-capture policy, names typed hydration failures, and uses the
  production 100,000-work-unit ceiling for its 64 authorized participants.
  The macOS/aarch64 run completed 3 warmups and 30 measured repetitions over
  1,920 records; all-root hydration completed on every repetition. The
  Cargo-free contract check, focused 64-session regression, two host-policy
  tests, and two black-box runner tests are green.
- The 2026-08-11/12 integration window is on the branch: the old broad MCP
  fact-store router is removed in `c061d3b883`; code-index import evidence is
  parser-backed in `0a38113ca9` and verified ignored dependencies are admitted
  in `8b5b0fb8c2`; the final-V2 memory floor is integrated in `ed37756924`
  (376 files) with automatic fact-store curation exposed in `83e495e839`,
  curator results authenticated in `4816851f13`, and curation contracts
  regenerated through the canonical generators in `ebc0dc1b71` and
  `309db960a6`; the production feedback/advisory runtime is mounted in
  `c2c11956d6`.
- The external remote-brain TLS listener is hardened as of 2026-08-12.
  `2b5b61c378` retains the TLS service authority in the daemon authority and
  service owners; `8c87d2a28d` defines `RemoteBrainTlsConfig`
  (`src/daemon/bootstrap.rs:26`, re-exported at `src/daemon.rs:358`), wires it
  through `src/main.rs` and CLI parsing, and lands nine focused listener
  journeys in `src/daemon/http_application_tests/remote_tls.rs` with
  checked-in localhost PEM fixtures: partial/wildcard admission rejection,
  invalid-identity and occupied-address startup refusal, non-private
  key-handle refusal, remote-only route serving with credential-authority
  isolation, connection bounds and incomplete-header expiry, ingress timeout
  excluded from slow handlers, bounded shutdown under stalled and saturated
  conditions, and expiry of saturated non-reading responses. `56fb459a1d`
  closes the same landing window by pointing the sealed-graph measurement
  bench's memfd replacement probe at a public path.
- The typed delivery-evidence vertical slice is mounted end to end across
  2026-08-12/13. `7f95e3f17c` indexes retained provider state, and
  `6af4a34d89` adds `crates/tracedecay-usecases/src/delivery.rs`:
  `ProjectDeliveryReadAuthorityV1` composes exact-scope reads over the
  retained GitHub review store, CI observation store, and release read
  authority under caller-independent bounds (4 PRs, 256 review items, 16 CI
  checks, 256 releases, 16 point reads, 16 MiB source bytes; no provider URL,
  database key, or source identity is caller-selectable) with per-source typed
  states and five direct usecase tests covering stale-head retention, CI
  manifest-replacement rejection, grant-gated release denial, non-GitHub
  credential-destination rejection, and latest/last-complete ownership.
  `4490c37847` projects it in
  `crates/tracedecay-dashboard-api/src/delivery_api.rs`; `fa02db0662` mounts
  the authority in the daemon project runtime and adds the grant-gated MCP
  handler `src/mcp/tools/handlers/dashboard_delivery.rs`; `7f8e2e61c5`
  preserves outer request identity; `4fe37520fd` regenerates the dashboard
  contracts through the canonical generator. `8c570994ba` renders
  `dashboard/src/workspaces/delivery/DeliveryPage.tsx` on the lazy `delivery`
  route, and `0328e58d88` registers the fixture authorities in the
  dashboard-api test harness so the two `tests/dashboard_api_test/delivery.rs`
  journeys exercise real Git reads plus typed unmounted external authority.
  The DeliveryPage DOM suite passed 29/29 on the current tree on 2026-08-13.
  One honest gap remains: `ProjectDeliveryFailureLocalizationSourceV1` is a
  truthful `NotConfigured` stub (`delivery.rs:219-226`) rendered as a typed
  unavailable projection; wiring a canonical failure-localization owner is
  recorded in the observability and delivery lane below.
- Runtime identity is ABA-proof in `43c3911cf3` (2026-08-13).
  `runtime_identity` compared `Arc` addresses, so a runtime rebuilt after
  destructive maintenance could alias its dropped predecessor when the
  allocator reused the address; each `ShardRuntime` now carries a monotonic
  per-process instance number and identity comparisons use it, preserving
  shared-attachment equality while a rebuilt runtime can never alias its
  predecessor
  (`crates/tracedecay-runtime-core/src/store_runtime/{registry,shard}.rs`).
- The 2026-08-14 CI-repair and takeover wave is on the branch (pushed through
  `c5c0a7663`, reviewed end-to-end by an independent read-only pass):
  semantic vector publication builds stage receipts before
  `publish_generation` drops staged rows (`ab01a31e5`, ends the one-batch
  `store_unknown_build` family); the daemon-hosted dashboard listener is
  daemon-process-owned and survives Core→Full remount (`58d62d913`); the
  fleet-wide test-harness daemon-before-init repair (`3163f31e7`) restored
  the previously mass-failing Linux/macOS suites; `tools/call` carries the
  caller's request deadline with a bounded response grace and received
  envelopes are never discarded (`9b92818cc`, pins updated in the transport
  suite); selected-project fact-store writes are denied as
  `not_found_or_not_authorized` instead of committing cross-project
  (`c74e4d8c4`); post-restart and live-ref-switch code-index query authority
  rebinds from the retained serving slot (`22d73d98d`, `e72eacaec`,
  `4e293bac0`, `5b794edff` — graph-replay failure now refuses the stale seat
  instead of being swallowed; `381c91925` keeps scanning identity-matching
  mounts for an installed authority); codex `turn/completed` provider turn
  id is a typed `Option` absence (`20320067f`); Hermes/Kiro hooks dispatch
  their live handlers again instead of the capture-only `{}` path
  (`c5c0a7663`); LSP protocol sessions register a warming owner before a
  sealed census so initialize/shutdown/exit admit during warming
  (`5e222426e`); Windows getrandom mapping, `large_enum_variant` boxing,
    and byte-identical consecutive dashboard builds (`28973da32`,
    `fd5b1dfe8`, `66c69e034`) close the remaining CI job classes.
- The remaining Work/workflow product surface is mounted (2026-08-19, PR
  branch `cursor/mount-workflow-product-surface-2353`). Workflow activation
  runs tool-catalog semantic admission before the lifecycle transition is
  journaled (`workflow_admission`: step operations must resolve in the Work
  executable catalog and `pinned_catalog_digest` must name the live digest;
  `WorkflowDefinitionService::admit_activation` is the one authority both
  activation paths run, and the daemon journey asserts the mounted route
  refuses an uncataloged candidate). The dashboard gained the fourteenth
  workspace, Workflows — definitions, lifecycle compare-and-swaps, and
  `get_run` projections over the mounted `/application/workflow` routes and
  newly generated contracts; handoff and run-control wire types stay
  uncontracted. A19 outcome: Work mounts no integration apply/review/stack
  mutation operation and must not (Plan 36 owns apply/receipt); the Work
  workspace's integration-outcome and stack-capability accounting cards now
  decode the mounted `operation.work.topology_metrics` projection cell by
  cell instead of wearing a stale "read model is not published" absence.
- A18 outcome (2026-08-19): `CodeIndexGenerationPublished` resolves as
  unadvertise for the dashboard surface plus verified-path confirmation for
  the daemon bus. The dashboard SSE variant
  `DashboardEventKindV1::CodeIndexGenerationPublished` was declared-but-unfed
  — no frontend subscriber, omitted from generated contracts — so it was
  deleted rather than thin-wired (`afe627324`). The scheduler-internal
  `CodeIndexGenerationPublishedV1` broadcast keeps its real production
  consumer (the post-mount query-authority waiter in
  `src/daemon/project_composition/code_index_activation.rs`) and is fed only
  after the durable publication compare-and-swap, the verified graph snapshot
  publish (`publish_verified_snapshot`, the Plan 39 watermark advance), and
  the serving swap; retained restores stay `Noop` and never re-broadcast,
  pinned by
  `restart_remount_serves_the_retained_generation_without_republishing`.

## Remaining work by lane

- CERTIFICATION RECORD 2026-08-15 (serial run, clean worktree at
  `d7c4a4c43`/`77076ea07`, isolated from in-flight working-tree edits):
  `grafeo_restart_acceptance` PASS (1/1, 17s);
  `daemon_suite indexing_lifecycle` FAIL —
  `mounted_incremental_lifecycle_preserves_only_complete_compatible_generations`
  times out waiting for a terminal generation (real at committed HEAD; its
  daemon log shows `full_upgrade_degraded` with "Work evidence retrieval
  scope does not match the mounted project session authority", guard from
  `3574fa695` — under active debugging by the session that added
  daemon-log-on-timeout diagnostics in `29a591519`);
  `typed_terminal_restart_acceptance` CLI legs PASS (2/2, incl.
  ResetRequired), WIP transport legs FAIL (2/2, details on the typed-terminal
  bullet below); `daemon_suite advanced_workflow` FAIL at the 900s semantic
  dispatch ceiling with 1801.6s wall — deterministic cost regression, see
  the semantic bullet.

### Work and TaskSession retrieval

- Already implemented at HEAD (verification, not construction): activate a
  real evaluated query profile in the mounted provider-workflow journey, then
  prove task-to-session correlation through both MCP and the typed SDK after
  transcript import and physical daemon restart. The journey lives in
  `tests/daemon_suite/advanced_workflow_journey/task_session.rs` (from
  `76a5da86c` / `29ebe000d`) and already preserves typed `Unavailable` when
  that authority is absent. Do not reconstruct this path.
- DONE 2026-08-14: the dashboard journey extension landed in `a68470647`
  (suite 3/3).
  `the_dashboard_work_surface_answers_who_worked_on_a_task_on_both_published_mounts`
  (`tests/work_route_exposure_conformance.rs:992`, driving
  `tests/work_route_exposure_conformance/work_task_session.rs`) closes "who
  worked on a task", provider-qualified session evidence, all four temporal
  modes, and restart on both published mounts. Two dimensions remain open
  with recorded reasons: exact continuation and rank-final revocation at the
  dashboard mount require an activated evaluated federated query authority
  (`DaemonWorkFederatedQueryAuthorityV1::authority_for`,
  `src/daemon/invocation_state.rs:831`); the `Anchor` continuation variant is
  unconditionally `Unavailable`
  (`src/daemon/work_evidence_retrieval.rs:225-230`).
- DONE 2026-08-15: `ProfileOwnedNoGit` selection poisoning resolved by owner
  ruling (split semantics): reads succeed over the covered slice with a typed
  disclosure; mutations keep a fail-closed refusal typed by its actual cause.
  The covered subset is structurally the journal's covered *prefix* —
  `fold_graph` requires canonical progression, so coverage cannot skip
  events. Landed via merge `75ec80fc7` (`ead7860f4` runtime + application,
  `3fe7344f5` dashboard contracts, `22f054481` unit tests, `b188a4407`
  journey flip): `WorkGraphSelectionCoverageV1::{Complete, Partial}` on all
  four `WorkGraphReadV1` variants
  (`crates/tracedecay-application/src/work_product/read.rs`),
  `covered_prefix`/`load_covered_journal`
  (`crates/tracedecay-rusqlite-runtime/src/work_product.rs`), typed
  `work.selection_coverage_incomplete` refusal on prepare/submit/CreateTask.
  Follow-up landed 2026-08-15 (merge `acf9a3896`): work/history now serves
  the covered journal prefix carrying `WorkGraphSelectionCoverageV1` as a
  second `selection_coverage` field (orthogonal to the existing
  `WorkHistoryCoverageV1`, which is pagination coverage); the exact-scope
  equality guard is retained inside the prefix; empty covered slice reads as
  an empty page with a `Partial` disclosure. Deferred: rename the shared
  type to `WorkSelectionCoverageV1` at a quieter moment (contract churn).

### Typed terminal problem propagation

- Keep the strict core from `850265033c`: ResetRequired is `Never + [Reset]`;
  PartialEffect is `Never + [Reconcile]` with a partial receipt and concrete
  commit proof; envelope construction is fallible and validated.
- Drive real mounted PartialEffect and ResetRequired results through HTTP, MCP,
  CLI, and both SDKs. Prove the partial-effect committed receipt and reset-only
  legal action survive each boundary and physical daemon restart; unit mappers
  and synthetic SDK envelopes are necessary but not final journey evidence.
  DONE 2026-08-19: all four legs are green. The CLI legs were proven at
  `d7c4a4c43` (PartialEffect via the `e56fadeff` lineage; ResetRequired
  un-`#[ignore]`d by `c962cd627`). The HTTP/MCP/Rust-SDK legs in
  `tests/typed_terminal_restart_acceptance/transport_boundaries.rs` pass
  2/2 after two production fixes: a `tools/call` refused at project open
  with `ResetRequired` now answers on the MCP tool surface with the
  canonical problem envelope under the operation's own MCP result contract
  (`mcp_project_open_reset_refusal`; previously a raw JSON-RPC `-32603`),
  and the socket `DaemonInvocationClient` keeps reading an authoritative
  effect over `DAEMON_TOOL_RESPONSE_GRACE` after cancel delivery instead of
  fabricating `ResetRequired` at the two-second shutdown bound — a
  post-cancel transport failure stays the typed indeterminate settlement,
  mirroring `settle_in_process_invocation`. Both journeys prove the
  partial-effect committed receipt and reset-only legal action across a
  physical daemon restart.
- DONE 2026-08-10 (verified 2026-08-13): the ten `schema_unavailable`
  application bindings were repaired in `d2b094ca7` — the primitive-surface
  read operations gained typed schemas in
  `crates/tracedecay-application/src/retrieval/primitive_surface.rs` with
  registration in `retrieval/catalog.rs` (`primitive_executable_schemas`
  covers all 27 primitive read specs, zero gaps). At HEAD the TypeScript
  client suite is 34/34 with `UNAVAILABLE_OPERATIONS` asserted empty at full
  strength, and the canonical generator reproduces the tree byte-identically.

### Retained production surfaces

- Run one physical-daemon restart journey through real project and profile
  stores that exercises memory reads/effects, session-refresh
  begin/status/cancel, and LCM retrieval with exact identity. Carry the same
  requests through CLI, MCP, HTTP, and both typed SDKs.
- In that journey, prove cancellation, unavailable families, partial-effect
  receipts, reset-required, and post-restart reconciliation as externally
  observed typed terminals. The focused owner/application tests prove their
  internal mappings but are not a restart or end-to-end transport receipt.

### Observability and delivery

- Recover and checkpoint the existing Work lifecycle, retry/leak, blocked
  interval, native integration, fan-out, and reduced rollup emitters without
  duplicating authority. The blocked-interval focused integration had passed
  1/1 before wind-down.
- DONE 2026-08-13 (recovered from session evidence 2026-08-14): the delivery
  settlement lane landed via merge `fe110b92a` (`codex/rc-delivery-settlement`,
  `8c55e01be` — transport-boundary settlement journeys over
  `src/daemon/broker_stream_transport.rs` and `src/daemon/hook_v2_replay.rs`);
  the orchestrator verified the four-part contract implemented and its three
  transport journeys green before merging, then hit the spend limit before
  writing this ledger line. The previously cited reconstruction source tree
  `52f68b8897…` is an unreachable orphan object and is no longer needed.
- Run execution-topology metrics, rollup, compaction, retry, cancellation, and
  restart journeys rather than contract inventories.
- RESOLVED-BY-DECISION (2026-08-13): CI failure-localization owner. Ruling —
  do not build a localization source; the retained CI index does not own
  localization state. `ProjectDeliveryFailureLocalizationSourceV1` stays
  `NotConfigured`-only (`crates/tracedecay-usecases/src/delivery.rs:219-235`)
  with its doc comment now naming the explicit owner: failure-localization
  evidence belongs to a future CI-annotation ingestion source, and this
  composition intentionally reports `NotConfigured` until that source exists
  and is explicitly retained here. Consumers must render it as typed
  unavailable (the dashboard already does). No further action pending the
  ingestion source landing.

### Additional wound-down lane handoffs

- DONE 2026-08-15: the TypeScript SDK is regenerated for the Work-read
  `selection_coverage` addition through the canonical generator; the
  codegen drift check is green again (it was red because the Rust side
  landed without the regen) and the full TypeScript client suite passed
  34/34. The Grafeo restart/isolation journey re-passed twice more and
  the Hermes stock TAP re-passed 1-25 against the current tip's fresh
  binary, corroborating the 08-14 closures below on the merged lineage.
- DONE 2026-08-14: Grafeo memory relations remain mounted in `f0708a7fda`
  with profile/project identity, CAS projection, hydration, and dashboard
  consumption. The full daemon restart/isolation journey
  `tests/grafeo_restart_acceptance.rs::memory_relation_graph_survives_physical_daemon_restart_and_isolates_profile_and_projects`
  passed 1/1 in 57.250s (`nextest` run `e08aa083-8519-464a-bfc5-b5c6a0bad3a2`)
  under isolated `CARGO_TARGET_DIR=/tmp/grafeo-rerun-target` after
  `bff4c108d` committed the fixture checkout, requested an authoritative
  reconcile, and retried warming identity. Selected-project writes stay
  denied (`c74e4d8c4`). Do not replace this journey with the narrow
  registry test.
- DONE 2026-08-14: exact-route Hermes plugin, unit, and stock checks closed
  against a fresh isolated binary (`CARGO_TARGET_DIR=/tmp/hermes-close-target`,
  `tracedecay 0.0.73+623a12cbcd51`, stock Hermes `9dd9ef0ec99a`). CI's
  hermes-integration job is green end to end: `hermes_plugin_unit_check.py`
  39/39, `hermes_stock_integration.sh` TAP 1-25 PASS, `hermes plugins list`
  shows tracedecay enabled, and `tracedecay doctor` Hermes section is clean.
  TAP 25 (`sync_turn` → `tracedecay_lcm_grep`) was not semantic-publication
  fallout (`semantic_publication_failure` / `store_unknown_build` absent after
  `ab01a31e5`); retained LCM grep used default multi-MiB `ExecutionLimits` and
  failed `within_request_budgets` as persistent `application.retained.saturated`.
  `abc6b23e6` caps those limits to the admitted 64KiB request budget. Exact-route
  + PATH isolation remain in `ded031f58` / `455a3aed7` (merge `9673f03db`).
- DONE 2026-08-13: the provider decoding/materialization work in
  `github_runtime/stack.rs` landed committed in `74ebfe3cc` (canonical stack
  identities) and `92d1d7225` (verified V2 authorities — the restored V3
  coordinator identity this lane's resume invariants name). Verified: zero
  stubs, decoded fields materialize through the typed domain snapshot with
  fail-closed bounds, the production mount runs owner refresh through the
  daemon advisory runtime, and all eight named journeys pass (13 tests across
  `github_stack_coordinator`, `github_stack_drift_observability`,
  `github_stack_anchor_authority`; `advisory` lib suite 84/84).
  RESOLVED (2026-08-13): the open decision handed to the daemon lane —
  `StackSignalKindV1::PotentialConflict` and `::AuthorizationLost` (plus
  `StackDriftKindV1::{DependencyMissing, RefDeleted,
  WorktreeGenerationChanged, Unknown}`) had no producer anywhere and no
  persisted analytics/rollup data could contain them (each was introduced,
  never wired to a producer, and verified by full-repo grep before removal).
  Ruling was retire, not implement: the variants and their match arms were
  removed from `crates/tracedecay-usecases/src/stack_coordinator/transition.rs`
  and `crates/tracedecay-domain/src/observability/execution.rs` (plus the
  mirrored `ExecutionStackDriftKindV1` projection in
  `crates/tracedecay-application/src/execution_topology_metrics/mod.rs`). Note
  `GitHubStackDeliveryStateV1::AuthorizationLost` (the delivery layer) is a
  separate, fully-wired enum and was not touched.
- DONE 2026-08-12 (verified 2026-08-13): the canonical-parent cutover landed
  — `src/application.rs` was removed in `540b6a605` and `05924ecdf`, and the
  tree carries zero `crate::application` facade imports in `src/`. Residual
  crate-split cleanup (open SEAMS: agent-hosts packaging seam, dashboard-api
  inversions, root-wiring lists) is tracked separately and is not part of
  this cutover's contract.
- Semantic configuration table ownership and activation reconciliation are
  implemented. The mounted fan-out journey
  `mounted_fan_out_recovers_then_synthesizes_and_hands_off` cleared
  `store_unknown_build` (`ab01a31e5`) and chunk-contract 2170 (`e7a740457`).
  On a quieter host (load 23→18, 96 cores) the product client dispatch
  ceiling of 300s in `evaluate_and_publish_semantic_profile` still fired;
  raising it measured honest FastEmbed 1x+10x `evaluate-and-publish` wall
  time of 625s. The ceiling is sized to 900s (625s + margin) as
  `SEMANTIC_EVALUATION_DISPATCH_DEADLINE_MICROS`. After evaluation
  completes the journey now dies at `activate_evaluated_semantic_profile`
  with CLI exit 2: `semantic evaluation publication rejected:
  InvalidRequest`, mapped from `SemanticActivationCoordinationErrorV1::Rejected`
  in `semantic_evaluation_response` (the underlying `SearchEvalError` is
  swallowed by `evaluate_default_candidate`). Exact, lexical, graph, and
  ordinary session retrieval must stay available while semantic activation
  is pending or unavailable. Measurements: 1811s wall on a heavily contended
  host (2026-08-14), then 1801.6s on a quiet host (2026-08-15, clean worktree
  at `d7c4a4c43`) — within 0.5% of each other despite very different load.
  Measured diagnosis (probes `37b7e5be2`, `c55a4da7e`, `64bfeef39` on
  `codex/rc-semantic-timing` — supersedes two earlier wrong theories, "work
  tripled" and "2×900s ceiling-clamp"): the wall matches
  `SEMANTIC_EVALUATION_ISOLATED_DISPATCH_DEADLINE_MICROS` = 1800s
  (`src/daemon_client.rs:417`, used by `tracedecay-search-eval-direct`) hit
  ONCE. Under it: the driver runs 12 isolated passes (3 profiles × 2
  partitions × 2 scales, `crates/tracedecay-search-eval/src/lib.rs:196`),
  each with a fresh TempDir+registry
  (`semantic_runtime/production.rs:541`) that defeats a digest memo which
  otherwise works (`recover_verified_snapshot` short-circuits, O(1)); each
  pass publishes 4 generations with 2 unconditional full-corpus digests
  (`recovered_generation_digest_from_database`, linear at 682µs/chunk —
  5.5s per full 768-d digest at 22k chunks, NOT a 900s clamp) ≈ ~990s of
  digest+publish plus ~96 DB close/reopen cycles; FastEmbed inference is
  the unmeasured remainder. RESOLVED 2026-08-15 (merge `ddf5fff36`, lane
  `codex/rc-semantic-timing`): (a) digest lever CLOSED-BY-RULING — the
  prepare-time digest is the baseline the post-reopen durability proof
  verifies (`replay.expected_recovered_digest`, publication.rs:514/567);
  exact-value incremental hashing is impossible (content-derived
  lexicographic entity order ≠ page arrival order) and per-page subdigests
  would change the persisted digest value, failing every published
  generation's own durability verification. Owner accepted the blocker:
  the digest algorithm stays; post-sharing residual digest cost (~40s)
  does not justify a persisted-evidence break. (b) LANDED — corpora
  published once per scale and shared (4→2 lexical publishes, 12→2
  projection-case sets, 12→2 incremental measurements; ~207s structural
  savings at quiet-box costs) plus projection-case and
  incremental-projection memos whose provenance is generation-id/digest
  composed (honest under sharing; binding checks reject mismatched cache
  entries). (c) CLOSED — no cross-pass re-embedding exists;
  `prepared_native` cache already keys by generation id (= content digest
  + pinned model). Ceilings untouched. REMAINING: one end-to-end
  `advanced_workflow` run on a quiet host to measure the true un-clamped
  wall (the pre-change wall was censored at the 1800s clamp, so savings
  cannot honestly be subtracted from it).
- Re-run the doctor authority-audit journey and a clean Cursor agents/in-
  composer install -> version bump -> doctor lifecycle. Preserve Cursor Core
  drift versus ownership-conflict distinctions and do not add Cursor Cloud.
- Re-run the full Grafeo, feedback SDK, workflow-metadata privacy, structured
  privacy, Costs accounting, LSP, and application final-surface suites in the
  aggregate matrix even where their focused review lanes approved. Their
  approval proves the scoped change, not current-tree RC integration.
- Exercise incremental indexing through save, rename, delete, ref switch,
  overflow, cancellation, and restart. Preserve serve-during-refresh and exact
  identity; only complete compatible semantic generations may publish.
- IN-FLIGHT 2026-08-14: `cd35ad9e9` admits markdown structure into the lite
  index (`crates/tracedecay-code-extraction/src/markdown_structure.rs` and
  `crates/tracedecay-code-index/src/languages.rs`). Grep-surface visibility
  of `.md` content is being fixed separately — not done.
- DONE 2026-08-14: `tracedecay_unmounted_files` generalized to cargo+npm
  ecosystems with correct `#[path]` semantics in `144f1d0ff` (43/43 focused
  tests across `src/mcp/tools/handlers/analysis/unmounted_files.rs` and
  `unmounted_files/{rust,typescript}.rs`).

### Dashboard, SDK, hosts, and release

- DONE 2026-08-14: contract freeze verified on the current tree. After Rust
  source settled, `npm run contracts:generate` then `npm run contracts:check`
  reproduced the checked-in dashboard contracts byte-identically (no schema
  drift; generated files unchanged). Isolated `scripts/check-sdk-codegen.sh`
  (`CARGO_TARGET_DIR=/tmp/contracts-pass-target`) also reproduced the SDK
  tree with no diff. Never hand-edit generated dashboard contracts.
- DONE 2026-08-14: Automations DOM tests re-run after that freeze.
  AutomationsPage, transport, schedulerDispatchScope, RunHistory, and
  KnowledgeCuration passed 41/41; `npm run typecheck` passed; the full
  dashboard vitest suite passed 1608/1608 (144 files); production
  `npm run build` succeeded in 3.09s. Basic browser usability remains
  required; screen-reader polish is not an RC priority.
- Regenerate SDK operations/types only after Work, TaskSession, terminal
  problems, source edit, retained surfaces, native topology, and automation are
  mounted and compile together.
- DONE 2026-08-14: supported host install/update/doctor/stock journeys rerun
  against a fresh isolated binary (`CARGO_TARGET_DIR=/tmp/host-stock-target`,
  `tracedecay 0.0.73+afe627324a73.dirty` after `f214e1a89`). Hermes stays the
  earlier close (`8d9d57f2e`). CI mirrors: `scripts/claude_stock_integration.sh`
  PASS (local Claude Code 2.1.232; CI pin 2.1.224), `claude plugin validate
  plugin --strict` PASS, `scripts/opencode_stock_integration.sh` PASS (stock
  OpenCode 1.18.4 matches CI; host `debug config` accepted 8 agents +
  `duplicateAnalyzerAvoidance`; `mcp list` connected). Cursor agents/in-composer
  (not Cloud): packaged-manifest + JSON parse PASS; default install was failing
  closed because `embedded/extension.js` (1.15 MiB) exceeded the 1 MiB artifact
  cap — `f214e1a89` raises it to 2 MiB; rerun installed Core+Agent+Context MCP
  (8 agents, in-composer `mcp.json`, native extension), `update-plugin`
  refreshed 3 components, and `tracedecay doctor` Cursor section is clean.
  Codex: stage + stock `codex plugin add tracedecay@personal` installed/enabled
  0.0.73; doctor Codex section clean (hooks remain host-untrusted until
  `/hooks`). Kimi: global install stages
  `~/.tracedecay/host-bundle-stage/kimi/tracedecay` and defers to interactive
  `/plugins install` (in scope); `update-plugin` ok; stock `kimi doctor` PASS.
  Kiro as scoped: typed `kiro-cli` absence (not on PATH; not expanded). Shared
  `--local` is a typed unavailable (`project-local host lifecycle is
  unavailable`); do not add Cursor Cloud. Remaining gaps: Kimi/Kiro doctor
  stay silent until `~/.kimi-code` / `~/.kiro` exist; Kiro's typed absence is
  wrapped by a secondary `StorageFailure` at
  `host_component_registration.rs:197`.
- DONE 2026-08-14: local default package/install/start journey is green on
  this branch (isolated `CARGO_TARGET_DIR=/tmp/package-smoke-target`).
  Validated existing `dashboard/app-dist`; built the production release
  binary (`--no-default-features --features production --locked`);
  `package-release-archive.py` produced `tracedecay-v0.0.73-x86_64-linux.tar.gz`
  (109M); installed into a temp prefix; daemon-first `tracedecay init` +
  `projects context` against a temp fixture; `mcp-conformance-smoke.sh` and
  packaged LSP-bridge initialize both passed on the installed binary.
  `check-release-drift.sh` aligned at 0.0.73; last 12 commits commitlint-clean;
  workspace `cargo package --allow-dirty --no-verify --exclude-lockfile`
  produced 29 crates; SDK `npm pack --ignore-scripts` produced
  `@tracedecay/sdk@0.1.0`. npm OIDC setup is the explicit remaining
  operator-owned publication action.
- The npm publish WORKFLOW side is complete: `92d701086` (2026-08-07) rewired
  the release publish job to tokenless OIDC trusted publishing — no NPM_TOKEN
  anywhere, a policy gate forbidding token/.npmrc/registry-url auth, and
  falsification tests. Only the operator's GitHub/npm trusted-publisher setup
  remains. Operator setup checklist (recovered; never previously written):
  on npmjs.com, add a trusted publisher for the SDK package pointing at
  ScriptedAlchemy/tracedecay's release workflow (`release.yml`) and
  environment; no token secrets are to be created; verify with a dry
  workflow run that publish authenticates via OIDC.
- RC verification protocol provenance (recovered 2026-08-14 from Codex thread
  019fded1, 2026-08-11): an "Ubuntu Linux RC matrix" protocol was defined
  (clean checkout `/fast/builds/tracedecay-v2-rc-linux`, per-SHA target dirs,
  isolated TRACEDECAY_DATA_DIR, ff-only merge gate) with perf-gate budgets
  (6 workers × 60s; index ≤900s; warm p95 ≤10s; max call ≤60s; RSS ≤6144MB;
  errors ≤5%; ≥10k nodes) and the finding that NO Work-rollup measurement
  script exists. Shard branches `codex/rc-linux-verify` and
  `codex/rc-work-rollup-project-shard` carried partial work (a stash for the
  latter survives as `stash@{0}`); no full green pass was executed and the
  clean-checkout state predates the reboot/reclaim.
- Operator-owned items (recovered 2026-08-14 from the 2026-08-08 session
  record; fleet must not pick these up): npm trusted-publisher/OIDC setup
  (operator said "later today" on 2026-08-08; no transcript reports it done);
  the corrupted `30445febbe` commit-body reword (needs force-with-lease, only
  on the operator's word after a completed generation); stash drops; an
  operator-machine doctor pass; and truthful human screen-reader/usability
  notes for the RC. Release/publishing workflows stay operator-owned per the
  standing npm OIDC boundary rule. PR #421 is the branch's only CI surface
  (`ci.yml` triggers on master pushes and PRs only) with
  `cancel-in-progress: false` accepted until RC for complete matrices.
- RESOLVED-BY-DECISION (2026-08-13): `src/doctor.rs:953` domain symbol
  extraction self-report. Ruling — keep the safe degradation, retire the
  "unimplemented" wording. `domain_symbol_rules_warning` now reports domain
  symbol extraction as a truthful typed capability-unavailable statement
  ("Domain symbol extraction is unavailable: no extractor reads {path}...",
  consistent with neighboring Doctor unavailable-surface wording), with
  identical pass/fail (`dc.warn`) semantics. No implement-or-retire decision
  remains open.
- RESOLVED (2026-08-14): the daemon-hosted dashboard not resuming after a
  physical daemon restart (observed 41321→36165, connection refused) was a
  lifecycle-ownership bug — Core→Full project remount retired the just-bound
  process-global listener via `McpServer::shutdown_background_tasks_until`.
  Fixed in `58d62d913`: the hosted dashboard is now shut down only as a
  daemon-process owner (`hosted_dashboard`, next to `http_application`), not
  on project-server retirement. Verified by isolated kill/respawn repro:
  post-restart relaunch URLs accept and hold.

### Backend performance and final verification

- Refresh the remaining stale performance evidence on the current tree. Run
  the same-host release `perf-gate.sh`, publish the repaired session benchmark
  from a clean Linux `--refresh-contract` run, and add real Work rollup
  latency/throughput evidence. Backend performance is higher priority than
  frontend micro-optimization.
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
3. Build `dashboard/app-dist` before the cold Rust build where `build.rs`
   requires it; all Cargo target directories were intentionally reclaimed.
4. Audit `4e4715c09a`, `7913905113`, `9f34595b4e`, and `b9b200eb10` by exact
   paths and patch IDs. Normalize their ownership in additive corrective
   commits; do not reset published history or blanket-revert peer work.
5. Compile `tracedecay-rusqlite-runtime`, then application, query, global-db,
   and usecases. Fix only the first real source failure before regenerating
   contracts.
6. Resume the Work/TaskSession daemon journey and retained owner restoration in
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

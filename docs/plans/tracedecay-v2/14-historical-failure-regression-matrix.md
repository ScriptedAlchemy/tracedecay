# TraceDecay V2 Cross-Cutting Regression Contract

## Status / Role

Status: active cross-cutting test contract.

Role: preserve observable failures learned from V1 and dogfooding while PR5 through
PR20 replace and optimize implementation. This is a compact ownership map, not a numbered failure
ledger or compatibility inventory.

## Outcome

The rewrite cannot declare a slice complete by passing its happy path while reviving a
known corruption, routing, privacy, scope, lifecycle, or truthfulness failure.

## Owns

- Observable failure classes that must remain represented in direct product tests.
- The PR slice responsible for preventing, exposing, and recovering from each class.
- The rule that a historical fix is evidence for a test, not architecture to copy.

## Does not own

- Numbered inventories, contiguous IDs, plan parsers, generated status views, or CI
  validation of Markdown.
- A second test runner, compatibility generator, migration ledger, or release catalog.
- Exact legacy file paths, line numbers, snapshots, PR heads, or implementation recipes.
- Product behavior already owned by the implementation plans.

## Required behavior

Each row names the observable failure class and the implementation PR whose direct tests
must cover prevention, visible state, retry or recovery, and restart behavior.

| Owner | Required regression classes |
|---|---|
| PR5 | Partial, malformed, duplicated, truncated, reset, or replaced provider input never advances beyond a complete sanitized frame; restart resumes without gaps. |
| PR6 | Remaining providers preserve native identity/order; projection replay and backpressure never duplicate, skip, or corrupt observations. |
| PR7 | Facts, memory, and stable anchors never cross owners; copied prompts never become authorship; correction, redaction, and deletion preserve safe lineage. |
| PR8 | Temporal/LCM reads never repair storage; copies, summaries, supersession, cursors, stale shards, and no-result states remain truthful. |
| PR9 | Code generations are deterministic; exact identifiers and phrases are not displaced by parse errors, echoes, wrong snapshots, or uncalibrated shard scores; stale, cross-generation, and dirty-overlay diagnostics never publish as current or enter clean generations. |
| PR10 | Semantic search never substitutes models, crosses privacy domains, recomputes unchanged documents, or shortens lexical results after model failure. |
| PR11 | Policy, application, settings, catalog, analyzer execution, and analyzer configuration remain authorized, deterministic, idempotent, privacy-safe, and free of alias-local business logic. Branch-aware feedback-cycle results ([Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)) never collapse new/pre-existing diagnostics, coverage state, or termination reason into a guessed clean result; post-edit diagnostics-and-impact is the first pillar of the PR11–PR13 read-only/advisory milestone. |
| PR12 | CLI, MCP, HTTP, output, and the [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) LSP gateway agree on lifecycle, framing, capabilities, protocol/catalog versions, cancellation, schemas, defaults, errors, pagination, formats, and nonzero failure status; notifications cannot satisfy pending responses; a method outside the supported capability set, or one the active analyzer declares unsupported, returns an explicit unavailable outcome rather than a guessed result; `prepareRename`/`rename` candidates never apply through `workspace/applyEdit` or an opaque server command. [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md) gateway and explicit diagnostics-call triggers surface the same typed feedback-cycle findings on LSP/MCP/CLI as the post-edit diagnostics-and-impact pillar. |
| PR13 | Hooks stay fast and thin; Scout and host bundles preserve address, privacy, lifecycle ownership, and effects without local query/model/storage work; only clean-generation or saved-content semantic evidence may commit to Scout envelopes, checkpoints, feedback records, observations, facts, memory, telemetry payloads, spools, caches, replicas, or exports — dirty-overlay or unsaved-secret semantic evidence must return typed suppressed or unavailable state and never durably persist hover, signature, diagnostic, or reference content; conflicting extension claims require safe discovery, explicit replacement confirmation, configuration preservation, and rollback; Claude Code, Cursor desktop, Cursor cloud, and Codex each receive their capability-specific LSP/native-diagnostics/hook surfacing path without being forced to a lowest-common-denominator behavior; Hermes and Kiro report hook/MCP/CLI or unavailable paths explicitly and are not assumed to receive full LSP. [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md) completes the PR11–PR13 read-only/advisory milestone with all four pillars: post-edit diagnostics+impact (PR11–PR12), CI-failure localization, read-only GitHub review-comment/thread ingestion and symbol-remapped surfacing, and tiered concurrent-agent proximity. TraceDecay never posts, updates, resolves, replies to, or dismisses GitHub comments; attempted writes produce separate `policy=denied` and `effect=suppressed` outcomes before any GitHub call and never populate GitHub ingress lifecycle or provider-outcome fields. No `posted`, `updated`, `dismissed`, or `replied` state exists; `resolved` exists only as an observed read-only lifecycle value. GitHub item/thread lifecycle is exhaustive and typed: `current`, `outdated`, `resolved`, `edited`, and `deleted`. GitHub ingress provider outcome is separately exhaustive and typed: `complete`, `partial`, `unavailable`, `denied`, `rate_limited`, `stale`, and `failed`; ingress `denied` means read authorization denial only. The [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) semantic-evidence provider states remain a third, separate set — unsupported, absent, indexing, stale, cancelled, timed-out, failed, and partial versus supported plus completed plus complete-coverage zero-findings. GitHub fixtures cover thread/reply lifecycle, bot versus maintainer authorship, edited/deleted/resolved/outdated states, exact versus symbol-remapped stale binding, and rate-limit/auth/ETag/restart recovery without persisting comment bodies. CI localization carries typed provenance, stale/partial/unavailable log states without log content, and never claims CI authority. Proximity fixtures cover exact-match and Plan 20-owned risk-threshold above/below tiers, advisory-only semantics with freshness/expiry, and never create a lock or schedule. All four pillars surface through LSP, agent hooks, MCP, and CLI when their owning PR ships; each trigger is one-shot with no automatic continuation or fix. |
| PR14 | Dashboard, Doctor, observability, and configuration views use canonical daemon operations, distinguish loading/complete-zero/ready/stale/error/locked/partial/denied/unauthorized/redacted/conflicting/offline/unknown/cancelled/timed-out/unsupported-schema, and offer executable recovery; the one unqualified Doctor application kernel, UI, and remediation consume typed Scout, host finding, GitHub-ingested review-thread, CI-localization, and proximity state emitted by PR13, then add PR17 auxiliary-provider health from Plan 20 configuration state, Plan 27 discovery/conformance/remediation evidence, Plan 32 lease/attempt/runtime evidence, and Plan 26 observations. This plan is the direct regression contract for that PR14 kernel, not the runtime kernel itself. Auxiliary health covers unsupported/absent/stale executable, executable/protocol drift, invalid fallback, sandbox/environment/capability mismatch, restart/reconnect/resume failure, stuck lease/attempt, provider availability, and desired-versus-observed configuration drift. Plan 27 supplies probes and confirmed remediation operations; Plan 32 supplies runtime evidence; Plan 11's frontend consumes the canonical findings; none defines another Doctor kernel or health formula. Table-driven direct tests cover the complete canonical semantic-evidence provider state set — unsupported, absent, indexing, stale, cancelled, timed-out, failed, and partial — and none of those states may render as `complete_zero_findings`; only supported plus completed plus complete coverage with zero findings may render `complete_zero_findings`. [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md) dashboard/Doctor read models add the canonical GitHub item/thread lifecycle (`current`, `outdated`, `resolved`, `edited`, `deleted`) and ingress provider outcome (`complete`, `partial`, `unavailable`, `denied`, `rate_limited`, `stale`, `failed`) as separate dimensions; CI localization provenance without log payloads; proximity emitted/suppressed/expired/risk-class state with Plan 20 setting provenance; and table-driven lifecycle/outcome/LSP projection fixtures consistent with Plans 37 and 35. Outbound GitHub write denials/suppressions remain separate policy/effect outcomes and never appear as GitHub ingress state. |
| PR15 | Explicit repository/worktree/ref and LSP workspace-folder targets never fall back to CWD, first workspace, or active checkout; cross-project results exact-load globally; dirty/stale graph and multi-root diagnostic coverage is explicit. [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md) multi-root/cross-project scope-isolation binds each feedback-cycle trigger, GitHub remap, CI localization, and proximity warning to its exact owning root with per-root branch/worktree/head/generation identity; ambiguous, denied, stale, or unsupported roots return typed unavailable or partial coverage with no fallback to another root; cross-root proximity and privacy scoping never leak another session's content. |
| PR16 | Remote authority, offline replay, cache verification, backup, restore, and failover never admit two writers or hide incomplete coverage; unsaved LSP content, overlays, and analyzer state remain node-local and never enter spools or replicas. [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md) node-local overlay and proximity computation stay on the workspace node; durable saved-content feedback, GitHub-ingested evidence, and CI-localization evidence are fenced through shard authority with restart/failover, retention/deletion, and authorization recheck on every anchor or handle expansion; remote partial or unavailable coverage is explicit and never substitutes a cached or replica projection as current; overlays, proximity state, and session-only feedback never migrate into spools, caches, or replicas. |
| PR17 | [Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md) task/work identity, versions, DAG readiness, projections, task-shape/decomposition/resize proposals, task-domain ready-node/decomposition/sizing/model-backend recommendation semantics, typed auxiliary-attempt requests, and proposal/graph-transition history remain canonical; Plan 06 owns pure evaluator/policy-decision mechanics; [Plan 26](26-observability-accounting-and-usage.md) owns model-capability profiles, provider observations, the canonical independent-review/task-outcome label vocabulary and measurement schema, and calibration read models; and [Plan 32](32-dynamic-workflow-runtime-and-sdk.md) scheduling, provider-adapter execution, history, leases, attempts, effects, artifacts, retries, and cancellation share one daemon runtime authority and never duplicate observable effects. Regressions cover ambient-board/CWD scope, copied task or parent/session identity, dependency loss, stale leases and late receipts, wrong worktree/branch, status-column readiness, locally invented/coerced outcome labels, runtime-completion-as-accepted-outcome, proposal-as-mutation, review-as-completion, cancellation/effect unknown, hidden route/backend substitution, self-grading, recursive self-dispatch, task inflation, omitted integration overhead, unsafe parallel decomposition, sparse/private or shifted cohorts, exact model/executable/protocol version drift, censored/unknown outcomes, selection/override bias, non-causal route claims, privacy leakage, opaque weight mutation, shell-string/argv injection, inherited environment or secret leakage, malformed/out-of-order/truncated provider streams, missing heartbeats, kill-escalation failure, unsafe restart/resume, and nondeterministic fallback. Claude-designated execution must use native Claude Code rather than Hermes Anthropic; Codex app-server is preferred and CLI fallback is explicit and policy-eligible. Live split/merge/resize/re-route evidence and auxiliary output must produce anchored evidence only; stale or unapproved proposals and provider output cannot change the graph or runtime. Kanban/DAG/timeline/causal/workload/model-calibration/attempt views must agree on the same versioned entity set, proposal disposition, Plan 26 label/schema revision, provider identity, terminal outcome, and history. Plan 37 advisory operations consumed as typed workflow steps are already shipped at PR13; PR17 composes them without becoming first owner and performs no GitHub writes. |
| PR18 | Rust, TypeScript, and Python SDKs preserve the public contract, cancellation, retries, privacy, and transport-neutral errors. |
| PR19 | Migration and cutover leave one writer and one canonical route, preserve rollback evidence, reject stale clients, and remove every superseded path. |
| PR20 | Performance optimization never weakens semantics, authority, privacy, ordering, coverage, durability, or crash/restart correctness and cannot hide tail/resource regressions behind averages. |

The following adjudicated counterexamples extend those rows without creating a
second Doctor, query engine, scheduler, or evaluation platform:

- **PR9:** parse caps and unsupported grammar remain partial; Tree-sitter reuse
  never proves identity; ambiguous rename/move/split/merge lineage abstains;
  syntax/name-resolved/analyzer/dynamic/heuristic edge authority remains
  distinct; and affected-test candidate modes never imply execution or safety.
- **PR10:** exact flat-vector scan remains a valid baseline/oracle; ANN,
  reranking, and quantization cannot regress protected exact, no-answer,
  wrong-scope, privacy, low-coverage, or zero-recall-tail strata; semantic
  failure preserves lexical bytes and ordering.
- **PR11/PR12:** fixtures cover grant expansion and expiry, adversarial PR
  framing, origin/destination impact disagreement, retained-but-ranked
  findings, `duplicate_noop`, stream gaps, cancellation races, and unknown
  schema values.
- **PR14:** unknown denominators never render zero, 100%, healthy, or clean;
  uniformly huge files do not become healthy through equality; fragmentation
  is not modularity; member count alone is not God Class; severity may
  disagree with evidence quality; dispatch is not verified recovery; and
  permissive/default and optional renderers preserve semantic selection,
  anchors, scope, coverage, truthful states, and keyboard access.
- **PR15–PR17:** frozen shard-vector paging, bridge provenance, authorization
  revocation, topology baselines, coupling abstention, minimal repair,
  selective escalation, harmful experience recall, verifier gaming,
  independent minority-review preservation, least privilege, and route
  propensity safety remain direct failure fixtures.
- **PR20:** open-loop overload prevents coordinated omission; A/A noise floors,
  cache-layer protocols, mixed insert/update/delete/retraction equivalence, and
  fixed build-invalidation classes remain direct gates.

Diagnostic publication across these rows is idempotent and version-monotone by
document/generation. Duplicates converge, stale updates cannot overwrite newer
state, and reconnect may redeliver current state; no plan claims exactly-once
LSP/network delivery.

## Flagship dashboard regression fixtures and gates

These are binding direct product-test assets for
[Plan 11](11-dashboard-frontend.md). They are typed API fixtures maintained
with the owning DTOs, not generated from this Markdown and not a second
dashboard/Doctor/query/task authority.

### Fixture manifest

`dashboard/test/fixtures/dashboard-state-taxonomy.json` contains exactly these
fixture IDs:

- `surface.loading`, `surface.complete-zero-findings`, `surface.ready`,
  `surface.partial`, `surface.stale`, `surface.locked`, `surface.denied`,
  `surface.unauthorized`, `surface.redacted`, `surface.conflicting`,
  `surface.offline`, `surface.unknown`, `surface.cancelled`,
  `surface.timed-out`, `surface.error`, and `surface.unsupported-schema`;
- `semantic.unsupported`, `semantic.absent`, `semantic.indexing`,
  `semantic.stale`, `semantic.cancelled`, `semantic.timed-out`,
  `semantic.failed`, `semantic.partial`, and
  `semantic.supported-completed-complete-coverage-zero-findings`;
- `github-lifecycle.current`, `github-lifecycle.outdated`,
  `github-lifecycle.resolved`, `github-lifecycle.edited`,
  `github-lifecycle.deleted`, `github-ingress.complete`,
  `github-ingress.partial`, `github-ingress.unavailable`,
  `github-ingress.denied`, `github-ingress.rate-limited`,
  `github-ingress.stale`, `github-ingress.failed`,
  `outbound.policy-denied`, and `outbound.effect-suppressed`.

`dashboard/test/fixtures/planner-parallel-source-progress.json` contains
`planner.validating`, `planner.parallel-mixed-progress`,
`planner.partial-zero`, `planner.complete-zero-findings`,
`planner.source-unavailable`, `planner.cancelled`,
`planner.stale-event-ignored`, `planner.reconnect-redelivery`, and
`planner.revision-gap-refetch`. Each fixture identifies run/event/revision,
required sources, independent source outcomes, known/unknown denominators,
coverage, omissions, canonical ordering, and finality. The aggregate can
render `complete_zero_findings` only when every required source is supported,
completed, and complete-coverage with zero matches.

`dashboard/test/fixtures/evidence-packet-matrix.json` contains
`evidence.complete-cited`, `evidence.partial-zero`,
`evidence.unknown-denominator`, `evidence.mixed-score-kinds`,
`evidence.uncalibrated-score`, `evidence.high-rank-no-confidence`,
`evidence.retriever-unavailable`, `evidence.all-citations-omitted`,
`evidence.redacted-citation`, `evidence.locked-citation`,
`evidence.cross-authority-disagreement`, and
`evidence.unsupported-schema-revision`. Every case carries retriever
contributions, why-this-result reason codes, freshness, score kind/revision,
coverage counts, omissions, and citations explicitly; absent evidence is data,
not whitespace.

`dashboard/test/fixtures/evidence-expansion-states.json` contains
`expansion.available`, `expansion.redacted`, `expansion.locked`,
`expansion.unauthorized`, `expansion.denied`, `expansion.stale`,
`expansion.revoked`, `expansion.expired`, `expansion.missing`,
`expansion.corrupt`, `expansion.partial`, and `expansion.error`.
`dashboard/test/fixtures/late-context.ndjson` contains
`late.same-generation`, `late.stale-generation`, `late.revision-gap`,
`late.rank-change`, `late.anchor-revoked`, and `late.anchor-superseded`.
Every expansion rechecks authorization; payload bytes never appear in fixture
URLs, local-storage snapshots, analytics, query keys, or durable cache
assertions.

`dashboard/test/fixtures/deep-link-state-matrix.json` contains
`deep-link.valid`, `deep-link.expired`, `deep-link.revoked`,
`deep-link.ambiguous-scope`, `deep-link.denied`, `deep-link.stale-version`,
`deep-link.historical-version`, and `deep-link.no-ambient-fallback`. Every
case preserves or rejects exact scope, selection, graph/entity version,
valid/observation time, filter revision, lens, and anchor IDs. No case falls
back to CWD, the first workspace, active checkout, current version, title,
card index, or renderer coordinates.

`dashboard/test/fixtures/github-feedback-matrix.json` contains
`github.thread-maintainer-current`, `github.reply-bot-current`,
`github.thread-outdated`, `github.thread-resolved`, `github.thread-edited`,
`github.thread-deleted`, `github.binding-exact`,
`github.binding-symbol-remapped-stale`, `github.ingress-complete`,
`github.ingress-partial`, `github.ingress-unavailable`,
`github.ingress-denied`, `github.ingress-rate-limited`,
`github.ingress-stale`, `github.ingress-failed`, `github.etag-unchanged`,
`github.etag-changed`, `github.reconnect-redelivery`,
`github.restart-redelivery`, `github.outbound-policy-denied`, and
`github.outbound-effect-suppressed`. It asserts no comment-body persistence
and no outbound GitHub call for denied/suppressed writes.

`dashboard/test/fixtures/projection-parity.json` binds stable scope, entity,
edge, cluster, selection, evidence-anchor, legal-action, valid/observation
time, and graph/version identities across default graph, semantic table,
Brain, Explorer, Loom, Code, and PR17 Kanban/DAG/timeline/causal/workload
projections. It includes `projection.filtered-selected`,
`projection.outside-current-page`, `projection.denied-anchor`,
`projection.cluster-hidden-members`, and `projection.temporal-dual-time`.

`dashboard/test/fixtures/renderer-fallback.json` contains
`renderer.default-permissive`, `renderer.webgl-unavailable`,
`renderer.init-timeout`, `renderer.context-lost-twice`,
`renderer.restore-timeout`, `renderer.budget-exceeded`,
`renderer.optional-cosmograph-unavailable`, and
`renderer.optional-cosmograph-parity`. The optional adapter fixture runs only
when that adapter is included, but the default/fallback path always runs and
must expose every feature and legal action.

`dashboard/test/fixtures/work-projection-matrix.json` contains
`task.ready`, `task.blocked-dependency`, `task.blocked-needs-input`,
`task.blocked-capability`, `proposal.pending`, `proposal.accepted`,
`proposal.rejected`, `proposal.superseded`, `proposal.stale`,
`route.abstained`, `route.fallback-recommended`, `route.fallback-selected`,
`route.version-drift`, `review.independent`, `review.non-independent`,
`outcome.censored`, `outcome.unknown`, and
`outcome.insufficient-coverage`. These are projections of Plan 24/26/32 wire
states, not locally declared substitutes for their canonical enums.

`dashboard/test/fixtures/auxiliary-attempt-matrix.json` contains
`attempt.unsupported`, `attempt.absent`, `attempt.stale`,
`attempt.lost-heartbeat`, `attempt.malformed-stream`, `attempt.cancelled`,
`attempt.timed-out`, `attempt.failed`, `attempt.partial`,
`attempt.unknown-termination`, `attempt.resume-unavailable`,
`attempt.version-drift`, and `attempt.completed-unaccepted`. Every case keeps
request versus attempt lineage, requested versus actual provider/backend/
executable/protocol/model/reasoning identity, stream coverage, authority
epoch, and terminal receipt visible.

`dashboard/test/fixtures/doctor-source-disagreements.json` contains
`doctor.unsupported-provider`, `doctor.absent-executable`,
`doctor.stale-executable`, `doctor.executable-drift`,
`doctor.protocol-drift`, `doctor.invalid-fallback`,
`doctor.environment-mismatch`, `doctor.capability-mismatch`,
`doctor.provider-degraded`, `doctor.restart-failed`,
`doctor.reconnect-failed`, `doctor.resume-failed`,
`doctor.stuck-lease`, `doctor.unknown-attempt`,
`doctor.config-valid-executable-absent`,
`doctor.version-outside-range`,
`doctor.capability-supported-sandbox-denied`,
`doctor.fallback-missing`, `doctor.fallback-forbidden`,
`doctor.host-repaired-attempt-stuck`,
`doctor.runtime-healthy-telemetry-partial`, and
`doctor.resume-receipt-stale`. Each yields the one canonical finding family,
source-attributed evidence/coverage, and owner-supplied legal actions.

### Execution-topology exhaustive matrix

The PR17 Work execution-topology contract has three checked-in sanitized
assets:

- `dashboard/test/fixtures/execution-topology-matrix.json` contains static
  topology snapshots and expected visual/table manifests;
- `dashboard/test/fixtures/execution-topology-events.ndjson` contains ordered,
  duplicated, delayed, stale, gap, restart, and dual-time event sequences; and
- `dashboard/test/fixtures/integration-operation-matrix.json` contains
  application-supplied dry-run/apply/cancel action references and receipts.

Every static record contains `id`, opaque TaskId/WorkItemVersionId and graph/
topology versions, exact fixture-local repository/worktree/branch/ref/base/
head/merge-base aliases, repository-snapshot digest, dirty/worktree/lease
state, lane-family support, dependency commits, proposed and observed merge
order, mechanical-conflict and semantic-proximity evidence, required/
observed tests and CI, valid and observation time, watermarks, coverage,
omissions, evidence anchors, legal-action IDs, and the expected normalized
`topologyManifest`. Fixture aliases are random and non-joinable; no real path,
branch name, commit subject, actor, prompt, source, patch, conflict body, test
log, or private session content is present.

Every row below is mandatory. “No apply” means the application payload
contains no `RequestApply`; the test does not merely hide a client-created
button. “Preserve” means the same TaskId, work-item/plan/graph/topology
versions, repository snapshot, valid/observation time, watermarks, coverage,
and anchors survive visual, table, inspector, playback, and deep-link
round-trip.

#### Worktree lifecycle and stack topology

| Fixture ID | Required assertion |
|---|---|
| `topology.worktree.unsupported` | Worktree lanes remain visibly unsupported; task lanes and totals remain available; no empty/clean inference. |
| `topology.worktree.absent` | Cataloged repository with no observed worktree renders absent with source coverage, not zero work. |
| `topology.worktree.unborn` | Unborn HEAD has no invented commit/ref/merge base and offers no integration apply. |
| `topology.worktree.attached-clean` | Exact attached ref, HEAD, generation, clean state, and complete coverage render independently of readiness and lease. |
| `topology.worktree.dirty-unstaged` | Unstaged dirty state is visible in lane, truth strip, table, inspector, and dry-run preconditions. |
| `topology.worktree.dirty-staged` | Staged dirty state is distinct from unstaged and does not imply a Plan 36 commit receipt. |
| `topology.worktree.dirty-mixed` | Staged plus unstaged state remains mixed; neither layer is dropped or collapsed to generic dirty. |
| `topology.worktree.dirty-untracked` | Untracked presence is explicit without persisting or previewing untracked content. |
| `topology.worktree.ignored-only` | Ignored-only state remains separate from tracked dirtiness and does not expose ignored names/content. |
| `topology.worktree.dirty-renamed-mode` | Rename and file-mode evidence remains typed and cannot be reconstructed from display paths. |
| `topology.worktree.dirty-submodule` | Submodule dirtiness/availability is explicit and cannot be flattened into parent clean state. |
| `topology.worktree.dirty-sparse` | Sparse-checkout coverage and excluded paths remain visible; unknown coverage is not clean. |
| `topology.worktree.detached` | Detached HEAD preserves object identity and disables attached-ref assumptions. |
| `topology.worktree.conflicted` | Native unmerged/conflicted state remains separate from semantic proximity and blocks unsupported mutation. |
| `topology.worktree.locked` | Native worktree lock and reason are separate from Plan 32 lease state and expose only supplied legal actions. |
| `topology.worktree.prunable` | Prunable is an observed lifecycle state, never permission for browser cleanup or branch deletion. |
| `topology.worktree.removed-active-attempt` | Removed/missing worktree plus active attempt renders conflicting/stale evidence and never rebinds to another checkout. |
| `topology.lease.none` | No runtime lease is distinct from queued, unavailable, or lost lease and cannot imply readiness. |
| `topology.lease.current` | Current lease displays opaque lease identity, authority epoch, freshness, and attempt relation without PID authority. |
| `topology.lease.stale-fenced` | Stale fenced lease remains historical and cannot expose current cancel/reclaim controls. |
| `topology.lease.authority-epoch-changed` | New authority epoch invalidates old actions/receipts while preserving old evidence. |
| `topology.lease.released-terminal` | Released terminal lease remains linked to its receipt and does not appear active. |
| `topology.lease.effect-unknown` | Lease/effect uncertainty blocks replacement and success until owner reconciliation. |
| `topology.stack.unsupported` | Stack lanes remain visibly unsupported while worktree/task projections retain identical entities. |
| `topology.stack.single` | A one-branch stack renders one canonical item reference, not a copied task or inferred dependency. |
| `topology.stack.linear-current` | Linear stack preserves exact dependency commits and application-supplied merge order. |
| `topology.stack.branched-current` | Branched stack preserves fan-out/fan-in and does not flatten to a misleading line. |
| `topology.stack.missing-dependency-commit` | Missing required commit is explicit, blocks clean/integrated copy, and links exact evidence. |
| `topology.stack.order-partial` | Known order plus omitted/unknown segments remains partial and never receives a complete ordinal. |
| `topology.stack.cycle-rejected` | Invalid cycle returns the typed owner error; UI does not repair, linearize, or render it as a valid stack. |
| `topology.stack.base-deleted` | Deleted base ref retains object/history evidence, marks current stack stale, and does not choose another base. |
| `topology.stack.protected-target` | Protected target exposes denial/policy provenance and no client-synthesized apply. |
| `topology.stack.branch-deleted-commit-retained` | Deleted branch label disappears from current refs while retained commit/task/receipt history remains addressable by immutable identity. |

#### Test and CI authority states

| Fixture ID | Required assertion |
|---|---|
| `topology.check.required-missing` | Missing required test/check is a blocker with owner evidence, never an inferred failure or pass. |
| `topology.check.queued` | Queued is distinct from running and exposes no invented progress percentage. |
| `topology.check.running` | Running preserves provider/run/head identity and cannot prove work acceptance. |
| `topology.check.passed` | Passed is an observed provider outcome for the exact head and does not prove semantic correctness or task acceptance. |
| `topology.check.failed` | Failed retains safe localization/provenance and an inert rerun hint; browser never reruns CI. |
| `topology.check.cancelled` | Cancelled remains terminal for that run/attempt and is not failure, pass, or zero evidence. |
| `topology.check.timed-out` | Timed out is distinct from cancelled/failed and retains partial coverage. |
| `topology.check.partial` | Partial jobs/pages/log coverage cannot satisfy a required-check gate. |
| `topology.check.stale-head` | Result for an old head remains historical and cannot satisfy the current proposal. |
| `topology.check.unavailable` | Provider/log unavailable renders unavailable with unknown outcome, not missing/clean. |
| `topology.check.denied` | Denied expansion exposes no private run/log existence or content. |
| `topology.check.rerun-observed` | New CI attempt is linked to the prior run and remains an observed external effect, not a dashboard action. |

#### Drift, retarget, concurrent edits, and conflict truth

| Fixture ID | Required assertion |
|---|---|
| `topology.drift.head-advanced` | Changed source HEAD marks prior proposal/dry-run stale; no silent refresh or apply. |
| `topology.drift.base-advanced` | Changed base object preserves old preview, exposes successor evidence, and requires a new dry run. |
| `topology.drift.merge-base-changed` | Changed merge base invalidates conflict and order evidence even when branch labels are unchanged. |
| `topology.drift.worktree-generation-changed` | Same path/label with a new worktree generation never reuses old lease, dirty, action, or receipt identity. |
| `topology.retarget.preview-current` | Retarget preview displays old/new target snapshots, affected dependency commits/tests, expiry, and effect-free status. |
| `topology.retarget.stale-cas` | Apply against a changed graph/ref/snapshot fails stale with zero mutation and one refresh action. |
| `topology.retarget.denied` | Known denied target exposes no hidden branch/worktree content, counts, conflicts, or apply. |
| `topology.retarget.partial` | Partial destination graph/Git coverage cannot claim safe, clean, or no semantic conflict. |
| `topology.integration.cross-merge-proposal` | Proposal preserves exact source/target snapshots, dependency commits, merge order, alternatives, conflict/test evidence, expiry, and Plan 24 disposition without mutating Git. |
| `topology.integration.cross-merge-dry-run` | Read-only native dry run remains a preview with mechanical/semantic coverage and no apply when the owner does not support merge mutation. |
| `topology.integration.cross-merge-external-receipt` | Externally observed native integration receipt links exact resulting snapshot and remains distinct from any TraceDecay request. |
| `topology.integration.receipt-partial-checks` | Native success plus partial required tests/CI remains mechanically integrated but not verified or accepted. |
| `topology.integration.proposal-superseded` | Superseded proposal retains history and loses every apply/cancel action tied to the old revision. |
| `topology.concurrent.disjoint` | Disjoint exact addresses render independently; absence of a warning is backed by complete eligible coverage. |
| `topology.concurrent.same-file` | Same-file overlap renders the immediate proximity class without inventing a lock or assignment. |
| `topology.concurrent.same-range` | Same-range overlap preserves both authorized contribution IDs and exact expiry. |
| `topology.concurrent.same-symbol` | Same-symbol overlap links graph evidence and remains advisory. |
| `topology.concurrent.shared-callers-tests` | Threshold-tier relation paths, pinned threshold revision/digest, score kind, and coverage are visible. |
| `topology.concurrent.below-threshold` | Below-threshold result is a covered no-warning outcome, not missing telemetry or zero risk. |
| `topology.concurrent.private-other-session` | Only coarse overlap class is visible; actor/session/root/address/content and hidden count are absent. |
| `topology.conflict.none-complete` | “No predicted conflict” appears only when both mechanical and semantic required sources are complete/current. |
| `topology.conflict.mechanical-only` | Native mechanical conflict can be present with complete semantic no-overlap; dimensions remain separate. |
| `topology.conflict.semantic-only` | Clean mechanical dry run can coexist with semantic overlap/impact risk and cannot be labeled safe integration. |
| `topology.conflict.both` | Mechanical and semantic findings retain independent severity, evidence, coverage, and actions. |
| `topology.conflict.mechanical-unknown` | Unknown native conflict denominator never renders clean, 0%, green, or apply-ready. |
| `topology.conflict.semantic-partial` | Partial graph/proximity evidence remains partial despite a clean native dry run. |
| `topology.conflict.semantic-denied` | Denial exposes no candidate or address cardinality and does not downgrade mechanical evidence. |
| `topology.conflict.false-positive` | Adjudicated clean outcome links to the prior prediction for precision accounting without rewriting history. |
| `topology.conflict.false-negative` | Observed conflict without a prior positive prediction links as a false negative for recall accounting; UI does not conceal it. |

#### Action, crash, restart, and duplicate-effect behavior

| Fixture ID | Required assertion |
|---|---|
| `operation.dry-run-effect-free` | Dry run returns immutable preview/digest and records zero Git/ref/index/runtime/test/CI effect. |
| `operation.apply-owner-supported` | Apply appears only from an owner-supplied action ref and carries every expected version, authorization, confirmation, and idempotency field. |
| `operation.apply-unsupported-merge-absent` | Plan 36 merge/rebase/cherry-pick plans expose dry run only; no DOM, keyboard, deep-link, or crafted-client apply path exists. |
| `operation.cancel-before-commit` | Accepted cancellation before the native commit point returns cancelled and proves unchanged authoritative state. |
| `operation.cancel-after-commit` | Cancellation racing after commit point returns the committed receipt, never a false unchanged/cancelled state. |
| `operation.duplicate-same-digest` | Repeated same-key/same-digest request returns the original operation/receipt and one observable effect. |
| `operation.duplicate-different-digest` | Same key with a different command digest returns idempotency conflict and no second effect. |
| `operation.stale-preview` | Any graph/item/repository/ref/lease/policy drift rejects the complete operation; no partial best-effort apply. |
| `operation.lease-locked` | Conflicting live lease/authority returns locked with exact safe current evidence and no reclaim inference. |
| `operation.effect-unknown` | Sent-without-receipt remains effect-unknown, blocks replacement/success, and offers only owner-supplied reconciliation/cancel controls. |
| `operation.crash-before-dispatch` | Crash before dispatch resumes as not-dispatched/cancelled or safely retryable with no effect receipt. |
| `operation.crash-after-admission-before-effect` | Durable admission resumes without duplicate dispatch and keeps the original deadline/idempotency identity. |
| `operation.crash-after-effect-before-receipt` | Unproved effect becomes effect-unknown and cannot be replayed or called success. |
| `operation.crash-after-commit-before-response` | Restart compares native state and replays the one committed receipt without a second mutation. |
| `operation.stale-late-receipt` | Receipt from an old authority epoch remains historical stale evidence and cannot settle current state. |
| `operation.duplicate-provider-effect-prevented` | Duplicate outbox/provider delivery settles once; prevented duplicate and committed effect counts remain separate. |

#### Delivery fanout, privacy, platform identity, and retention

| Fixture ID | Required assertion |
|---|---|
| `topology.delivery.hook-only` | Hook delivery remains complete with no LSP connection and binds exact root/branch/generation. |
| `topology.delivery.lsp-only-trigger` | LSP save trigger projects the same canonical event identity and cannot create topology authority. |
| `topology.delivery.hook-lsp-dedupe` | Hook and LSP copies converge by event/revision without duplicate card, warning, receipt, or metric effect. |
| `topology.delivery.five-surface-redelivery` | Hook/MCP/LSP/dashboard/CLI redelivery preserves one result and permits duplicate transport delivery only. |
| `topology.delivery.multi-root-scoped` | Per-root events never fall back to the first workspace; ambiguous roots return partial/unavailable. |
| `topology.delivery.stale-lsp-ignored` | Late old-generation LSP event cannot overwrite current Work/dashboard state. |
| `topology.delivery.lsp-disconnected` | Disconnect marks only LSP delivery coverage; hook/MCP/dashboard/CLI and canonical history remain truthful. |
| `topology.privacy.overlay-session-only` | Dirty overlay topology/proximity is visible only to its owning session and enters no durable event, packet, metric, cache, or playback frame. |
| `topology.privacy.cross-session-coarse` | Cross-session proximity reveals only approved coarse class/freshness and no actor, task, root, path, range, symbol, or content. |
| `topology.privacy.denied-no-existence` | Denied task/root/anchor is externally non-enumerating with no hidden counts or lane placeholders. |
| `topology.privacy.redacted-anchor` | Redacted evidence retains only the approved metadata allowlist and safe typed reason. |
| `topology.privacy.secret-canary` | Canary is absent from DOM, accessibility tree, events, URL, storage, analytics, fixtures, and request history. |
| `topology.privacy.metric-no-identity` | Metrics contain no TaskId, run/attempt, actor, project/repository, path, branch/ref, commit, model version, or reversible digest label. |
| `topology.privacy.no-url-or-storage-payload` | Patch/conflict/test/CI/source bytes never enter URL, query key, local storage, IndexedDB, service worker, or durable browser cache. |
| `topology.privacy.expansion-auth-revoked` | Authorization loss replaces expanded content immediately with denied/revoked and preserves only legal safe metadata. |
| `topology.platform.repo-moved-same-identity` | A moved repository with proved canonical identity preserves task/history and updates only observed locator metadata. |
| `topology.platform.repo-moved-ambiguous` | Ambiguous move returns unavailable/ambiguous and never picks by basename, CWD, or recent use. |
| `topology.platform.symlink-alias-same-identity` | Symlink alias and real root resolve one canonical repository/worktree identity without duplicate lanes. |
| `topology.platform.symlink-escape` | Escaping symlink is denied/unsupported before content or action disclosure. |
| `topology.platform.windows-drive-case` | Drive-letter and case normalization preserve canonical identity without changing display evidence. |
| `topology.platform.windows-case-collision` | Case-colliding paths/refs are conflicting/unsupported and never merged into one cell. |
| `topology.platform.windows-unc-root` | UNC repository/worktree identity survives deep link, restart, and event replay without URI/path confusion. |
| `topology.platform.windows-long-path` | Long-path capability is explicit; unsupported access remains partial/unavailable, never absent. |
| `topology.platform.windows-cross-volume-worktree` | Cross-volume linked worktree either retains exact native identity or reports typed unsupported with no path fallback. |
| `topology.platform.windows-separator-normalization` | Slash differences do not create duplicate identities or leak raw paths into events/metrics. |
| `topology.retention.ref-force-moved` | Ref movement preserves old object/proposal/receipt history, marks current projection stale, and never rewrites past frames. |
| `topology.retention.stack-archived` | Archived stack remains historical and non-executable; current lane totals do not silently include it. |
| `topology.retention.worktree-pruned` | Pruned worktree retains immutable task/commit/receipt references while current lifecycle becomes missing/pruned. |
| `topology.retention.expired-detail` | Expired optional detail returns an expired tombstone and aggregate coverage, not an empty successful inspector. |
| `topology.retention.receipt-retained` | Owning-entity receipt remains queryable after branch/worktree deletion according to its retention class. |
| `topology.retention.redacted-tombstone` | Privacy deletion retains only the permitted non-reversible tombstone; branch/path/actor/task details are absent. |
| `topology.retention.historical-ref-missing` | Playback of a deleted ref uses retained object/event evidence and explicitly marks current ref unavailable. |

`dashboard/test/fixtures/execution-topology-events.ndjson` contains exactly
`event.snapshot-monotone`, `event.duplicate-redelivery`,
`event.stale-generation-ignored`, `event.revision-gap-refetch`,
`event.dual-time-late-observation`, `event.follow-live-pause-step-seek`,
`event.operation-receipt-replay`, `event.ref-deleted-history-retained`, and
`event.queue-overflow-stale-refetch`. Each sequence starts from a named static
fixture, identifies stream/event/entity revisions and watermarks, and ends in
one expected manifest digest. Completion order and duplicate transport
delivery cannot change that digest.

Scale and stream assets are deterministic checked-in generators plus hashes:
`dashboard/test/fixtures/generators/graphScale.mjs` emits 1,000/2,000,
10,000/25,000, 50,000/150,000, and 100,000/300,000 node/edge tiers;
`dashboard/test/fixtures/generators/sseChurn.mjs` emits 100 events/s for ten
minutes, 1,000/s for ten seconds, duplicates, out-of-order revisions,
redelivery, and queue overflow. Generator output is presentation-load input,
never canonical product evidence.

### Required DOM and accessibility assertions

`dashboard/test/dashboard-state-matrix.vitest.tsx` table-renders every
`surface.*`, `semantic.*`, and GitHub fixture. It asserts an always-visible
`data-authority-state`, labeled text, source revision/watermark, freshness,
coverage/omission values, and supplied legal-action IDs. Loading uses
`main[aria-busy=true]` and a named status. Partial/stale/offline use named
status; ready uses a named region with visible state text; cancelled and
timed-out use a named status plus only application-supplied retry/action
references; locked/denied/unauthorized/conflicting/error/unsupported-schema
use an alert; redacted uses a note and contains no secret canary. Every
semantic/GitHub outcome renders its literal typed outcome in a labeled
definition list. Unknown renders literal “Unknown” and no percentage.
`complete_zero_findings` renders only for the full canonical predicate. No
required state exists only in color, icon, tooltip, hover, canvas pixels,
animation, spatial order, opacity, CSS pseudo-content, or an expanded drawer.

`dashboard/test/planner-progress.vitest.tsx` asserts independent source rows,
known-denominator progress only, stage/elapsed/cancel after 500 ms, partial
results that remain labeled partial, stale event rejection, reconnect
deduplication, and one refetch on a revision gap. It fails if the browser
invents a source, merges/reranks results, rewrites server order, or calls
partial zero “no results.”

`dashboard/test/evidence-provenance.vitest.tsx` asserts that every compact
result's truth strip shows authority, freshness, coverage, citation and
omission counts, and score kind. It proves mixed score kinds are not averaged
or placed on one scale; heuristic/ordinal/uncalibrated values never render
confidence or probability; calibrated probability retains calibrator,
revision, cohort, horizon, support, and drift validity; server rank does not
become confidence; missing citations remain visible.

`dashboard/test/evidence-expansion.vitest.tsx` and
`dashboard/test/late-context.vitest.tsx` assert authorization recheck,
focus return, accessible source/locator names, stale-successor explicit
choice, immediate revoked-content removal, monotone packet revisions,
focus-preserving late-context announcements, stale-generation suppression,
and revision-gap refetch. Redaction canaries must be absent from DOM,
accessible names, snapshots, URLs, browser storage, telemetry, and MSW request
history.

`dashboard/test/deep-link-states.vitest.tsx` asserts exact round-trip identity,
historical rendering, typed stale/revoked/expired/ambiguous/denied outcomes,
and no ambient fallback. `dashboard/test/github-feedback.vitest.tsx` asserts
lifecycle/ingress/policy/effect separation, authorship, stale binding,
ETag/restart convergence, no-write network history, and no comment-body
persistence.

`dashboard/test/projection-parity.vitest.tsx` compares normalized
`ProjectionManifest` values rather than pixels. Visual, table/text, renderer,
and Work lenses must preserve the same stable IDs, selection, scope, versions,
coverage, anchors, state, route identity, terminal outcome, and legal actions.
An omitted projection member remains counted and typed, never silently
dropped.

`dashboard/test/execution-topology-matrix.vitest.tsx` table-renders every
`topology.*` fixture through lane/rail/heatmap, semantic table, TaskId
inspector, and truth strip. It asserts identical entity and edge IDs, exact
scope/version/time/watermark/anchor round-trip, separate dirty/worktree/lease/
readiness/runtime/conflict dimensions, optional lane-family states, visible
omissions, and no path/label/card-order/current-checkout fallback.

`dashboard/test/execution-topology-actions.vitest.tsx` renders every
`operation.*` fixture and inspects MSW history. It proves that each request is
copied from one supplied legal-action reference, includes exact expected
versions and idempotency identity, and makes at most one typed application
call. It asserts zero Git/CI/provider calls from the browser; dry run is
effect-free; unsupported operations have no apply action; stale/denied/
locked/effect-unknown remain terminal or owner-recoverable; duplicate same-
digest requests return one receipt; and cancellation after commit preserves
the committed receipt.

`dashboard/test/execution-topology-playback.vitest.tsx` replays every
`event.*` sequence in original, reversed-delivery, duplicated, and
restart-redelivered order. It asserts the expected manifest digest,
valid-time/observation-time separation, one refetch per revision gap/overflow,
stale-generation rejection, focus/selection retention, and effect-free
pause/step/seek/follow-live controls. Historical branch/worktree deletion
never erases retained commit, TaskId, proposal, or receipt evidence.

`dashboard/test/authority-negative.vitest.ts` uses AST/import-boundary checks
to reject renderer/Kanban imports of policy, Doctor evaluation, command
construction, task evaluators, provider/runtime adapters, persistence, or
storage. It rejects identity from array/draw/card/lane/title/path/session/
branch/PID; local rank/cluster/causality/readiness/critical-path/health/
coverage/route/action/remediation calculations; browser task/board/runtime
stores; optimistic graph/runtime/recovery mutation; lane/order/process-exit/
heartbeat/provider-output completion; adapter-state persistence; browser
source merge/rerank; stale deep-link/proposal rebasing; and visual/table legal-
action drift.

Playwright files
`dashboard/test/e2e/dashboard-critical-journeys.spec.ts`,
`dashboard/test/e2e/dashboard-responsive.spec.ts`,
`dashboard/test/e2e/dashboard-keyboard.spec.ts`,
`dashboard/test/e2e/renderer-fallback.spec.ts`,
`dashboard/test/e2e/work-projection-parity.spec.ts`, and
`dashboard/test/e2e/recovery-resume.spec.ts`, and
`dashboard/test/e2e/execution-topology.spec.ts` cover Brain → finding → evidence
→ Doctor → supplied action → preview → confirmation → dispatch → receipt →
verified recovery; planner query → partial results → Loom; diagnostic → exact
generation/anchor/affected tests; graph/table selection parity; Work
cross-lens identity; stale proposal CAS; topology lane/table/TaskId parity;
dry-run/apply/cancel authority; crash receipt recovery; dual-time playback;
and reload without duplicate dispatch. `@axe-core/playwright` runs every
universal state and critical
journey. Screenshots are supplemental and cannot satisfy authority,
provenance, coverage, redaction, network, or semantic parity assertions.

Responsive/a11y tests use Plan 11's exact viewport, zoom, keyboard,
reduced-motion, forced-color, target-size, focus, live-region, virtual-row, and
manual NVDA/Firefox plus VoiceOver/Safari gates. Performance tests use Plan
11's pinned runner, graph tiers, payload/frame/latency/long-task/heap/SSE
budgets, not averages that hide p95, tail, leak, or queue-overflow failures.

### Cross-view authority invariants

All direct tests enforce:

1. Supported + completed + complete coverage + zero findings is the only
   `complete_zero_findings` predicate.
2. Severity, evidence quality, confidence/score kind, coverage, freshness,
   completeness, omissions, uncertainty, and authority remain separate.
3. Scope, entity/finding/Task/attempt/lease/anchor identity, branch/worktree/
   head/generation, versions, and legal actions survive navigation and fallback.
4. SSE/query publication is idempotent and version-monotone; stale delivery
   cannot win and reconnect does not imply exactly-once.
5. Plan 20 owns desired/effective configuration, Plan 27 owns discovery/
   conformance and host remediation references, Plan 32 owns runtime, Plan 26
   owns measurements/labels/calibration, Plan 24 owns work state, and the PR14
   Doctor application owner owns findings. The dashboard renders supplied
   state/actions and this plan only binds regressions.
6. Dispatch, receipt, and post-operation verification remain separate; no
   polished success treatment can call dispatch or process exit recovery.
7. Runtime completion is not accepted work outcome; review is not completion;
   proposal acceptance is not optimistic graph/runtime mutation.
8. Graphical quality, animation, clustering, card lanes, renderer choice, and
   friendly empty artwork can never hide missing authority, partial coverage,
   omissions, stale scope, inaccessible evidence, or illegal actions.
9. Native Git, Plan 24, Plan 32, test, and CI authority remain separate. The
   browser may request only an owner-supplied typed action; it never constructs
   a mutation, infers an effect, or turns dry-run/proposal/dispatch into a
   native/test/CI receipt.

### Milestone and command gates

PR14 Gates A through F and PR17 Gates A through D from Plan 11 each land the
fixtures and direct tests for their owned slice before the milestone closes.
PR14 Gate C cannot close without planner/evidence/late-context/projection
tests; PR14 Gate D cannot close without Doctor/recovery authority tests; PR14
Gate F cannot close without accessibility, fallback, budgets, SSE, and
usability records;
PR17 Gate C cannot close without every `topology.*` and `event.*` case, exact
TaskId drill-down, platform identity cases, and the topology performance
budget. PR17 Gate D cannot close without every `operation.*` case, duplicate-
effect/crash recovery, authority-negative checks, Plan 26 metric parity, and
zero privacy canary leaks. PR17 cannot close without Work/attempt/proposal
parity and all provider-state fixtures. PR20 may optimize implementations but
may not relax or average away these gates.

The implementation retains/updates the existing dashboard scripts and adds
the exact dependencies, configuration/harness files, script bodies, execution
order, manual assistive-technology records, usability schema/results, and
performance protocol named in Plan 11. Test and measurement scripts fail if
zero cases or samples execute and print fixture/state/sample counts; build and
Cargo check fail on compilation or validation errors:

```bash
npm --prefix dashboard run test:acceptance
cargo test --all-features --test dashboard_api_test
cargo check --all-features
```

## PR17 auxiliary-provider Doctor contract

The Doctor application kernel shipped by the PR14 product slice remains the
only product-health authority for PR17 auxiliary providers. This plan remains
its regression contract, not an executable kernel:

- Plan 20 supplies desired/effective configuration revision, validation,
  provenance, and desired-versus-observed activation state.
- Plan 27 supplies observed executable/provider availability, version/protocol/
  capability/sandbox evidence and references to confirmed install/update/
  repair/rollback operations.
- Plan 32 supplies lease/attempt authority, progress/heartbeat, cancellation/
  kill, reconnect/resume, terminal receipt, and stuck/unknown runtime evidence.
- Plan 26 supplies denominator-safe availability, failure, latency, drop, and
  recovery observations.
- Plan 11's dashboard frontend renders the resulting finding identities,
  evidence, severity, coverage, and legal remediation actions without
  recalculating health.

Canonical findings distinguish unsupported, absent, stale executable,
executable/protocol drift, invalid or forbidden fallback, sandbox/environment/
capability mismatch, provider unavailable/degraded, restart/reconnect/resume
failure, and stuck or unknown lease/attempt. Diagnosis never mutates settings,
repairs a host, or reclaims runtime state. An explicit remediation action
delegates to the owning Plan 20 configuration operation, Plan 27 host operation,
or Plan 32 runtime control with authorization, confirmation where applicable,
CAS/idempotency, and a typed receipt.

Table-driven fixtures compose every source above, including disagreement:
configured-valid but executable absent; discovered-new version outside range;
capability probe supported but sandbox policy denied; configured fallback
missing or forbidden; host repaired while attempt remains stuck; runtime
healthy while provider telemetry is partial; reconnect advertised but resume
receipt stale. Every case yields one stable Doctor finding family with explicit
coverage and owner-specific legal actions, never a Plan27/provider/dashboard-
local diagnosis.

## Pinned Hermes PR17 evidence fixtures

The following checked-in tests from
`NousResearch/hermes-agent@c48d53413aa2c09f6d5703082361c2754f1d5350`
are prior-art evidence for direct TraceDecay fixtures, not APIs or schemas to
copy:

- `tests/hermes_cli/test_kanban_dispatch_lock.py` → concurrent authority epochs
  prove only the fenced Plan 32 owner can admit, reclaim, spawn, or write.
- `tests/hermes_cli/test_kanban_per_profile_cap.py` → running attempts count
  against stable provider/backend/capability capacity, capped work reports a
  deferred reason and becomes eligible later, and invalid limits fail
  validation instead of silently disabling the cap.
- `tests/hermes_cli/test_kanban_reclaim_claim_lock_guard.py` → a stale
  reclaim/cancel/terminal transition cannot clobber a newly leased attempt;
  exact lease ID plus authority epoch, not PID, is the guard.
- `tests/hermes_cli/test_kanban_block_kinds.py` → dependency, needs-input, and
  capability blockers remain typed; recurrence survives unblock, repeated same
  cause reaches human review/triage, and parent completion re-evaluates a
  dependency without erasing blocker history.
- `tests/agent/test_kanban_stop.py` → heartbeat or plain-text exit without a
  terminal receipt is a bounded protocol violation, never completion; guidance
  and persisted retry/violation budgets remain separate.
- `tests/tools/test_async_delegation.py` → finished-undelivered completion is
  durably delivered once with exclusive acknowledgement, abandoned running
  state becomes unknown, capacity races admit at most the bound, and
  cancellation still emits a terminal event.
- `tests/tools/test_kanban_redaction.py` → secret canaries cover comments,
  completion summary/result/metadata, and blocker reasons before persistence,
  extended to every TraceDecay event/receipt/artifact/review/hint/log sink.
- `tests/hermes_cli/test_kanban_swarm.py` → parallel workers remain
  independently visible, verifier readiness waits for every worker, and
  synthesis waits for review; TraceDecay requires reviewed graph proposals and
  stable identities instead of a fixed template or comment blackboard.

These tests must use synthetic or reviewed sanitized fixtures. A platform exclusion is a
typed capability result, not silent coverage. Retrying a flaky test does not close the
failure class.

## Acceptance

- Every PR5–PR20 description and test plan references its row before implementation is
  considered complete.
- Each owned suite exercises failure injection plus retry/restart, not only validation
  errors before work begins.
- Corruption, disk-full, concurrent writer, process death, partial shard, wrong scope,
  stale identity, provider ambiguity, secret canary, and unsupported-platform cases have
  end-to-end coverage in their owning slices.
- LSP suites include stale generations, conflicting dirty overlays, malformed or
  interleaved frames, notification/response confusion, cancellation races,
  analyzer restart exhaustion, competing extension claims, graph-only versus
  analyzer-only coverage, analyzer disagreement, stale versus current
  generation, overlay versus clean-generation semantic evidence, provenance
  dedupe, cross-project merge boundaries, and `prepareRename`/`rename`
  candidates that never self-apply.
- PR14 and LSP/gateway suites include table-driven direct tests for the
  complete canonical semantic-evidence provider state set: unsupported, absent,
  indexing, stale, cancelled, timed-out, failed, and partial. Each state must
  render its typed outcome explicitly; none may collapse to
  `complete_zero_findings`. Only supported plus completed plus complete
  coverage with zero findings may render `complete_zero_findings`.
- PR14 direct tests execute every fixture and assertion in **Flagship dashboard
  regression fixtures and gates**. `npm --prefix dashboard run
  test:acceptance` must pass on the pinned accessibility/performance profile;
  manual NVDA/Firefox and VoiceOver/Safari records have no critical-journey
  failure; and the task-based usability thresholds in Plan 11 pass. Visual
  review, screenshots, or a renderer benchmark alone cannot close a semantic,
  authority, provenance, coverage, redaction, accessibility, or legal-action
  regression.
- PR17 execution-topology acceptance executes every
  `execution-topology-matrix.json`, `execution-topology-events.ndjson`, and
  `integration-operation-matrix.json` record on the visual and accessible
  representations. It covers the complete worktree lifecycle; linear and
  branched stacks; dependency commits and merge order; head/base/merge-base/
  generation drift and retarget; disjoint and overlapping concurrent edits;
  mechanical-only, semantic-only, combined, unknown, false-positive, and
  false-negative conflicts; process/daemon crashes at every operation
  boundary; hook/LSP/five-surface fanout; privacy and authorization loss;
  moved and symlinked repositories; Windows drive/UNC/case/long-path/
  cross-volume behavior; duplicate requests/effects; and branch/ref/worktree/
  receipt retention. Every case preserves lossless TaskId drill-down and
  truthful stale/partial/denied/locked/unsupported/effect-unknown state.
- Scout suites must include a **positive** saved-content/clean-generation
  fixture proving committed semantic evidence remains bound to exact
  saved-content/clean-generation identity through envelope, checkpoint, feedback
  state, telemetry metadata, and every durable spool, cache,
  replica, and export representation; no sink may drop, substitute, or relabel
  that identity.
- Scout suites must include a **negative** unsaved-secret dirty-overlay fixture
  proving no durable envelope, checkpoint, feedback record,
  observation, fact, memory entry, telemetry payload, spool, cache, replica, or
  export contains overlay-derived hover, signature, diagnostic, reference, or
  implementation source/evidence; durable feedback requests for such evidence
  return typed suppressed or unavailable state.
- [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)
  suites must cover the exhaustive one-shot termination taxonomy (clean,
  `duplicate_noop`, blocked, incomplete coverage, stale/replan required, budget exceeded,
  cancellation, user stop, daemon unavailable) with per-trigger terminal
  reason, duplicate-trigger dedupe, suppression, stage and total latency, and
  explicit later-trigger identity — no max-iterations state and no
  loop-iteration count because each trigger is one deliberate evaluation. PR13
  all-four-pillar integration (post-edit diagnostics+impact, CI localization,
  GitHub review-comment/thread ingestion and surfacing, tiered proximity)
  must be complete before PR14 dashboard/Doctor consumption. Table-driven
  fixtures cover three separate exhaustive state sets: GitHub item/thread
  lifecycle (`current`, `outdated`, `resolved`, `edited`, `deleted`); GitHub
  ingress provider outcome (`complete`, `partial`, `unavailable`, `denied`,
  `rate_limited`, `stale`, `failed`), where ingress `denied` means read
  authorization denial only; and Plan 35 semantic-evidence provider states
  (unsupported, absent, indexing, stale, cancelled, timed-out, failed, partial
  versus supported plus completed plus complete-coverage zero-findings).
  No `posted`, `updated`, `dismissed`, or `replied` GitHub state exists;
  `resolved` is required only as an observed read-only lifecycle value. GitHub fixtures cover
  thread/reply lifecycle with bot versus maintainer authorship,
  edited/deleted/resolved/outdated states, exact versus symbol-remapped stale
  binding, rate-limit/auth/ETag/restart recovery, and no-write attempts that
  produce separate `policy=denied` and `effect=suppressed` outcomes before any
  GitHub call, never ingress state. CI
  localization fixtures cover typed provenance, stale/partial/unavailable log
  states without log content, and never claim CI authority. Proximity fixtures
  cover exact-match and risk-threshold above/below tiers using Plan 20's pinned
  effective `feedback.proximity.risk_threshold` value and revision/digest,
  advisory-only semantics, freshness/expiry, and privacy scoping without
  creating a lock or schedule. LSP projection fixtures prove `Diagnostic.range`, `source`,
  `codeDescription.href`, bounded `data`, `relatedInformation` pointers,
  conservative severity, and idempotent version-monotone clear/republish per
  Plan 37/35; duplicate delivery may occur after reconnect but stale
  publication cannot win.
  Surfaces include LSP, agent hooks, MCP, CLI, and dashboard when their owning
  PR ships. Dirty overlay and privacy canary fixtures prove unsaved/private
  source never reaches durable sinks or GitHub. Lossless truncation/expansion
  handle/anchor fixtures cover auth/expiry/corrupt/missing states without
  persisting payloads. PR15 multi-root/cross-project fixtures prove per-root
  branch/worktree/head/generation binding, ambiguity/no-fallback, and
  cross-root proximity/privacy isolation. PR16 remote fixtures prove
  node-local overlay/proximity, fenced durable feedback/GitHub/CI evidence,
  restart/failover, retention/deletion, authorization recheck, remote
  partial/unavailable coverage, and no overlay migration into spools, caches,
  or replicas. No automatic continuation or fix; no state collapses to a clean
  empty result.
- Aggregate verification reports failures by product test, without parsing this file or
  generating a second inventory.
- Removing V1 code cannot remove the last direct test for one of these classes.

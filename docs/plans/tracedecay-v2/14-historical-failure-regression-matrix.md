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
| PR14 | Dashboard, Doctor, observability, and configuration views use canonical daemon operations, distinguish empty/stale/error/locked/partial, and offer executable recovery; the one unqualified Doctor kernel, UI, and remediation consume typed Scout, host finding, GitHub-ingested review-thread, CI-localization, and proximity state emitted by PR13, then add PR17 auxiliary-provider health from Plan 20 configuration state, Plan 27 discovery/conformance/remediation evidence, Plan 32 lease/attempt/runtime evidence, and Plan 26 observations. Auxiliary health covers unsupported/absent/stale executable, executable/protocol drift, invalid fallback, sandbox/environment/capability mismatch, restart/reconnect/resume failure, stuck lease/attempt, provider availability, and desired-versus-observed configuration drift. Plan 27 supplies probes and confirmed remediation operations; Plan 32 supplies runtime evidence; Plan 11 consumes the canonical findings; none defines another Doctor kernel or health formula. Table-driven direct tests cover the complete canonical semantic-evidence provider state set — unsupported, absent, indexing, stale, cancelled, timed-out, failed, and partial — and none of those states may render as a clean empty result; only supported plus completed plus complete-coverage zero-match may present as clean empty. [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md) dashboard/Doctor read models add the canonical GitHub item/thread lifecycle (`current`, `outdated`, `resolved`, `edited`, `deleted`) and ingress provider outcome (`complete`, `partial`, `unavailable`, `denied`, `rate_limited`, `stale`, `failed`) as separate dimensions; CI localization provenance without log payloads; proximity emitted/suppressed/expired/risk-class state with Plan 20 setting provenance; and table-driven lifecycle/outcome/LSP projection fixtures consistent with Plans 37 and 35. Outbound GitHub write denials/suppressions remain separate policy/effect outcomes and never appear as GitHub ingress state. |
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

## PR17 auxiliary-provider Doctor contract

The PR14 Doctor kernel remains the only product-health authority for PR17
auxiliary providers:

- Plan 20 supplies desired/effective configuration revision, validation,
  provenance, and desired-versus-observed activation state.
- Plan 27 supplies observed executable/provider availability, version/protocol/
  capability/sandbox evidence and references to confirmed install/update/
  repair/rollback operations.
- Plan 32 supplies lease/attempt authority, progress/heartbeat, cancellation/
  kill, reconnect/resume, terminal receipt, and stuck/unknown runtime evidence.
- Plan 26 supplies denominator-safe availability, failure, latency, drop, and
  recovery observations.
- Plan 11 renders the resulting finding identities, evidence, severity,
  coverage, and legal remediation actions without recalculating health.

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
  render its typed outcome explicitly; none may collapse to a clean empty
  result. Only supported plus completed plus complete-coverage zero-match may
  present as clean empty.
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

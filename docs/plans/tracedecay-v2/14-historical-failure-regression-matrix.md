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
| PR11 | Policy, application, settings, catalog, analyzer execution, and analyzer configuration remain authorized, deterministic, idempotent, privacy-safe, and free of alias-local business logic. |
| PR12 | CLI, MCP, HTTP, output, and the [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) LSP gateway agree on lifecycle, framing, capabilities, protocol/catalog versions, cancellation, schemas, defaults, errors, pagination, formats, and nonzero failure status; notifications cannot satisfy pending responses; a method outside the supported capability set, or one the active analyzer declares unsupported, returns an explicit unavailable outcome rather than a guessed result; `prepareRename`/`rename` candidates never apply through `workspace/applyEdit` or an opaque server command. |
| PR13 | Hooks stay fast and thin; Scout and host bundles preserve address, privacy, lifecycle ownership, and effects without local query/model/storage work; only clean-generation or saved-content semantic evidence may commit to Scout envelopes, checkpoints, delivery/feedback records, observations, facts, memory, telemetry payloads, spools, caches, replicas, or exports — dirty-overlay or unsaved-secret semantic evidence must return typed suppressed or unavailable state and never durably persist hover, signature, diagnostic, or reference content; conflicting extension claims require safe discovery, explicit replacement confirmation, configuration preservation, and rollback; Claude Code, Cursor desktop, Cursor cloud, and Codex each receive their capability-specific LSP/native-diagnostics/hook delivery path without being forced to a lowest-common-denominator behavior; Hermes and Kiro report hook/MCP/CLI or unavailable paths explicitly and are not assumed to receive full LSP. |
| PR14 | Dashboard, Doctor, observability, and configuration views use canonical daemon operations, distinguish empty/stale/error/locked/partial, and offer executable recovery; the unqualified Doctor kernel, UI, and remediation consume typed Scout and host finding state emitted by PR13; table-driven direct tests cover the complete canonical semantic-evidence provider state set — unsupported, absent, indexing, stale, cancelled, timed-out, failed, and partial — and none of those states may render as a clean empty result; only supported plus completed plus complete-coverage zero-match may present as clean empty. |
| PR15 | Explicit repository/worktree/ref and LSP workspace-folder targets never fall back to CWD, first workspace, or active checkout; cross-project results exact-load globally; dirty/stale graph and multi-root diagnostic coverage is explicit. |
| PR16 | Remote authority, offline replay, cache verification, backup, restore, and failover never admit two writers or hide incomplete coverage; unsaved LSP content, overlays, and analyzer state remain node-local and never enter spools or replicas. |
| PR17 | Workflow scheduling, history, leases, effects, artifacts, retries, and cancellation share daemon authority and never duplicate observable effects. |
| PR18 | Rust, TypeScript, and Python SDKs preserve the public contract, cancellation, retries, privacy, and transport-neutral errors. |
| PR19 | Migration and cutover leave one writer and one canonical route, preserve rollback evidence, reject stale clients, and remove every superseded path. |
| PR20 | Performance optimization never weakens semantics, authority, privacy, ordering, coverage, durability, or crash/restart correctness and cannot hide tail/resource regressions behind averages. |

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
  saved-content/clean-generation identity through envelope, checkpoint, delivery
  receipt, feedback state, telemetry metadata, and every durable spool, cache,
  replica, and export representation; no sink may drop, substitute, or relabel
  that identity.
- Scout suites must include a **negative** unsaved-secret dirty-overlay fixture
  proving no durable envelope, checkpoint, receipt, feedback record,
  observation, fact, memory entry, telemetry payload, spool, cache, replica, or
  export contains overlay-derived hover, signature, diagnostic, reference, or
  implementation source/evidence; durable delivery requests for such evidence
  return typed suppressed or unavailable state.
- Aggregate verification reports failures by product test, without parsing this file or
  generating a second inventory.
- Removing V1 code cannot remove the last direct test for one of these classes.

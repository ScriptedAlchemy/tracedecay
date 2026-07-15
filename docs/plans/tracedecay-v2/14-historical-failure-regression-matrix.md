# TraceDecay V2 Cross-Cutting Regression Contract

## Status / Role

Status: active cross-cutting test contract.

Role: preserve observable failures learned from V1 and dogfooding while PR5 through
PR19 replace implementation. This is a compact ownership map, not a numbered failure
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
| PR6 | Projection failure or replay never corrupts source observations, duplicate entities, hide partial coverage, or produce order-dependent results. |
| PR7 | Stable anchors never cross providers/owners, copied prompts never become authorship, and redaction/deletion never leaves an unsafe resolution path. |
| PR8 | Query reads never repair or mutate storage; caps, pagination, stale data, unavailable shards, and no-result states remain truthful. |
| PR9 | Exact identifiers and phrases are not displaced by echoes, copies, stale summaries, wrong projects, or uncalibrated shard scores. |
| PR10 | Semantic search never silently substitutes models, crosses privacy domains, recomputes unchanged documents, or shortens results after model failure. |
| PR11 | Policy and hints remain bounded, deduplicated, attributable, quiet when irrelevant, and consistent across supported hosts. |
| PR12 | Hooks stay fast, sanitize before durable writes, respect lifecycle ownership, and preserve provider-specific event semantics without duplicated effects. |
| PR13 | CLI, MCP, HTTP, and generated clients agree on schemas, defaults, errors, pagination, formats, capabilities, and nonzero failure status. |
| PR14 | Application and UI paths use daemon authority, distinguish empty from stale/error/locked/partial, and expose recovery actions that can actually succeed. |
| PR15 | Explicit repository/worktree/ref targets never fall back to CWD; cross-project results exact-load globally; dirty/stale graph coverage is explicit. |
| PR16 | Secrets never reach observations, projections, indexes, analytics, logs, handles, exports, backups, or UI; remediation cannot resurrect removed bytes. |
| PR17 | Doctor, repair, update, backup, consolidation, and migration never compete with a live writer, guess identity, discard recovery evidence, or claim an incomplete checkpoint. |
| PR18 | Metrics conserve attempts and outcomes, state denominators/caps/horizons, and never turn missing or sampled evidence into zero or success. |
| PR19 | Cutover leaves one writer authority and one canonical route, preserves rollback evidence, rejects stale clients explicitly, and survives crash/restart at every publication boundary. |

These tests must use synthetic or reviewed sanitized fixtures. A platform exclusion is a
typed capability result, not silent coverage. Retrying a flaky test does not close the
failure class.

## Acceptance

- Every PR5–PR19 description and test plan references its row before implementation is
  considered complete.
- Each owned suite exercises failure injection plus retry/restart, not only validation
  errors before work begins.
- Corruption, disk-full, concurrent writer, process death, partial shard, wrong scope,
  stale identity, provider ambiguity, secret canary, and unsupported-platform cases have
  end-to-end coverage in their owning slices.
- Aggregate verification reports failures by product test, without parsing this file or
  generating a second inventory.
- Removing V1 code cannot remove the last direct test for one of these classes.

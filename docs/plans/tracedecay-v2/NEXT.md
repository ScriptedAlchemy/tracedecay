# PR8: Session/LCM temporal retrieval

**Status:** active execution slice.

This file points contributors at the current implementation slice. It is
documentation only: TraceDecay never parses, imports, schedules, or executes
`NEXT.md`, and product task/work graphs never infer state from it.

PR6 is complete with clean benchmark acceptance at `05da230e`. PR7 is complete
with memory, facts, provenance, migration, hardening, and dogfood evidence.
PR8 now implements the temporally correct Session/LCM retrieval path defined by
[Plan 23](23-session-lcm-temporal-retrieval-and-evaluation.md) under the shared
execution boundary in [Plan 05](05-query-crate.md).

Names retained from completed foundation work are historical evidence, not
instructions to restore an old type, file layout, fixture, milestone, or gate.
Audits map retained behavior, migration, and recovery requirements to current
canonical owners and direct regressions before declaring a gap; renamed or
deleted machinery alone is not missing product behavior.

## Product slice

Replace fragmented message search and LCM lookup with one bounded temporal
retrieval kernel for messages, Turns, sessions, threads, agents, occurrences,
logical copies, and summary-DAG nodes. Preserve exact retained evidence,
history, provenance, privacy, stable anchors, and truthful coverage while
returning the smallest useful context.

PR8 operates on explicitly resolved current-project/single-root scope. It must
not guess another root or fall back to CWD. All retained later capabilities are
mapped in [the authoritative roadmap](00-plan-set-index.md).

The active path is:

```text
sanitized PR5/PR6 observations plus PR7 anchors
  -> occurrence/copy/Turn/session/thread/agent and summary-lineage projections
  -> one typed temporal request and read-only store/projector ports
  -> current | as_of | evolution | forensic evaluation
  -> deterministic candidate merge, hydration, coverage, and pagination
  -> compact anchored context or a typed partial/unavailable result
```

## Production boundary

Plan 23 owns temporal truth, occurrence/copy semantics, summary lineage,
context assembly, freshness, and PR8 acceptance using Plan 05's typed scope,
budget, cancellation, cursor, watermark, deterministic-merge, coverage, and
explanation primitives. Plan 09 owns authorized application orchestration.

The temporal kernel remains in root-package modules while it has one consumer;
physical extraction requires measured reuse and compilation evidence. CLI/MCP
compatibility bindings translate and delegate without private search,
freshness, hydration, pagination, or repair behavior. The daemon/store path is
the sole mutable authority, reads are side-effect free, and explicit refresh is
a separate durable operation.

## Required behavior

- Model every retained provider message as an immutable occurrence with source
  identity, order, ingest time, valid time when known, scope, and sanitization
  receipt.
- Represent logical copies through evidence-backed relations. Hashes,
  timestamps, titles, or embeddings alone never collapse messages.
- Make Turns and threads first-class retrieval grains; preserve provider-native
  Session and agent identity without inferring missing IDs.
- Support explicit `current`, `as_of`, `evolution`, and `forensic` modes.
  Corrections and supersession append assertions and never rewrite history.
- Publish summary-DAG nodes with exact source anchors, source horizon,
  model/config route, watermark, sanitizer receipt, and successor/stale
  lineage. A summary never replaces or hides exact evidence.
- Use one temporal kernel behind `message_search`, `lcm_grep`, load, describe,
  expand, and expand-query compatibility bindings.
- Pin request scope, temporal mode, store/projection/configuration watermarks,
  ordering, cursor identity, privacy decision, and coverage.
- Preserve exact identifiers, quoted phrases, errors, paths, symbols, and
  commands before approximate or configured semantic candidates.
- Hydrate only selected authorized evidence. Empty, stale, partial, wrong
  scope, redacted, retained, locked, and unavailable remain distinct.
- Return bounded context with exact supporting Turns/evidence, summary lineage,
  conflicts, omissions, and continuation anchors rather than transcript dumps.
- Keep LCM payloads and summary nodes authoritative only for session-linked
  narrative/tool-output context. GitHub, CI, diagnostics, Git snapshots, and
  workflow/effect receipts resolve through Plan 13 anchors and their owning
  stores.
- Keep transport `rh_` handles and collection cursors out of durable evidence
  identity.
- Make refresh explicit, daemon-owned, joinable by source frontier/target
  watermark, restart-safe, idempotent, cancellable, and receipt-backed.

## Active implementation order

1. Complete the domain contracts for occurrences, logical copies, temporal
   assertions, Turns/threads/agents, summary nodes, lineage, modes, requests,
   results, coverage, and cursors.
2. Complete store migrations/repositories and rebuildable projections without
   introducing a second writer or repairing during reads.
3. Implement the root-package temporal kernel behind typed read-only ports,
   with deterministic ordering, pagination, hydration, abstention, and
   cancellation.
4. Route existing message/LCM application and compatibility surfaces through
   that kernel and make freshness an explicit operation.
5. Close migration, restart, concurrency, privacy, deletion, performance, and
   compatibility acceptance with direct product tests.

## Direct verification

- copied prompts collapse only with origin evidence; independent repetition
  remains distinct;
- current/as-of/evolution/forensic fixtures preserve corrections, conflicts,
  supersession, and exact historical occurrences;
- summary publication and successor lineage are atomic, restart-stable, and
  drill down to exact retained anchors;
- punctuation, CJK, emoji, provider filters, quoted technical strings, and
  exact identifiers retain deterministic inclusion and ordering;
- reads create no rows, files, cursors, repairs, or writable connections;
- concurrent equivalent refreshes share one operation and terminal receipt;
- pagination rejects changed-watermark or wrong-scope cursors;
- authorization, prompt-injection, secret-canary, redaction, retention,
  deletion, unavailable-source, and partial-coverage cases fail closed;
- single-root scope never switches through CWD, linked worktree, or another
  active project;
- migration/replay is idempotent and projector rebuild preserves anchors,
  history, ordering, and coverage;
- focused PR8 benchmarks record corpus/watermarks, p50/p95, candidate counts,
  allocations, peak RSS, store opens, and exact no-op behavior; and
- stock format, focused tests, all-feature checks/tests, and relevant
  cross-platform gates pass before PR8 completion.

## Later work

Do not pull later product journeys into PR8. Their complete retained features,
semantic ownership, and PR assignments are in
[00-plan-set-index.md](00-plan-set-index.md).

## Prohibited scope

- no parsing or execution of this file or any V2 roadmap document;
- no task/work graph filtering, Kanban behavior, plan execution, or workflow
  runtime;
- no later lexical/semantic code index, policy/transport convergence, or
  multi-root federation;
- no universal query AST, task/board query language, Search Quality Lab, or
  benchmark bureaucracy;
- no writable read path, implicit ingest/repair, fallback store, or direct
  client database access; and
- no GitHub write, autonomous Git mutation, or authority inferred from CWD,
  branch names, response handles, or summary prose.

## Done

PR8 is complete when one root-package temporal kernel serves all message,
Turn, session, thread, agent, and LCM context; raw evidence and summary lineage
remain recoverable and temporally correct; every read is side-effect free;
refresh is explicit and daemon-owned; results are anchored, scoped,
coverage-aware, stable across restart, and compact; compatibility bindings
delegate without private behavior; focused performance evidence is recorded;
and the direct correctness, privacy, concurrency, migration, cross-platform,
and aggregate gates pass.

# V2 query crate

## Status / Role

- Status: pending after the completed PR4 authoritative store boundary.
- PR5 delivers the first production query path.
- PR7 adds facts and provenance, PR8 adds LCM/session retrieval, PR9 adds lexical code search, and PR10 adds semantic search.
- PR11 composes query use cases in application and policy. PR12 exposes them through CLI, MCP, HTTP, and dashboard surfaces.
- tracedecay-query is a transport-neutral execution library. It does not replace domain-specific query contracts with one universal query language.

## Outcome

Every product surface can run the same bounded query use case and receive deterministic rows, pagination, coverage, and explanations. Each domain keeps a typed request suited to its data while reusing common scope and execution primitives.

## Owns

- Shared query primitives: explicit scope, page request, opaque cursor, cost budget, cancellation, frozen watermark, coverage, timing, and safe explanation metadata.
- Planning against application-resolved scope and store-advertised read capabilities.
- Bounded shard selection, execution coordination, deterministic merge, and stable tie-breaking.
- Cursor authentication and validation against scope, access, schema, ranking version, index generation, and captured watermarks.
- Query-side ranking mechanics shared by compatible channels, including finite scores, declared normalization, stable fallback, and component explanations.
- Read-only ports implemented by root store/projector adapters.

## Does not own

- A universal TraceQueryV1 god AST. Activity, facts, LCM, lexical code, semantic code, graph, and export use typed domain requests.
- Scope discovery, authorization, saved-view mutation, annotations, usage accounting, or policy effects.
- SQLite/libSQL connections, SQL, migrations, projector writes, model downloads, HTTP, SSE framing, MCP, CLI, or dashboard code.
- Task plans, work items, boards, leases, attempts, workflow execution, or agent orchestration.
- Source parsing, generated inventories, generated architecture views, or plan-document enforcement.
- Hidden network inference or silent fallback between incompatible indexes or models.

## Required behavior

- **PR5 — minimal path:** implement one typed activity/session listing request end to end through a read port, with explicit scope, bounded page size, stable order, cursor, cancellation, and coverage.
- **PR5 — scope:** accept only application-resolved Profile, Project, Repository, Checkout, Worktree, Ref, and Snapshot roots. Never infer CWD, current project, first project, current branch, or another client’s prior selection.
- **PR5 — snapshots:** capture selected shard watermarks before reads. Frozen pages never observe later rows; unavailable or stale shards remain named in coverage.
- **PR5 — pagination:** bind opaque authenticated cursors to the canonical request digest, access digest, scope generation, schema, ranking profile, index generations, expiry, shard positions, and global cutoff.
- **PR5 — execution:** reject work above declared budgets before expensive I/O, cap concurrency, propagate cancellation, and never emit a cursor for an uncommitted merge state.
- **PR5 — purity:** reads never update retrieval, usage, hint, ranking, or memory counters. Application records adoption later as an explicit event.
- **PR7 — facts/provenance:** add typed fact, assertion, evidence, contradiction, supersession, trust, and as-of requests. Preserve source and privacy-domain identity through merge and hydration.
- **PR8 — LCM/session:** add typed recent-session, message, occurrence, logical-copy, summary-DAG, current, as-of, evolution, and forensic requests. Native rows remain addressable; representative views report hidden and unknown counts.
- **PR9 — lexical code:** add exact identifier, phrase, token, field, bounded fuzzy, relation, path, impact, affected-test, facet, and timeline requests. Exact identifiers precede approximate candidates.
- **PR9 — lexical ranking:** centralize tokenizer/profile versions, lexical normalization, deterministic fusion, diversity, and explanations. Preserve a named V1 compatibility profile only where direct fixtures require it.
- **PR10 — semantic:** add local semantic candidate and bounded rerank channels only with exact model, tokenizer, dimension, metric, normalization, runtime, index-generation, privacy, and watermark compatibility.
- **PR10 — fallback:** when semantic or rerank execution is unavailable, preserve the pre-stage lexical result bytes and order when the selected profile permits fallback; otherwise fail explicitly.
- **PR11 — composition:** expose typed query services to application and pure policy evaluators without importing application or policy into this crate.
- **PR12 — surfaces:** CLI, MCP, HTTP, dashboard, exports, and live views map typed requests and responses without implementing their own scope, ranking, cursor, hydration, or coverage rules.
- **PR12 — export/live:** stream bounded frozen exports with manifests and ordered snapshot/delta/gap contracts. Filesystem publication and SSE framing remain adapter responsibilities.

## Acceptance

- PR5 direct tests cover explicit multi-root scope, no ambient fallback, irrelevant-shard pruning, captured watermarks, deterministic equal-score ordering, cursor tampering/mismatch/expiry, partial coverage, cancellation, and concurrency caps.
- PR7 direct tests cover provenance preservation, contradiction/supersession, as-of knowledge, denied payloads, redacted frontiers, and unknown denominators.
- PR8 direct tests cover native versus representative views, copied prompts, punctuation/CJK/emoji, provider filters, summary freshness, temporal resolution, and restart-stable pagination.
- PR9 direct tests compare lexical inclusion and declared ordering with redacted V1 fixtures and cover exact identifiers, fuzzy bounds, graph limits, impact roles, facets, and deterministic diversity.
- PR10 direct tests cover incompatible representations, privacy isolation, missing artifacts, exact fallback, semantic failure, rerank caps, and byte-stable lexical fallback.
- PR11/PR12 contract tests submit equivalent typed requests through application, CLI JSON, MCP JSON, HTTP JSON, dashboard, export, and live adapters and compare semantic results before rendering.
- Benchmarks record corpus and watermark with p50/p95, candidate counts, allocations, peak RSS, shard opens, and quality deltas. No ranking change ships without direct held-out evidence and worst-stratum checks.
- Architecture tests reject storage, transport, UI, policy, task-executor, and model-runtime dependencies from tracedecay-query.

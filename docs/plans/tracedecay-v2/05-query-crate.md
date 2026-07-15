# V2 query crate

## Status / Role

- Status: modules first after the completed PR4 authoritative store boundary.
- PR5 adds only the observation read/replay needed by its capture vertical in
  existing store and application modules. Extract `tracedecay-query` only when
  PR8+ reuse, dependency isolation, or compile-time savings justify the boundary.
- PR7 adds facts and provenance, PR8 adds LCM/session retrieval, PR9 adds lexical code search, and PR10 adds semantic search.
- PR11 composes query use cases in application and policy. PR12 exposes them through CLI, MCP, HTTP, and dashboard surfaces.
- [PR20](33-end-to-end-performance-optimization.md) owns the versioned retrieval
  workloads, comparable latency, throughput, resource, and no-op baselines, and
  cross-path optimization.
- If extracted, `tracedecay-query` is a transport-neutral execution library. It
  does not replace domain-specific query contracts with one universal language.

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

- **PR8 — one temporal kernel:** message, Turn, session, thread, agent, summary,
  LCM expansion, and compact context requests share one temporal retrieval and
  hydration pipeline. `message_search`, `lcm_grep`, `lcm_load`, `lcm_describe`,
  `lcm_expand`, and `lcm_expand_query` are temporary bindings to it, not query
  implementations.
- **PR8 — compatibility:** compatibility bindings translate inputs and results
  only. They preserve the kernel's scope, temporal mode, watermarks, ordering,
  cursors, coverage, authorization, and cancellation without private fallback.

- **PR5 — observation read:** add one typed observation point-read plus bounded
  sequence replay from the already resolved canonical profile or project store.
  Return sanitized content or payload reference, sanitization receipt, source
  identity/cursor, projection status, and explicit coverage.
- **PR5 — boundary:** use the existing store/application path with bounded
  reads and cancellation. Do not add activity/session search, multi-root query,
  ranking, shard merge, authenticated distributed cursors, or a query framework.
- **PR8+ — shared execution:** introduce frozen multi-root watermarks,
  authenticated cursors, budgets, shard selection/merge, and reusable query
  execution only with the temporal and later retrieval product slices.
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

- PR5 direct tests cover point-read, bounded ordered replay, receipt/source/cursor
  and projection status, partial or unavailable coverage, cancellation, exact
  retry, and canonical profile/project ownership without ambient fallback.
- PR7 direct tests cover provenance preservation, contradiction/supersession, as-of knowledge, denied payloads, redacted frontiers, and unknown denominators.
- PR8 direct tests cover native versus representative views, copied prompts, punctuation/CJK/emoji, provider filters, summary freshness, temporal resolution, and restart-stable pagination.
- PR9 direct tests compare lexical inclusion and declared ordering with redacted V1 fixtures and cover exact identifiers, fuzzy bounds, graph limits, impact roles, facets, and deterministic diversity.
- PR10 direct tests cover incompatible representations, privacy isolation, missing artifacts, exact fallback, semantic failure, rerank caps, and byte-stable lexical fallback.
- PR11/PR12 contract tests submit equivalent typed requests through application, CLI JSON, MCP JSON, HTTP JSON, dashboard, export, and live adapters and compare semantic results before rendering.
- Benchmarks record corpus and watermark with p50/p95, candidate counts, allocations, peak RSS, shard opens, and quality deltas. No ranking change ships without direct held-out evidence and worst-stratum checks.
- Architecture tests reject storage, transport, UI, policy, task-executor, and model-runtime dependencies from tracedecay-query.

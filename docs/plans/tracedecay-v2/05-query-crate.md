# V2 query crate

## Status / Role

- Status: shared query-contract authority. The temporal-kernel delivery is
  complete. Lexical/code and semantic implementations are callable;
  their locked quality/resource acceptance remains active under Plans 25, 31,
  and 15.
- The capture delivery added only the observation read/replay needed by its capture vertical in
  existing store and application modules. Extract `tracedecay-query` only when
  later reuse, dependency isolation, or compile-time savings justify the
  boundary.
- The temporal-kernel delivery established the temporal kernel's behavior and ownership contract:
  typed domain requests, read-only storage/projector boundaries, and no
  SQL/transport/policy authority in the kernel. Its proposed module layout,
  type spellings, suite spine, fixture names, and benchmark paths describe how
  that delivery may be proven; acceptance audits the callable behavior and
  direct regressions rather than requiring those artifacts by name.
- The extraction decision is re-evaluated at the lexical-code delivery, when lexical code retrieval
  becomes the first second consumer of the shared execution primitives. It
  proceeds only with the Plan 19 evidence: named reuse across slices and a
  same-host measurement showing a smaller frequently touched compile graph.
- The memory delivery adds facts and provenance, the temporal-kernel delivery
  adds LCM/session retrieval, the code-intelligence delivery adds
  lexical code search, code navigation, and current diagnostics, and the
  semantic delivery adds semantic search.
- The application delivery composes query use cases in application and policy.
  The surface delivery exposes them
  through CLI, MCP, HTTP, LSP, export, and live non-dashboard adapters.
  The dashboard delivery first ships dashboard binding and dashboard parity.
- Plan 05 owns the shared execution semantics. Plan 23 remains the current
  owner of temporal retrieval semantics; Plans 25 and 31 own active
  lexical/code and semantic acceptance. Plans 09/10/11/12/14 and Plan 24
  consume the accepted temporal-kernel behavior, not a superseded implementation shape.
- The task/work delivery journey reuses these shared scope, budget,
  cancellation, cursor, watermark, merge, and coverage primitives to execute
  Plan 24 requests. Plan 24 owns those typed requests, graph
  traversal/projection semantics, lenses, readiness meaning, and legal pivots.
- Production retrieval slices emit representative latency, throughput,
  resource, and no-op measurements directly to end-to-end performance work.
- If extracted, `tracedecay-query` is a transport-neutral execution library. It
  does not replace domain-specific query contracts with one universal language.
- Staged execution: the temporal kernel operates on current-project/single-root scope with
  available watermark/cursor semantics; frozen multi-root watermarks, shard
  selection/merge, canonical multi-root composition, and federation activate
  with multi-root scope ([Plan 16](16-cross-project-repository-worktree-scope.md)).

## Outcome

Every product surface can run the same bounded query use case and receive deterministic rows, pagination, coverage, and explanations. Each domain keeps a typed request suited to its data while reusing common scope and execution primitives.

## Owns

- Shared query primitives: explicit scope, page request, opaque cursor, cost budget, cancellation, frozen watermark, coverage, timing, and safe explanation metadata.
- Shared renderer- and transport-neutral `ObservabilityEnvelopeV1` value
  primitives: descriptor and revision, entity occurrence, raw value and unit,
  numerator and denominator, eligible/covered/unknown/excluded counts,
  coverage state, uncertainty kind, cohort identity, optional normalization,
  temporal baseline and delta, provenance anchors, explanation components, and
  availability state. Plan 26 owns descriptor, cohort, calibration, and label
  semantics; these shared values define neither a universal health score nor a
  universal query AST.
- Planning against application-resolved scope and store-advertised read capabilities.
- Execution coordination, deterministic merge, and stable tie-breaking; bounded
  shard selection activates with multi-root execution (Plan 16).
- Cursor authentication and validation against scope, access, schema, ranking version, index generation, and captured watermarks.
- Query-side ranking mechanics shared by compatible channels, including finite scores, declared normalization, stable fallback, and component explanations.
- Immutable query-evidence input types for current diagnostics, code
  navigation, and impact/affected-test hybrid reads bound to exact generation,
  file, symbol, span, producer, and freshness evidence.
  [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)
  composes these typed inputs into its branch-aware feedback-cycle result and
  concurrent-agent proximity warnings without this crate importing feedback-
  cycle, GitHub, or proximity types.
- Typed Git requests for working-tree, staged, and revision-range diffs plus
  status, history, blame, and hunk lookup. `HunkRef` binds repository identity,
  immutable side anchors or an explicitly captured mutable-state watermark,
  path identity, native Git diff options, and hunk coordinates.
- Read-only ports implemented by root store/projector adapters.

## Does not own

- A universal TraceQueryV1 god AST. Activity, facts, LCM, lexical code, semantic code, graph, and export use typed domain requests.
- Scope discovery, authorization, saved-view mutation, annotations, usage accounting, or policy effects.
- SQLite/rusqlite connections, SQL, migrations, projector writes, model
  downloads, HTTP, SSE framing, MCP, CLI, or dashboard code. The retired
  libSQL runtime is not a future query-layer dependency.
- Task/work domain identity, request schemas, graph traversal/projection
  semantics, saved lenses, Kanban/board meaning, readiness, leases, attempts,
  workflow execution, or agent orchestration. Plan 05 may execute a Plan
  24-owned typed request through shared primitives; it never defines a task
  query language or universal query AST.
- Source parsing, generated inventories, generated architecture views, or plan-document enforcement.
- A Git object database, revision walker, blame implementation, patch parser,
  or independent diff engine. Native Git supplies repository facts and diff
  semantics; the query layer types, bounds, joins, and explains them.
- An LSP-specific graph or query engine, ranking, hydration, scope, or fallback
  path; LSP remains a transport adapter under
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- Live analyzer sessions, provider lifecycle, provider traits, or implicit
  fetches from admitted analyzers. Plan 09 invokes providers and translates
  provider results into Plan-05-owned explicit evidence inputs; this crate
  never imports Plan 09 or provider traits and never opens analyzer sessions.
- Hidden network inference or silent fallback between incompatible indexes or models.

## Required behavior

- **One temporal kernel:** message, Turn, session, thread, agent, summary,
  LCM expansion, and compact context requests share one temporal retrieval and
  hydration pipeline. `message_search`, `lcm_grep`, `lcm_load`, `lcm_describe`,
  `lcm_expand`, and `lcm_expand_query` are temporary bindings to it, not query
  implementations.
- **Temporal-kernel compatibility:** compatibility bindings translate inputs and results
  only. They preserve the kernel's scope, temporal mode, watermarks, ordering,
  cursors, coverage, authorization, and cancellation without private fallback.
  Audit this through each currently callable compatibility operation and direct
  regressions against the shared kernel. Historical handler paths, type names,
  fixture filenames, benchmark scripts, and test-module registration are not
  independent rebuild requirements.

- **Observation read:** add one typed observation point-read plus bounded
  sequence replay from the already resolved canonical profile or project store.
  Return sanitized content or payload reference, sanitization receipt, source
  identity/cursor, projection status, and explicit coverage.
- **Observation-read boundary:** use the existing store/application path with bounded
  reads and cancellation. Do not add activity/session search, multi-root query,
  ranking, shard merge, authenticated distributed cursors, or a query framework.
- **Shared execution (single-root):** introduce current-project scope,
  single-root frozen watermarks and authenticated cursors, cost budgets, and
  reusable query execution for the temporal kernel and its compatibility
  bindings.
- **Multi-root execution (Plan 16):** consume Plan 16's
  `ResolvedScopeSet`, then execute source selection, bounded per-shard
  retrieval, duplicate collapse, fusion, hydration, and coverage as separate
  phases. Capture a repeatable per-shard snapshot/watermark vector.
  Incomparable shard scores use a versioned deterministic rank-based fallback;
  raw scores are compared only under an evaluated compatible fusion profile.
  Distributed cursors bind the query digest, authorized scope-set digest,
  sorted shard identities, per-shard snapshot/index generation and
  continuation/exhaustion state, fusion/ranking/dedup revisions, last
  total-order key, authorization epoch, schema/catalog revision, expiry, and a
  policy-safe coverage summary.
- **Facts/provenance:** add typed fact, assertion, evidence, contradiction, supersession, trust, and as-of requests. Preserve source and privacy-domain identity through merge and hydration.
- **Immutable Git evidence:** anchors may identify retained commit, tree,
  blob, index, and captured-worktree evidence. Mutable ref names and ambient
  checkout state are routing inputs only; resolution records the exact object
  identity or captured state watermark that was observed.
- **LCM/session:** add typed recent-session, message, occurrence,
  logical-copy, summary-DAG, current, as-of, evolution, and forensic requests
  over current-project/single-root scope. Native rows remain addressable;
  representative views report hidden and unknown counts.
- **Extraction gate:** before adding the lexical surface, decide the
  `tracedecay-query` extraction with the Plan 19 evidence (reuse across the
  temporal kernel and this slice; same-host compile-graph measurement).
  Either outcome, lexical-code work lands against the same typed-request/port contract
  the temporal kernel modules already enforce — location changes, contracts do not.
- **Lexical code:** add exact identifier, phrase, token, field, bounded
  fuzzy, relation, path, impact, affected-test, facet, and timeline requests.
  Preserve a non-demotable exact tier for identifiers, paths, quoted phrases,
  error text, tool names, and configuration keys. Index whole identifiers and
  language-profiled subtokens separately. Return each channel's raw score,
  rank, normalized feature, fusion contribution, pre/post-rerank rank, and
  calibration identity where applicable; none is a probability unless a
  valid, cohort-bound calibrator says so. Impact and affected-test requests may
  merge only explicit typed reference/dispatch evidence inputs alongside
  graph, Git, and test inputs; graph paths preserve edge authority and their
  weakest coverage state. Affected tests are typed as
  `conservative_dependency_candidates`, `observed_coverage_candidates`,
  `predictive_ranked_candidates`, or `unknown_unsupported`; no mode proves a
  test executed, a change was delivered, or universal safety.
- **Git queries:** add typed working-tree, staged, and arbitrary revision-
  range diff requests plus status, history, blame, and `HunkRef` resolution.
  Native Git remains authoritative for objects, revision traversal, status,
  blame, rename detection, and patch/hunk production. Results can join hunks to
  generation-matched symbols, callers, change hazards, and affected-test
  candidates through the code graph; Tree-sitter maps source spans to canonical
  structure and `ast-grep-core` evaluates explicit structural patterns only.
  No layer reimplements Git history, blame, or diff semantics.
- **Dual provenance:** Git evidence provenance and repository-state
  watermarks remain distinct from code-index generation, graph, diagnostic,
  and test-attribution provenance. Joins report both watermarks, mismatch and
  partial coverage explicitly; a matching path or line never implies matching
  content or generation.
- **Diagnostics/navigation:** add typed current-diagnostic and
  code-navigation reads over exact clean-generation and graph-backed evidence,
  covering declaration, definition, type-definition, implementation,
  references, symbols, and call hierarchy. Current results require matching
  scope, generation, content, producer provenance, freshness, and
  clearing/supersession state. This query work does not import Plan 09, depend
  on live providers, or open analyzer sessions. The graph remains authoritative
  for stable identity, generations, bounded traversal, history, cross-project
  evidence, and test attribution.
- **Analyzer-derived navigation:** activate hover, signature help, type
  hierarchy, and rename-candidate merging in the same slice as Plan 35
  analyzer producers. Plan 09 translates provider results into Plan-05-owned
  explicit evidence inputs; the query crate validates and merges only those
  typed inputs and never opens analyzer sessions or depends on live provider
  availability. Rename candidates are not durable clean-generation evidence.
- **Lexical ranking:** centralize tokenizer/profile versions, lexical
  normalization, deterministic fusion, diversity, and explanations. `V1` may
  name the initial final profile. V2 profile bindings, indexes, and all related
  persisted state accept only their exact final shape; any other shape returns
  typed `ResetRequired` and requires explicit reset or recreation. There is no
  storage compatibility/rebuild reader, migration, backfill, dual write, or
  census path. Independently released public query protocols are separate from
  persisted-state admission; fixtures alone do not establish publication.
- **Semantic:** add local semantic candidate and bounded rerank channels only with exact model, tokenizer, dimension, metric, normalization, runtime, index-generation, privacy, and watermark compatibility.
- **Semantic fallback:** when semantic or rerank execution is unavailable, preserve the pre-stage lexical result bytes and order when the selected profile permits fallback; otherwise fail explicitly.
- **Application composition:** expose typed query services to application and pure policy evaluators without importing application or policy into this crate.
- **Surfaces:** CLI, MCP, HTTP, LSP, exports, and live views map typed
  requests and responses without implementing their own scope, ranking, cursor,
  hydration, coverage, or fallback rules. LSP uses the same query kernels as
  every other adapter. Dashboard binding and dashboard parity remain owned by
  the dashboard delivery; the surface delivery does not ship dashboard adapters.
- **Export/live:** stream bounded frozen exports with manifests and ordered snapshot/delta/gap contracts. Filesystem publication and SSE framing remain adapter responsibilities.

  **Owner decision (2026-08-08, Zack): NOT NEEDED — descoped without
  replacement.** The kernel-level frozen-export mechanism has no remaining
  consumer on either half. The live-view half is satisfied by deviation:
  the dashboard — the only live consumer that ever existed — gets its
  streaming consistency from the Plan 11 revision-monotone HTTP/SSE state
  path (envelope watermarks + the events stream) at the adapter layer,
  not from kernel export contracts. The export half's only intended
  reader, the ResearchBundle family, was deleted by an earlier recorded
  owner decision. CLI, MCP, and hook surfaces are request/response
  consumers whose consistency is already guaranteed by the landed bounded
  snapshot + watermark envelopes and restart-stable cursor pagination
  (cursors pin generation/watermark across pages); none of them holds a
  long-lived subscription to changing results. Revisit clause: if
  subscription-style consumers materialize post-V2 (for example MCP
  resource subscriptions letting agents watch a query), the
  snapshot/delta/gap contract described above is the design to implement
  then — nothing in this decision forecloses it.
- **Task/work reuse:** accept Plan 24-owned typed request and projection
  descriptors through narrow consumer-owned ports, then provide only shared
  scope resolution inputs, budgets, cancellation, pagination, watermarks,
  deterministic merge, coverage, and explanations. Execute Plan 24-owned task
  lookup, context, history, thread, impact, attempt, and evidence requests
  through those primitives. `TaskId` is a stable authorized retrieval root;
  Plan 05 neither defines task identity nor copies task evidence into a
  query-owned store. Do not add `TaskQuery`, board filter DSL, task entity
  semantics, or a universal cross-domain AST.

## Acceptance

- Observation-read direct tests cover point-read, bounded ordered replay, receipt/source/cursor
  and projection status, partial or unavailable coverage, cancellation, exact
  retry, and canonical profile/project ownership without ambient fallback.
- Facts/provenance direct tests cover provenance preservation, contradiction/supersession, as-of knowledge, denied payloads, redacted frontiers, and unknown denominators.
- Facts/provenance tests also prove Git anchors resolve immutable objects or retained
  captured state after ref movement, checkout removal, and index rebuild.
- Temporal-kernel direct tests cover native versus representative views, copied prompts, punctuation/CJK/emoji, provider filters, summary freshness, temporal resolution, and restart-stable pagination.
- Lexical-code direct tests compare lexical inclusion and declared ordering with redacted V1 fixtures and cover exact identifiers, fuzzy bounds, graph limits, impact roles, facets, deterministic diversity, and generation-exact clean-generation/graph-backed diagnostic and navigation reads.
- Git-query tests cover working, staged, committed-range, rename, deletion,
  binary, merge-history, blame, and hunk queries; stable `HunkRef` replay;
  symbol/caller/hazard/affected-test joins; bounded partial coverage; and
  rejection or explicit degradation when Git and code-generation watermarks do
  not match native content.
- Surface direct tests cover analyzer-derived hover, signature help, type hierarchy, and rename-candidate merging from explicit typed evidence inputs only.
- Semantic direct tests cover incompatible representations, privacy isolation, missing artifacts, exact fallback, semantic failure, rerank caps, and byte-stable lexical fallback.
- Application/surface contract tests submit equivalent typed requests through application,
  CLI JSON, MCP JSON, HTTP JSON, LSP, export, and live adapters and compare
  semantic results before rendering. Dashboard contract tests add dashboard binding
  and dashboard parity on the same typed requests.
- Task/work composition tests prove Plan 24 requests retain identical selected entity
  IDs, versions, scope, watermarks, coverage, and ordering when run through
  shared execution primitives, while no task/board/request semantics enter
  Plan 05. Counterexamples prove unknown coverage never becomes zero or
  healthy, exact tiers are never demoted, cursor replay cannot change
  cohort/shard/profile identity, and task summaries cannot replace exact Plan
  13 anchors.
- Benchmarks record corpus and watermark with p50/p95, candidate counts, allocations, peak RSS, shard opens, and quality deltas. No ranking change ships without direct held-out evidence and worst-stratum checks.
- Boundary regressions reject storage, transport, UI, policy, task-executor,
  and model-runtime authority in the query kernel, plus any LSP-private query
  engine or fallback. The gate follows the current owner boundary whether the
  implementation remains a module or is extracted into a crate.

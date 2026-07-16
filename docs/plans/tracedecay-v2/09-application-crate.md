# TraceDecay V2 Application Crate

## Status / Role

Normative product plan. `tracedecay-application` is the transport-neutral use-case layer between product adapters and the domain/query/store ports. It participates in every vertical product PR from PR5 onward; PR11 completes the shared application core needed by the public adapters.

## Outcome

Every user-visible operation has one direct typed application entry point. CLI, MCP, HTTP, hooks, automations, and the dashboard invoke the same behavior without duplicating policy, authorization, consistency, or error handling.

## Owns

- Typed request, response, and error contracts for product use cases.
- `RequestContext`: actor, project/repository/worktree scope, capabilities, request ID, deadline, and cancellation.
- Read orchestration across query and store ports.
- Command orchestration, validation, authorization, idempotency, and transaction boundaries.
- Freshness, coverage, provenance, pagination, and partial-result semantics.
- Stable progress and event contracts consumed by streaming adapters.
- Typed transport-neutral operations and state contracts for LSP session
  admission, current diagnostics, analyzer engine and coverage state, and code
  navigation as required by
  [35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- The one typed, transport-neutral semantic-evidence/provider contract that
  ships in PR11 with the application core. Every analyzer-backed capability
  implements this contract. Plan 35 implements analyzer-backed providers behind
  it; this crate owns the contract's type, evolution, and canonical
  provider-result identity/compatibility semantics—not a copy scoped to LSP.
  Every provider result identity tuple is complete from PR11 onward: PR11 ships
  explicit current-project/single-root scope/project/worktree identity available
  then; PR15 upgrades and composes that scope identity with Plan 16 canonical
  multi-root/cross-project scope identity. Plan 16 is not a PR11 prerequisite.
  The tuple also includes clean-generation or node/client/session overlay
  identity; file/content digest; document version where applicable;
  producer/analyzer identity and revision; requested capability; freshness;
  coverage/completeness; provenance; Plan 25 language-descriptor
  identity/revision; Plan 20 configuration revision/digest; and Plan 06 policy
  decision/revision/digest.
- Translation from provider results into Plan-05-owned explicit query-evidence
  inputs for diagnostics, navigation, impact, and affected-test reads.
- The one advisory, transport-neutral branch-aware feedback-cycle
  request/result, orchestration, and finding lifecycle, shipping in PR11 as
  part of the first PR11–PR13 milestone defined by
  [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md).
  Plan 37 defines the architecture; this crate owns the concrete contract.
  Producers are composed, not reimplemented: post-edit diagnostics plus Plan 05
  impact evidence, CI-failure-localization input, ingested GitHub review
  threads, and concurrent-agent proximity warnings. Each request is one-shot
  only — no automatic follow-up, fix application, or effect execution.
  Results carry branch/worktree/commit/generation/content identity, stable
  finding IDs, [Plan 13](13-research-provenance-and-context-anchors.md)
  `RetrievalAnchorId`s where durable evidence exists, coverage/state,
  safe bounded previews, pagination/continuation metadata, and source
  provenance. Findings translate into Plan 05 explicit query-evidence inputs;
  this crate creates no second diagnostic or finding store.
- Direct product operations for capture, search, context, sessions, memory, code, delivery, automation, Doctor, configuration, and workflows.
- Canonical structural-search, source-outline, and source-rewrite operations
  backed by the PR9 in-process code-intelligence kernel.
- One source-edit `EditTransaction` for preview and apply across exact, symbol,
  insert, move, and structural rewrites.
- One daemon-owned `GitIndexTransaction` for typed `stage_hunks`,
  `unstage_hunks`, and `commit_index` execution against a real locked Git
  index. It owns immutable previews, CAS revalidation, idempotency, receipts,
  and explicit effect classes without exposing arbitrary Git execution.

## Does not own

- HTTP, SSE, MCP, CLI, hook, or frontend transport details.
- LSP JSON-RPC framing, stdio or socket bridging, upstream process
  supervision, or per-connection protocol buffers.
- SQL, libSQL connections, filesystem layout, indexing, or migration mechanics.
- Domain entity definitions or domain invariants.
- A generic command bus, query bus, plugin framework, service locator, or runtime registry.
- A generic LSP or JSON-RPC pass-through operation.
- Analyzer-provider cache storage, admission, reuse, eviction, invalidation
  execution, or lifecycle; those remain owned by
  [35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- GitHub REST/GraphQL identity, comment posting, or adapter packaging;
  [Plan 27](27-cross-host-agent-plugin-bundles.md) owns read-only GitHub
  ingestion mechanics. PR17 workflow composition is optional and does not gate
  the PR11–PR13 advisory cycle.
- A second diagnostic or finding store, transport bindings, LSP field
  projection, or host delivery adapters; those remain owned by Plans
  05/13/21/27/35/37 respectively.
- Developer plan parsing, Markdown execution, task scheduling, agent orchestration, edit bundles, generated inventories, or compatibility ledgers.
- JavaScript workflow execution. PR17 workflows are real typed product operations, not developer-plan machinery.
- Merge, rebase, cherry-pick, branch/tag/ref mutation, history rewriting, or an
  autonomous Git workflow engine.

## Required behavior

- Define one explicit service method or use-case type per product operation; prefer ordinary Rust calls over indirection.
- Depend only on domain types and narrow port traits. No adapter or root-crate imports.
- Validate scope and capability before reads or writes; never infer authority from transport origin.
- Preserve repository, worktree, branch, project, and user scope through every call.
- LSP-facing operations preserve authorized workspace scope, deadline,
  cancellation, document version, source generation, freshness, and coverage
  without accepting transport-native arbitrary payloads.
- Navigation, type-hierarchy, context, impact, affected-test, diagnostics, and
  refactoring-preview operations are enriched internally with the
  semantic-evidence provider's source/producer identity, provenance,
  coverage, freshness, and conflicts rather than exposed through a duplicate
  public `lsp_*` tool family. Active-document type semantics may come from an
  admitted analyzer provider; the code graph remains authoritative for stable
  symbol identity, generations, bounded traversal, history, cross-project
  evidence, and test attribution. Unsupported, absent, indexing, stale,
  cancelled, timed-out, failed, and partial provider states are reported
  explicitly; none may collapse to a clean empty result. Empty output is valid
  only for a supported, successfully completed request with complete coverage
  and zero matches. Impact and affected-test operations may incorporate provider
  reference/dispatch evidence translated into Plan-05-owned explicit typed
  inputs alongside graph, Git, and test evidence; a provider never proves that
  a test executed or that a change was delivered.
- Catalog, dashboard, and observability surfaces consume typed application
  results and state, never the provider port directly.
- Return structured freshness, coverage, provenance, warnings, and continuation data where relevant.
- Make mutation retries safe through operation-specific idempotency keys and daemon-owned transactions.
- Source edits use one journaled all-or-recoverable `EditTransaction`. Preview
  pins the file set and digests; apply revalidates every digest/CAS guard, stages
  sibling files, journals recovery data, and commits in deterministic order.
  A single file publishes by atomic rename; portable multi-file atomicity is not
  claimed. Success is reported only after every file commits. After a crash,
  reconciliation completes or rolls back the journal before new edits or
  reindexing. CLI `--dry-run` and tool `dry_run` mean this same preview.
- PR11 Git index mutations use `GitIndexTransaction`. Preview pins repository,
  worktree, HEAD/index identity, selected hunks, path/content digests, intended
  effect class, and canonical transaction digest. Apply acquires the real Git
  index lock, revalidates every CAS guard, executes only the previewed
  `stage_hunks`, `unstage_hunks`, or `commit_index` steps, and releases the lock
  on every outcome. A reused idempotency key returns the same durable receipt;
  mismatched input fails closed. Concurrent index change, stale HEAD/content,
  lock contention, and patch conflict remain distinct typed states. No partial
  success is reported as committed.
- `str_replace` is a compatibility binding to one-operation
  `multi_str_replace`; `insert_at_symbol` binds typed `insert_at`. Keep
  `replace_symbol`, in-process structural rewrite, and `move_symbol` as typed
  views over the same transaction; do not add split/import mutation tools.
- Check cancellation and deadlines around expensive or multi-stage work.
- Map domain and port failures into a small stable application error taxonomy without erasing actionable detail.
- Keep streaming events bounded, ordered, resumable where the product contract requires it, and independent of SSE framing.
- Expose workflow create, validate, run, inspect, cancel, and history operations in PR17 as typed domain/application contracts only.
- Add each use case in the same product PR as its domain/store/query behavior; do not create speculative APIs ahead of executable behavior.
- PR11 ships the transport-neutral semantic-evidence/provider contract with
  the completed application core and explicit current-project/single-root
  scope/project/worktree identity in every provider result. PR9 query work does
  not import this crate or depend on live providers.
- PR15 upgrades provider-result scope identity by composing PR11's
  single-root identity with Plan 16 canonical multi-root/cross-project scope
  identity. Plan 16 is not a PR11 prerequisite.
- PR11 removes remaining root-level business orchestration by routing adapters
  through the completed application core.
- PR11 feedback-cycle requests bind project/repository/worktree/branch/ref/HEAD
  SHA, clean source-generation identity or an explicitly tagged ephemeral
  overlay, file digest and document version, agent/session/turn identity,
  changed files/ranges/symbols, the exact trigger, policy/config digests, and
  deadline/cancellation/budget inputs. Overlay-triggered requests may return
  immediate session-only findings to the authorized overlay owner; those
  findings are never durable — they cannot enter capsules, envelopes,
  checkpoints, receipts, feedback-history records, observations, facts, memory,
  telemetry payloads, spools, caches, replicas, exports, or ingested GitHub
  evidence. Durable findings require exact saved-content/clean-generation
  identity.
- PR11 feedback-cycle results name exactly one termination reason from Plan
  37's taxonomy, distinguish new versus pre-existing diagnostics, preserve
  complete provider-state sets without collapsing unavailable/partial coverage
  to clean empty results, and expose finding lifecycle state
  (active/superseded/resolved/cleared) keyed by stable finding IDs plus Plan 13
  anchors when present.
- Keep the application's direct dependency graph narrow and feature-minimal.
  Concrete stores, transports, providers, model runtimes, dashboard assets, and
  their build scripts must not enter its normal check or test graph.
- Treat PR11 as a compilation-boundary migration as well as an ownership
  migration: record same-host warm incremental check and representative
  application-test compilation before and after root orchestration moves.
  Regressions require an identified cause and explicit disposition.

## Acceptance

- Every shipped product operation has a typed application contract and focused unit tests.
- CLI, MCP, HTTP, hooks, automations, and dashboard paths share those contracts rather than reimplementing behavior.
- Dependency checks prove the crate is transport- and storage-neutral.
- Authorization, scope, cancellation, idempotency, freshness, coverage, and error semantics have direct tests.
- `GitIndexTransaction` tests use a real repository and index lock and cover
  preview immutability, CAS drift, conflicting hunks, concurrent index change,
  idempotent replay, crash-safe receipts, lock release, and exact effect-class
  enforcement without permitting generic or history-mutating Git commands.
- Unsupported, absent, indexing, stale, cancelled, timed-out, failed, and partial
  provider states have direct tests; none collapse to a clean empty result.
  Empty output is valid only for supported, successfully completed requests with
  complete coverage and zero matches.
- No generic bus/framework, plan parser/executor, generated inventory, or JavaScript workflow runtime exists in this layer.
- PR11 leaves no product orchestration in transport handlers or the legacy root crate.
- A focused application check or test does not compile transport, dashboard,
  provider, or concrete-storage targets, and the legacy root crate's dependency
  fan-in is measurably reduced.
- PR11 feedback-cycle fixtures cover one-shot advisory semantics, every
  producer class (post-edit diagnostics/impact, CI-localization input, ingested
  GitHub review threads, proximity), finding lifecycle transitions, Plan 13
  anchor attachment, Plan 05 evidence translation without a second store,
  pagination/continuation metadata, dirty-overlay non-durability, and exact
  termination reasons for branch/head/content/generation change, duplicate
  triggers, cancellation, and budget exhaustion.

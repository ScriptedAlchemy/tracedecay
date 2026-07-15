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
- Direct product operations for capture, search, context, sessions, memory, code, delivery, automation, Doctor, configuration, and workflows.
- Canonical structural-search, source-outline, and source-rewrite operations
  backed by the PR9 in-process code-intelligence kernel.
- One source-edit `EditTransaction` for preview and apply across exact, symbol,
  insert, move, and structural rewrites.

## Does not own

- HTTP, SSE, MCP, CLI, hook, or frontend transport details.
- LSP JSON-RPC framing, stdio or socket bridging, upstream process
  supervision, or per-connection protocol buffers.
- SQL, libSQL connections, filesystem layout, indexing, or migration mechanics.
- Domain entity definitions or domain invariants.
- A generic command bus, query bus, plugin framework, service locator, or runtime registry.
- A generic LSP or JSON-RPC pass-through operation.
- Developer plan parsing, Markdown execution, task scheduling, agent orchestration, edit bundles, generated inventories, or compatibility ledgers.
- JavaScript workflow execution. PR17 workflows are real typed product operations, not developer-plan machinery.

## Required behavior

- Define one explicit service method or use-case type per product operation; prefer ordinary Rust calls over indirection.
- Depend only on domain types and narrow port traits. No adapter or root-crate imports.
- Validate scope and capability before reads or writes; never infer authority from transport origin.
- Preserve repository, worktree, branch, project, and user scope through every call.
- LSP-facing operations preserve authorized workspace scope, deadline,
  cancellation, document version, source generation, freshness, and coverage
  without accepting transport-native arbitrary payloads.
- Return structured freshness, coverage, provenance, warnings, and continuation data where relevant.
- Make mutation retries safe through operation-specific idempotency keys and daemon-owned transactions.
- Source edits use one journaled all-or-recoverable `EditTransaction`. Preview
  pins the file set and digests; apply revalidates every digest/CAS guard, stages
  sibling files, journals recovery data, and commits in deterministic order.
  A single file publishes by atomic rename; portable multi-file atomicity is not
  claimed. Success is reported only after every file commits. After a crash,
  reconciliation completes or rolls back the journal before new edits or
  reindexing. CLI `--dry-run` and tool `dry_run` mean this same preview.
- `str_replace` is a compatibility binding to one-operation
  `multi_str_replace`; `insert_at_symbol` binds typed `insert_at`. Keep
  `replace_symbol`, in-process structural rewrite, and `move_symbol` as typed
  views over the same transaction; do not add split/import mutation tools.
- Check cancellation and deadlines around expensive or multi-stage work.
- Map domain and port failures into a small stable application error taxonomy without erasing actionable detail.
- Keep streaming events bounded, ordered, resumable where the product contract requires it, and independent of SSE framing.
- Expose workflow create, validate, run, inspect, cancel, and history operations in PR17 as typed domain/application contracts only.
- Add each use case in the same product PR as its domain/store/query behavior; do not create speculative APIs ahead of executable behavior.
- PR11 removes remaining root-level business orchestration by routing adapters through the completed application core.
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
- No generic bus/framework, plan parser/executor, generated inventory, or JavaScript workflow runtime exists in this layer.
- PR11 leaves no product orchestration in transport handlers or the legacy root crate.
- A focused application check or test does not compile transport, dashboard,
  provider, or concrete-storage targets, and the legacy root crate's dependency
  fan-in is measurably reduced.

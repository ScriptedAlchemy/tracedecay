# TraceDecay V2 RC Recovery Design

**Status:** Approved on 2026-08-08

## Purpose

Finish the latest Claude Code root session's interrupted V2 delivery in place,
preserve the useful implementation already present on
`codex/tracedecay-total-redesign-plan`, and produce a release-candidate branch
whose product surfaces are wired through real production journeys.

The recovery is not a new roadmap. The sole roadmap precedence remains
`docs/plans/tracedecay-v2/00-plan-set-index.md`; this design reconciles the
unfinished checkout with that authority.

## Recovered State

The latest Claude root session was
`99dc84b5-f5ec-4ebb-8b96-318e1b20f871`. It referenced 140 ordinary subagents
and one 36-agent code-review workflow. Every referenced agent transcript,
metadata record, and task output was recovered. The root session stopped after
Claude hit its monthly quota and never produced its requested final checkpoint.

The checkout contains two local commits beyond the remote integration floor and
an unstaged implementation spanning Work, workflow fan-out, worktrees,
observability, host integration, privacy, LSP, retained context, SDKs, and the
dashboard. A late queue run established that workspace compilation succeeds,
but the broad root library run still had 98 failures. Several green results in
older Claude logs were invalid because shell pipelines masked Cargo failures;
only direct exit status and non-vacuous test counts are accepted below.

## Product Outcome

The RC branch must provide the promised V2 product through the production
daemon and supported host journeys. Contracts, fakes, generated clients, and
dashboard components do not count as delivered unless a production caller can
exercise them and the relevant failure states remain typed.

RC readiness means:

- canonical Rust authorities own every wire shape;
- Work and workflow operations are admitted, dispatched, persisted, observed,
  and retrievable through their promised surfaces;
- SDK, MCP, HTTP, CLI, host, and dashboard availability claims match mounted
  production behavior;
- host installation and execution preserve operator state and isolate test
  state;
- privacy, identity, staleness, denial, rollback, and replay boundaries fail
  closed with falsifiable tests;
- generated artifacts are regenerated only from canonical authorities;
- focused and aggregate verification reports zero unclassified failures.

## Recovery Strategy

Preserve and finish Claude's dirty tree in place. Each dirty module is handled
in one of three ways:

1. complete its real production journey and retain it;
2. fold it into the canonical authority that already owns the behavior; or
3. delete it when no V2 acceptance requirement or shipped compatibility
   obligation justifies it.

The recovery will not checkpoint the entire dirty tree as a mixed WIP commit,
reset it to the remote branch, or build parallel shadow authorities.

## Delivery Slices

### 1. Correctness and security foundation

Repair the load-bearing defects before mounting additional callers:

- make work-synthesis replay byte-stable by persisting the complete admitted
  result atomically rather than recomputing source and draft state;
- bind a run's deadline and topology to durable run identity rather than
  lexical attempt ordering or caller self-attestation;
- clear the environment of spawned provider processes and restore only the
  admitted snapshot;
- return truthful fresh-store reset, Doctor observation, retained-source,
  graph-generation, and ownership states instead of fabricated defaults;
- preserve typed absent, unsupported, denied, stale, and unavailable outcomes.

### 2. Work and workflow runtime

Complete the canonical Work journey:

- mount all 26 Work operations through definitions, binding, dispatch,
  application ownership, and production handlers;
- resolve and pin topology from registered control-plane authority before a
  provider starts;
- provide real worktree inventory and cleanup adapters with partial, stale,
  foreign, denial, reconcile, and rollback behavior;
- enforce fan-out fences and `max_parallel` through the real run-control path;
- persist checkpoints only when a production consumer retrieves or hands them
  off; otherwise remove the unused contract;
- emit topology/run/attempt/handoff events and project them through a
  generation-bound read model.

### 3. Shared transports and hosts

Expose only mounted capabilities:

- finish Context Scout and the V2 SDK operations that have canonical schemas
  and production executors; operations without a real journey remain typed
  unavailable rather than fabricated;
- wire required remote enrollment, status, replay, backup, restore, and
  failover operations through their promised CLI/MCP/SDK/dashboard surfaces;
- make LSP advisory registrations, snapshots, and denied outcomes derive from
  real daemon authority;
- apply one install/update/uninstall/isolation contract across all supported
  hosts, with a Kiro CLI adapter that clears ambient `KIRO_HOME`, controls its
  working directory, preserves peer MCP entries, and rolls back failures;
- parse and sanitize structured provider metadata before any GitHub, fact, or
  session sink sees it.

### 4. Contracts and dashboard

After Rust request/result shapes settle:

- export dashboard schemas and regenerate TypeScript contracts;
- regenerate Rust and TypeScript SDKs from the same registry;
- update fixtures to embed `WorkTopologyPolicyV1` rather than the obsolete
  `topology_policy_digest`;
- mount the recovered Observatory views and Work topology accounting in real
  navigation and data routes;
- bind joined dashboard data to one generation and label capped denominators as
  partial rather than exact;
- add DOM journey tests for user-visible V2 behavior. Automated functional UI
  coverage remains required; manual screen-reader polish is not an RC blocker.

### 5. Cutover, cleanup, and release evidence

Complete cutovers instead of keeping branch-local compatibility:

- remove the root application compatibility shim after migrating its callers;
- split newly created or materially touched hand-written modules that exceed
  the repository's 1,000-line ceiling;
- remove dead flags, aliases, test-only production ports, stale docs, and
  unmounted claims;
- run formatting, compiler, lint, contract, dashboard, host, integration,
  aggregate Rust, and current CI checks with direct exit-status evidence;
- record release evidence and the exact remaining operator actions.

## Execution Model

Implementation uses test-driven slices. Every behavioral change begins with a
focused failing test, records the expected failure, adds the minimum production
change, and records the passing result. Generated artifacts are the explicit
exception: their generator and drift check are the test.

Each slice has one implementation owner at a time because all agents share this
dirty checkout. Terra workers own cohesive integration slices, Luna workers own
small mechanical tests, fixtures, and generated-artifact work, and Sol workers
own subtle concurrency, replay, identity, and security boundaries. Workers must
re-read files before editing, stage only their declared ownership, preserve peer
changes, and create conventional commits. A separate worker reviews every slice
for spec compliance and code quality before the next dependent slice begins.

## Verification

Focused development checks are followed by:

- `cargo fmt --all -- --check`;
- `cargo check --workspace --all-targets --all-features`;
- repository clippy policy with warnings denied;
- non-vacuous focused Rust tests and then the full workspace suite;
- dashboard contract generation/check, typecheck, tests, and production build;
- SDK generation and conformance checks;
- supported-host bundle, install/update/uninstall, and isolation journeys;
- end-to-end Work, workflow, retained-context, remote, LSP, and dashboard
  journeys;
- current GitHub CI for the final pushed commit.

Failures are fixed at their root. Assertions are not weakened, timeouts are not
raised to hide races, and tests are not ignored or filtered into vacuous greens.

## Human and Operator Gates

The code branch can be RC-ready while clearly recording these external actions:

- npm trusted-publisher/OIDC configuration, which the user will provide;
- designated live semantic evaluation/profile runs;
- machine-specific Doctor/Cursor/Kiro journeys where the real host is required;
- the planned large-store garbage-collection observation.

No RC tag or package publication occurs until those required external gates are
classified and the user authorizes publication.

# PR17: Daemon-owned typed workflows and automations

**Status:** implementation authority for PR17.

## Decision

TraceDecay workflows compose existing typed application operations. The daemon validates
versioned definitions, owns runs, schedules steps, records effects, and exposes controls.

PR17 adds no JavaScript/TypeScript runtime, generated Claude workflow JavaScript,
Markdown parser, progress tracker, rewrite executor, taskgraph compiler, or shell command tape.
Plan files remain prose and are never executable workflow input.

## Definition contract

An immutable workflow definition version contains:

- stable definition/version identity, owner, explicit project/profile scope,
  input/output schema, and retention class;
- typed step IDs referencing cataloged application operation IDs;
- schema-validated literal inputs or typed references to prior step outputs;
- explicit dependency edges, bounded fan-out groups, concurrency and failure
  policy, route/capability requirements, budgets, and acceptance conditions;
- configuration/catalog/policy/privacy snapshots and a definition digest.

Definitions are data, not source code. Unknown operations, cycles, dangling references,
incompatible schemas, unbounded fan-out, privilege escalation, or
unsupported effects reject before activation. Editing creates a new version;
admitted runs stay pinned to their exact version and snapshots.

Lifecycle is `Candidate -> Validated -> Active -> Retired | Rejected`. Names
are scoped aliases only; run admission resolves and records an exact version.
Files may be explicit import/export artifacts, but watchers never auto-import,
activate, or infer authority from CWD or nearest-directory precedence.

## Run and effect authority

Runs reuse the existing daemon scheduler, generic operations/steps, leases,
executor registrations, policy, event/outbox, idempotency, accounting, and
subscription mechanisms where their contracts fit. There is no workflow
database, journal, scheduler, lease family, retry loop, or worker authority.

Canonical run history records admission, step readiness, attempt dispatch,
delivery/effect observation, validated result, retry decision, cancellation,
checkpoint, and terminal receipt. A step becomes ready only from committed
history. Admission plus outbox, result plus transition, and terminal closure are
atomic owner-shard transactions.

Every effect has stable run/step/attempt/idempotency identity. Idempotent effects
may resume after restart; at-least-once and non-repeatable adapters follow their
declared reconciliation rules. Sent-without-receipt becomes `EffectUnknown` and
blocks automatic retry and successful completion. A replacement attempt is
legal only after the daemon proves the previous effect absent or safely
repeatable.

Pause and cancellation fence new admissions, reconcile in-flight effects, and
then publish a stable state. Cancellation never rewrites completed history.
Retries retain prior evidence and remain bounded by attempt, time, token, cost,
output, and concurrency budgets. Restart rebuilds readiness from canonical
history and cannot duplicate a committed observable effect.

## Application and surfaces

Typed application use cases cover definition list/get/create-version/validate/
activate/retire/diff and run list/get/start/pause/resume/cancel/retry/status/
history. Mutations use expected version, authority epoch, actor, reason,
idempotency key, and typed receipts. Protected inputs, outputs, transcripts, and
artifacts resolve through existing authorized payload routes.

HTTP/OpenAPI and generated Rust/TypeScript/Python clients bind those operations.
CLI provides `tracedecay workflow definition ...` and
`tracedecay workflow run ...` commands with Markdown default and typed JSON.
MCP stays compact: run, inspect, and control tools plus paged resources. No MCP
client executes or schedules locally.

The dashboard shows definitions, versions, dependency graph, run timeline,
step/attempt state, inputs/outputs, executor/model route, queue/latency,
tokens/cost, effects, retries, cancellation, coverage, and legal controls from
daemon application views. Browser code never computes readiness or completion.

## Remote and host behavior

One daemon authority epoch owns each run. Remote hosts receive bounded typed execution
units and return addressed receipts; they never advance history, choose steps, or mint
leases. Failover verifies history/outbox/effect frontiers and fences the old owner.

Codex, Claude Code, Cursor, and Hermes bundles project the same cataloged
workflow operations. Existing Claude-generated workflow scripts may be retained
only as historical observations or explicit migration evidence; they are not
executed, translated, imported, or installed by PR17.

## Acceptance

PR17 is complete when definition validation/versioning, shared scheduling,
atomic history/outbox transitions, restart resume, effect reconciliation,
cancellation, bounded retries/fan-out, API/SDK/CLI/MCP/dashboard parity, remote
fencing, authorization/privacy, backup/restore, and fault-injection tests pass.
Tests must prove no duplicate observable effect, no false terminal success, no
ambient file/CWD authority, and no dependency on JavaScript, Markdown parsing,
taskgraph materialization, or arbitrary shell execution.

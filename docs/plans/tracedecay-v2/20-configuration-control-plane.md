# Configuration control plane

## Status / role

The V2 control plane is the sole typed authority for supported settings,
precedence, validation, effective values, opaque credential references, and
desired-versus-observed activation. CLI, MCP, HTTP, dashboard, Doctor, policy,
runtime, and provider discovery consume the same daemon-owned resolution.

Fresh V2 profiles accept one final validated snapshot. Missing, invalid,
unknown, or ambiguous settings fail closed with a typed reset or recreation
outcome; they are not inferred from paths, CWD, Git configuration, host files,
or observed habits. This plan adds no reader, converter, backfill, dual write,
or profile census for older internal snapshots.

An independently released public configuration protocol may retain explicit
version negotiation when release evidence identifies its external consumers.
Unreleased source-only request helpers and declarations change in place.

## User outcome

Before admitting work, a user can inspect the effective provider, model,
sandbox, content eligibility, budget, fallback, concurrency, and optional Git
restrictions that govern it. Admission pins the complete snapshot; later
changes affect only future admissions and never silently reinterpret an active
attempt.

## Canonical behavior

- Each setting has one typed definition with key, value type, default,
  validation, sensitivity, scope, documentation, and lifecycle owner.
- Resolution has one deterministic precedence order and returns an effective
  redacted value, provenance, validation state, restart requirements,
  desired/observed revisions, and snapshot identity.
- Ordinary valid mutations commit atomically with expected-revision CAS and
  activate through the canonical control plane. Invalid input and stale writers
  commit nothing.
- Credential writes are write-only and return opaque reference metadata.
  Plaintext credentials never appear in reads, history, audit, logs,
  diagnostics, UI, provider events, or receipts.
- Scope bindings and restrictive allow/deny policy use existing typed identity.
  Paths, labels, provider keys, host profiles, and source locators never create
  authority.
- Doctor reads typed configuration and activation evidence; remediation invokes
  an explicit authorized control-plane operation rather than a local write.
- Analyzer, renderer, feedback, delivery, and workflow settings remain
  registered values. A surface or adapter cannot supply an unregistered local
  default.

## Work and provider behavior

One complete work-execution snapshot covers the configured provider/backend/
model/protocol, executable reference, sandbox and approval mode, filesystem and
egress constraints, environment allowlist, opaque secret references, context
and output bounds, deadlines, cancellation, capacity, and concurrency.

The snapshot also constrains optional placement and Git topology: sealed roots,
protected refs, integration mode, clean/test/review gates, retention eligibility,
and notifications. It does not create task, runtime, or Git authority. Raw
paths and provider output never reach execution adapters as authority inputs.

Provider discovery reports capability evidence only. Plan 32 negotiates against
the pinned snapshot; a discovered executable or capability outside the allowed
reference/range is stale or unsupported, never an implicit setting change.

## Protected mutation and recovery

Protected dry-run changes no effective behavior. Apply re-resolves authority,
scope, roots, refs, provider evidence, and frozen digests before creating one
configuration revision and receipt. Drift, revocation, expiry, ambiguity,
policy widening, unsupported topology, or CAS conflict commits nothing.

Forward rollback creates a new valid revision after current validation. It never
rewinds tables, restores plaintext secrets, revives stale authority, or weakens
Git safety.

## Delivery and acceptance

1. Resolve and display the complete redacted snapshot in the real create-work
   and proposal path.
2. Pin it before a real provider step and expose requested/actual route plus
   activation drift in progress and outcome views.
3. Exercise protected setting changes through the existing dry-run and apply
   path only where a production journey needs them.
4. Test CAS races, invalid atomic mutation, credential secrecy, desired/observed
   drift, unavailable executables, adapter-local default attempts, mid-attempt
   rereads, narrowed authority, unsafe Git modes, rollback, and restart.

## Not in Plan 20

- A workflow-specific registry, provider-local configuration source, or
  configuration-only acceptance gate.
- Internal stored-state conversion or branch/history-derived configuration
  recovery.
- Configuration that schedules work, executes Git, invokes providers, mutates
  task graphs, or auto-applies replans.

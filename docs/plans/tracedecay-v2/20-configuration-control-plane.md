# Configuration control plane

## Status / role

The V2 control plane is the sole typed source for supported settings,
precedence, validation, revision history, effective values, opaque credential
references, and desired-versus-observed activation. CLI, MCP, HTTP, dashboard,
Doctor, policy, runtime, and provider discovery consume the same daemon-owned
resolution.

The canonical mechanism is the daemon-owned configuration authority and its
callable application reads and mutations. Stored revisions, redacted audit,
snapshot digests, and activation evidence are durable contracts; historical
schema-registry names, exact setting-definition files, migration packets, and
fixture inventories are not. Missing effective behavior or an unreachable
mutation is a gap; a renamed/deleted declaration scaffold is not.

The existing config files and stored settings/revisions on `origin/master` and
in live profiles are persistence evidence for the migration below. New
source-only/internal request helpers and registry declarations change in place.
Wire-visible request revisions retain negotiation until an authorized
installed-client/host census proves absence. Any branch-written config field,
stored setting/revision, snapshot, journal, checkpoint, or receipt remains
readable/migratable until the profile census proves absence.

**Reachability correction (updated 2026-07-27).** The former production-write
gap is closed. `ProductionConfigurationDaemonClient::mutate_direct` now rejects
scope drift, issues a short-lived exact-project grant, commits through the
canonical control plane, rereads and installs the new pinned snapshot, and
records the runtime component's activation. The daemon-hosted dashboard retains
the daemon's in-process `DaemonInvocationExecutor`; direct runtime acceptance
proves advertised apply, commit, revision advance, durable reread, no fallback
write to `config.json`, and stale-revision CAS rejection. The control-plane-less
dashboard fixture still correctly withholds apply and reports typed unavailable.

Startup forward-repair is also delivered for revisions created before the
current registry gained new defaults: it completes missing registered values
and provenance before repairing the daemon source binding, while unknown keys,
ambiguous authority, and conflicting protected bindings still fail closed.
This does not certify the complete PR17 work-execution snapshot or every
component's desired/observed lifecycle. The live semantic path is currently
disabled by an invalid configuration snapshot, so Plan 20 still owns that
operational repair and Plan 31 owns successful semantic activation.

PR17 adds only the settings needed by the executable work loop. It does not
create a workflow-specific registry or provider-local configuration source.

## PR17 user outcome

Before admitting work, a user can see the effective provider, model, sandbox,
local/remote content eligibility, budget, fallback, concurrency, and optional worktree/Git restrictions
that will govern it. The admitted run pins that complete snapshot. Later
configuration changes affect future admissions only; they never silently
reroute or reinterpret an active attempt.

## End-to-end production path

1. Work creation and evidence retrieval use the current authorized effective
   configuration and record its revision and behavior digest.
2. Proposal evaluation receives one complete snapshot containing task-shape
   bounds, route allowlists, budget and content-location limits, review requirements,
   deterministic fallback, and any optional topology restrictions.
3. Explicit provider admission resolves the configured executable reference
   and allowed provider/backend/model/protocol capabilities against Plan 27
   observations, then pins the complete snapshot before Plan 32 acquires a
   lease or starts a process.
4. Progress and outcome views show the pinned revision, requested and actual
   route, and any desired-versus-observed drift. No adapter rereads mutable
   settings mid-attempt.
5. Replanning uses the recorded snapshot and new evidence. Changing settings
   or accepting a replan is a separate authorized operation; neither happens
   implicitly.

## Retained control-plane behavior

- Each setting has one typed definition with key, value type, default,
  validation, sensitivity, scope, deprecation, and documentation.
- Resolution has one deterministic precedence order and returns effective
  redacted value, provenance, validation state, restart requirements,
  desired/observed revisions, and a snapshot identity.
- The snapshot has separate behavior and resolution-provenance digests, so
  moving an unchanged value between layers does not invalidate behavior while
  still remaining auditable.
- Ordinary valid mutations commit atomically with expected-revision CAS and
  activate directly. Invalid input or stale writers commit nothing.
- Credential writes are write-only and return opaque reference metadata.
  Plaintext credentials never appear in reads, history, audit, logs,
  diagnostics, UI, provider events, or receipts.
- Scope bindings and restrictive allow/deny policy remain typed references to
  existing ProjectId or verified projectless UserProfileId authority. Paths,
  CWD, labels, provider keys, host profiles, collections, and source locators
  never create identity or authority.
- The optional user-profile default collection remains a convenience selector
  only. Every referenced source is reauthorized; stale, missing, or denied
  defaults never fall back to all projects, CWD, the first project, or the
  newest collection.
- Deny rules accumulate, allowlists intersect with independently granted
  capability, and absence of an allow rule grants nothing.
- Protected scope-control and topology-policy changes retain dry-run, explicit
  apply, expected-revision CAS, actor-bound confirmation, idempotency,
  reauthorization, append-only redacted audit, crash recovery, and forward
  rollback. Ordinary scalar changes do not gain a ceremonial preview phase.
- Doctor reads typed configuration and activation evidence but can remediate
  only by invoking an explicit authorized control-plane operation.
- Analyzer configuration remains the canonical source for enablement,
  executable reference, arguments, initialization options, settings,
  environment allowlist, local/remote content eligibility, resource limits, restart policy, and
  per-language selection. Its revision/digest remains part of semantic-provider
  result identity and cache admission; host registration receives only the
  non-sensitive enabled-language projection.
- Plan 37 proximity configuration retains the threshold, score scale and input
  profile, eligible cohort, freshness decay, warning expiry, and
  suppression/deduplication windows. Configuration cannot disable or delay the
  immediate tier or widen authorization scope.
- Dashboard renderer selection, Scout/feedback quiet mode and delivery bounds,
  and typed workflow phase/boundary timing policy remain registered settings.
  Renderer choice cannot alter graph/query semantics, quiet mode cannot
  suppress critical safety evidence, and paper-reported constants do not
  become product defaults.
- List, explain, get, set, unset, atomic batch, write-only credential,
  observed-state, protected dry-run/apply, forward rollback, and audit remain
  semantically compatible across CLI, MCP, HTTP, dashboard, and Doctor.
- Stored configuration migration remains additive, idempotent, and
  behavior-preserving. It compares the migrated effective/provenance digests
  with the legacy resolver, imports only fully typed authorized values, and
  quarantines unknown, invalid, undecodable, path-derived, ambiguous, or
  authority-inventing values without truncating revision, receipt, plan,
  audit, or quarantine history. It never guesses a source binding, project,
  collection, worktree root, provider route, or topology default from CWD,
  environment, Git configuration, host files, or observed habits.

## PR17 configuration consumed by the loop

One complete work-execution snapshot covers:

- eligible provider/backend/model/protocol and reasoning choices, configured
  executable references and allowed versions, sandbox and approval mode,
  filesystem/network/egress constraints, environment allowlist, and opaque
  secret-reference policy;
- context, output, artifact, token, cost, deadline, cancellation, progress,
  reconnect/resume/restart, capacity, fairness, and concurrency limits;
- task-shape, decomposition, independent-review, evidence-coverage,
  calibration/drift, exploration, live-proposal cooldown/deduplication, human
  override, and deterministic-fallback rules; and
- optional placement, branch/review topology, protected refs, integration
  modes, branch naming, worktree roots, local-stack depth, clean/test/review
  gates, protected targets, escalation, retention eligibility, cleanup limits,
  and notification level; and
- every Plan 24/32 Work projection, workflow definition/run/control,
  provider-execution, placement/integration, handoff/experience,
  outcome/calibration, and SDK-facing operation that consumes configuration.

The snapshot must be complete and internally valid. Missing or invalid
  fallback, executable, provider, model, approval, cancellation, or
topology settings fail closed; an adapter, host bundle, discovery probe, or
surface cannot supply a local default.

## Provider and model rules

- Plan 27 reports discovered executable and protocol capability evidence; it
  cannot choose precedence, defaults, model, fallback, sandbox, environment,
  timeout, cancellation, or resume behavior.
- Plan 32 performs live negotiation against the pinned snapshot. A discovered
  executable or capability outside the configured reference/range is stale or
  unsupported, never an implicit configuration update.
- Claude-designated work uses the configured supported native Claude Code CLI.
  Hermes Anthropic is not a substitute.
- Codex-designated work uses the configured app-server route first when
  supported. Codex CLI is eligible only when the same pinned snapshot
  explicitly permits that fallback.
- Requested and actual provider/backend/executable/protocol/model/reasoning
  identity and the exact fallback decision remain visible in history.
- Human override can select only a snapshot-eligible route and cannot widen
  authority, budget, deadline, or egress.

## Topology and Git policy

Topology configuration constrains optional execution; it does not create task,
runtime, or Git authority. The safe default uses the existing worktree only,
one active attempt, no autonomous integration, no automatic cleanup, and
protected default and shared refs. Configured roots are sealed and resolved
through canonical scope; raw locators never reach host or provider adapters.

Branch naming is deterministic and generated only from bounded typed
components. Arbitrary templates, task text, prompts, shell fragments, paths,
regex selectors, and provider output are forbidden.

Any executable integration mode requires a clean exact destination, fresh
typed preflight, configured successful checks, explicit authorization,
protected-ref compliance, and support from the native Git owner. Force update,
rebase, amend, reset-based history replacement, branch deletion, backward ref
movement, and semantic conflict resolution are unrepresentable. Retention
marks eligibility only; cleanup still requires fresh scope, holder, dirty-data,
commit, PR, receipt, and effect reconciliation.

## Protected mutation and rollback

Protected dry-run changes no effective behavior. Apply re-resolves authority,
scope, roots, refs, provider evidence, and every frozen digest before one
atomic configuration revision and receipt. Drift, revocation, expiry,
ambiguity, policy widening, unsupported topology, or CAS conflict commits
nothing.

Forward rollback validates historical values against the current schema,
authority, provider observations, roots, refs, and safety floor, then creates a
new revision. It never rewinds tables, deletes audit history, restores
plaintext secrets, revives stale authority, or weakens Git safety.

Public missing, denied, revoked, or hidden targets remain
`restricted_or_unavailable` without stable identity, count, path, timing, or
cause disclosure. Audit reauthorizes target identity at read time.

## Implementation slices

1. Resolve and display the complete work-execution snapshot in the real create
   work and proposal path.
2. Pin that snapshot through one real provider admission and expose its
   requested/actual route and activation drift in progress/outcome views.
3. Exercise an optional protected topology change through existing dry-run and
   apply only when the production journey needs that Git behavior.
4. Exercise analyzer, proximity, scope-control, credential, observed-state,
   rollback, audit, all retained Work/workflow/provider/topology settings, and
   SDK-facing configuration operations through their existing production
   consumers; the rewrite removes no setting family or lifecycle.

The same slice adds the minimal typed setting, persistence, mutation, and
surface behavior it consumes. PR17 does not land standalone definition
registries, schema phases, provider configuration ports, exact file/type
inventories, or configuration-only gates.

## Replacement and deletion

- Remove provider/model/fallback/default values from host bundles, provider
  adapters, surfaces, task handlers, and workflow handlers.
- Remove any PR17 workflow-specific setting registry, shadow precedence path,
  mutable mid-run reread, or copied topology default.
- Keep migration input files read-only after cutover and quarantine undecodable
  or authority-inventing legacy values instead of guessing.

## Direct acceptance

The PR17 production journey must show the effective redacted snapshot, use it
for an explained proposal, pin it before a real provider step, preserve it
through progress, cancellation/restart, and terminal outcome, and use the
recorded revision when presenting a non-auto-applied replan.

Focused failures cover CAS races, invalid atomic mutation, credential
write/read/log handling,
desired/observed drift, absent or unsupported executables, provider/model
version drift, invalid fallback, adapter-local default attempts, mid-attempt
rereads, narrowed authority, protected-change expiry, topology/ref
drift, unsafe Git modes, forward rollback, and crash recovery. Cross-surface
tests compare the same effective value, provenance, safe error, and receipt;
one aggregate PR17 journey replaces standalone setting and fixture-corpus
gates.

## Not in PR17

- Public SDK configuration APIs are stabilized in PR18.
- PR20 may optimize resolution or activation only from measured production
  latency.
- Configuration never steers agents, mutates task graphs, schedules workflows,
  invokes providers, executes Git, or auto-applies replans.

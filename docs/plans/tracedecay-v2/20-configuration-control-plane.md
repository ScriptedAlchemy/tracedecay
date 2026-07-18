# Configuration Control Plane

## Status / Role

- Required V2 control plane.
- PR11 delivers the typed configuration core and daemon operations.
- PR13 registers Plan 37's threshold-tier proximity settings and their
  scale/profile/cohort revisions through this same control plane; the immediate
  tier remains unconditional.
- PR14 fully delivers the canonical Doctor/UI integration and observed
  activation state; PR17 extends it with auxiliary-provider configuration
  evidence through the same kernel.
- PR17 registers Plan 24 task/model-routing, exploration, budget, privacy, and
  deterministic-fallback settings through the same control plane.
- PR17 also registers every auxiliary-provider setting here; no catalog, host
  bundle, application handler, runtime adapter, or dashboard keeps a parallel
  executable/provider configuration source.
- Configuration changes activate directly after validation; there is no preview/apply workflow.

## Outcome

Every supported TraceDecay setting has one typed definition and one daemon-owned resolution path.
CLI, API, MCP, and UI read and mutate the same effective configuration, while credentials remain
opaque and operators can see which revision the running system actually uses.

## Owns

- Typed setting definitions, defaults, validation, and deprecation metadata.
- Configuration layers, precedence, provenance, and effective-value resolution.
- Atomic mutation, revision history, compare-and-set conflict handling, and audit metadata.
- One `ConfigurationSnapshotId` with separate
  `effective_behavior_digest` and `resolution_provenance_digest`; this plan
  alone defines their resolution and identity semantics.
- Direct activation and observed daemon/component revision state.
- Opaque credential references and write-only credential mutation surfaces.
- One typed analyzer configuration source for enablement, executable reference,
  arguments, initialization options, settings, environment allowlist, privacy
  class, resource limits, restart policy, and per-language selection for
  [35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- Canonical analyzer configuration source and revision/digest consumed by
  [09](09-application-crate.md) provider-result identity and
  [35](35-daemon-lsp-gateway-and-universal-diagnostics.md) runtime snapshot
  composition.
- CLI, API, MCP, and UI configuration surfaces.
- Typed configuration validation, provenance, desired/observed activation, and
  drift evidence consumed by the one Plan 14 Doctor kernel.
- Typed PR17 bounds for task/model route allowlists, cohort/sample/coverage
  floors, exploration share, latency/token/cost limits, rollback thresholds,
  circuit breakers, privacy/egress ceilings, and deterministic fallback;
  task-shape scales and unknown thresholds; calibrated size bands and
  calibrated-probability/interval validity, support, error, and coverage
  floors; ordinal-rank and heuristic-scale revisions;
  decomposition/parallelism and integration-gate limits; independent-review
  requirements; estimator/cohort/horizon and model-version drift rules;
  censoring ceilings; live proposal triggers and cooldown/dedupe; and
  human-override/approval requirements.
- The sole typed PR13 configuration definitions and resolution rules for
  [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)'s
  threshold-tier proximity risk threshold, versioned risk-score scale and
  input profile, authorization-scoped eligible-cohort revision, freshness
  decay, warning expiry, and suppression/dedupe windows. Plan 37 retains
  proximity inputs, tier, warning, and advisory-delivery semantics; Plan 20
  alone owns these setting definitions, defaults, precedence, validation,
  revisions, and effective snapshot.
- The sole typed PR17 auxiliary-provider configuration definitions and
  resolution rules for executable references; allowed executable/protocol/
  model version ranges; provider/backend enablement and preference; sandbox,
  approval, filesystem/network/egress and environment-disclosure policy;
  opaque secret-reference policy; model/reasoning defaults and allowlists;
  context/output/artifact/token/cost budgets; deadline, cancellation,
  interrupt/terminate/kill escalation and grace periods; progress/heartbeat
  bounds; reconnect/resume/restart policy; capacity/fairness limits; and the
  explicit Codex app-server-to-CLI fallback policy.

## Does not own

- Feature business logic beyond the settings contract each feature registers.
- Secret detection and sink enforcement; Plan 18 consumes opaque references and protects outputs.
- Task assignment, agent steering, work-graph mutation, model-route decisions,
  plan execution, or workflow scheduling. Plan 06/24/32 consume the typed
  settings and pinned revision; configuration never applies those decisions.
- Executable discovery, installation, repair, host capability probes, stock-host
  conformance, provider invocation/supervision, process/session adoption,
  leases, attempts, receipts, or remediation execution. Plan 27 discovers and
  remediates against this plan's effective snapshot; Plan 32 executes against
  one pinned snapshot.
- Preview/apply/rollback ceremonies for normal configuration changes.
- Dynamic workflow definitions; PR17 stores those as typed product data using daemon operations.
- Generated inventories, Markdown parsers, trackers, executors, or workflow JavaScript.
- Static extension, language-ID, root-marker, and parser facts owned by
  [25](25-code-intelligence-indexing-crate.md).
- Semantic-provider cache storage, eviction, reuse, invalidation execution, and
  lifecycle; those remain owned solely by
  [35](35-daemon-lsp-gateway-and-universal-diagnostics.md), which consumes only
  the canonical configuration source and digest this plan publishes.
- Doctor repair, remediation, or implicit mutation of configuration state; PR14
  owns the canonical Doctor kernel, diagnosis, finding identity, and
  remediation orchestration. Explicit configuration mutation still invokes
  this plan's typed operation with authorization, dry-run/preview where
  applicable, idempotency/CAS, receipts, rollback/recovery, and audit.

## Required behavior

1. One typed source
   - Each setting declares its key, type, default, validation, sensitivity, scope, and documentation.
   - CLI, API, MCP, UI, Doctor, and persisted encoding use that definition directly.
   - Analyzer settings cover enablement, executable reference, arguments,
     initialization options, settings, environment allowlist, privacy class,
     limits, restart policy, and per-language selection without duplicating
     Plan 25's static language facts.
   - Each analyzer-configuration mutation publishes a new canonical revision and
     digest. That digest participates in Plan 09 provider-result identity and
     Plan 35 cache keys. Any change to executable reference, arguments,
     initialization options, settings, environment allowlist, privacy policy,
     limits, restart policy, or per-language selection invalidates exactly the
     affected provider cache entries; this plan owns the configuration source
     and digest only, not cache storage, reuse policy, or invalidation
     execution.
   - Host plugin projection may consume only the non-sensitive enabled-language
     registration selection. Executable references, arguments, initialization
     options, settings, environment, and credentials never enter host
     registration artifacts.
   - Untrusted LSP requests cannot select analyzer commands, arguments,
     initialization options, settings, or environment values.
   - Every Plan 37 threshold-tier evaluation pins the
     `ConfigurationSnapshotId`, threshold key revision, risk-scale/input-profile
     revision, and eligible-cohort revision it consumed. A numeric risk
     assessment declares `ordinal_rank`, `heuristic_score`,
     `calibrated_probability`, or `calibrated_interval` plus producer/origin,
     scale or calibration revision, evidence anchors, and coverage. A heuristic
     never renders as probability, and probability/interval semantics require
     held-out cohort, horizon, support, error, and drift-validity metadata.
     Configuration cannot disable or delay Plan 37's immediate tier or widen
     authorization/privacy scope.
   - Unknown and deprecated keys produce structured, actionable diagnostics.

2. Deterministic resolution
   - Explicit layers have one documented precedence order.
   - Reads return setting-schema and resolver revisions, effective/redacted
     value, ordered candidate-layer revisions, winning reason,
     overridden/defaulted/rejected layers with safe reasons,
     validation/deprecation state, restart requirement, desired/observed
     revisions, sensitivity metadata, and the snapshot identity.
   - `effective_behavior_digest` is the behavior/cache identity;
     `resolution_provenance_digest` is audit/replay identity. Moving an
     unchanged value between layers may change provenance without changing
     behavior; changing the effective winner changes both.
   - Resolution is pure and testable; adapters do not implement precedence independently.

3. Atomic direct activation
   - A valid mutation commits one new revision and becomes desired active configuration immediately.
   - Invalid input commits nothing and leaves the previous revision active.
   - Compare-and-set rejects stale concurrent writes with the current revision.
   - Multi-setting updates validate and commit atomically.

4. Observed state
   - The daemon records which configuration revision each long-lived component has loaded.
   - Gateway and analyzer state names the loaded configuration revision,
     analyzer definition revision, and activation or restart failures.
   - Surfaces distinguish desired revision from observed revision and show pending restart or failure.
   - Activation failures preserve the last working runtime state and expose an actionable error.

5. Opaque credentials
   - Configuration stores credential references, never returned plaintext values.
   - Credential writes use a dedicated write-only operation and return only reference metadata.
   - Reads, history, audit events, errors, logs, UI, and diagnostics cannot reveal credential values.

6. Shared surfaces
   - CLI, API, MCP, and UI support list, explain, get, set, unset, and atomic batch mutation.
   - All surfaces return the same validation errors, revisions, provenance, and observed state.
   - UI groups settings by product capability and makes overrides and restart requirements visible.

7. Doctor integration
   - Plan 20 emits typed evidence for invalid persisted values, unknown keys,
     deprecated keys, unresolved credential references, precedence mistakes,
     and desired/observed revision drift.
   - Plan 14 Doctor alone detects, identifies, aggregates, explains, and
     presents those findings. Detection is read-only; automatic or implicit
     repair is forbidden.
   - Doctor remediation invokes separate Plan 20 explicit confirmed operations
     with authorization, dry-run/preview where applicable, idempotency/CAS,
     receipts, rollback/recovery, and audit.

8. PR17 auxiliary providers
   - One effective provider snapshot contains every setting named above plus
     its scope, provenance, revision, digest, sensitivity, and desired/observed
     activation state. Partial snapshots are invalid.
   - Plan 27 discovery and conformance receive the resolved non-secret
     executable/version/capability expectations and return observed evidence;
     they cannot choose precedence, defaults, fallback, environment disclosure,
     model/reasoning, timeout, cancellation, kill, or resume behavior.
   - Plan 32 pins exactly one snapshot at admission and includes its revision
     and digest in negotiation, lease/attempt identity, event history, and
     terminal receipt. An adapter cannot read mutable configuration again to
     change an admitted attempt.
   - A discovered executable or capability outside the configured reference or
     allowed range is `stale`/`unsupported`, never an implicit setting update.
     Missing or invalid fallback policy fails closed.
   - Auxiliary configuration changes publish a new revision and invalidate only
     affected future admissions and observed activation state. They never
     rewrite or silently re-route an admitted attempt.

9. Adjudicated settings
   - Register dashboard renderer selection/capability with a permissive
     default and optional adapter; no renderer setting selects graph/query
     semantics.
   - Register Scout/feedback quiet mode, delivery interval/window limits, and
     typed phase/boundary timing policy. Paper-reported timing and rate
     constants are not defaults.
   - Register PR17 topology-assessment, selective-escalation, minimal-repair,
     task-recall quarantine/retirement, role-isolation, and bounded-exploration
     controls. These settings constrain Plan 24 proposals and Plan 32
     execution; they never create either authority.

## Acceptance

- PR11 ships the typed registry, deterministic resolver, revisioned store, atomic daemon operations,
  compare-and-set behavior, direct activation, and opaque credential references.
- Cross-surface tests prove CLI, API, MCP, and UI observe identical values and errors.
- Concurrent writers cannot lose updates or partially commit a batch.
- Credential values never appear in reads, history, audit data, logs, diagnostics, or UI payloads.
- PR14 ships complete configuration UI, Doctor read-only checks, explicit configuration
  remediation operations, and desired-versus-observed state.
- Restart-required and failed-activation scenarios preserve the last working runtime configuration.
- Resolution fixtures are byte-stable across winning, overridden-only,
  layer-move, invalid/deprecated, and secret-reference-rotation cases; they
  prove behavior and provenance digests change only for their declared
  purposes and no cycle/attempt rereads mutable configuration after pinning.
- PR13 direct proximity-configuration tests prove CLI/API/MCP/UI parity for
  threshold, scale/input-profile, eligible-cohort, freshness, expiry, and
  suppression/dedupe settings; byte-stable above/below-threshold evaluation
  against pinned evidence; immediate-tier emission regardless of threshold;
  exact configuration/cohort revision capture; stale-revision rejection; and
  no adapter-local default, mutable reread, or authorization widening.
- Score-schema validation rejects numeric outputs missing producer/origin,
  score kind, scale/calibration revision, evidence anchors, or coverage.
  Held-out evaluation reports ranking quality and probability/interval
  calibration error, support, coverage, horizon, and drift by eligible cohort;
  incomparable score kinds or scale revisions cannot be ordered or averaged.
- PR17 tests prove task/model-routing limits resolve identically across
  surfaces, activation is versioned/audited, unsafe widening is rejected, and
  missing or invalid settings select the declared deterministic fallback.
  Estimator, cohort, horizon, drift, censoring, decomposition, review, and live
  proposal settings are pinned into recommendations and never activate an
  accepted graph/runtime change by themselves.
- PR17 auxiliary-provider configuration fixtures prove one definition and
  precedence path across CLI/API/MCP/UI; complete snapshot/digest stability;
  unknown/deprecated/invalid executable, protocol/model range, sandbox,
  environment, fallback, deadline/kill, and resume settings; secret opacity;
  desired-versus-observed drift; Plan 27 read-only consumption; Plan 32
  admission pinning; and no adapter-local defaults or mid-attempt reread.
- No task steering, developer-plan machinery, preview/apply pipeline, or
  workflow JavaScript is present.

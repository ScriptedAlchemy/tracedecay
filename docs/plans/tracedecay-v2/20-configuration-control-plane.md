# Configuration Control Plane

## Status / Role

- Required V2 control plane.
- PR11 delivers the typed configuration core and daemon operations.
- PR14 fully delivers UI, Doctor integration, and observed activation state.
- Configuration changes activate directly after validation; there is no preview/apply workflow.

## Outcome

Every supported TraceDecay setting has one typed definition and one daemon-owned resolution path.
CLI, API, MCP, and UI read and mutate the same effective configuration, while credentials remain
opaque and operators can see which revision the running system actually uses.

## Owns

- Typed setting definitions, defaults, validation, and deprecation metadata.
- Configuration layers, precedence, provenance, and effective-value resolution.
- Atomic mutation, revision history, compare-and-set conflict handling, and audit metadata.
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
- Doctor read-only detection and explanation for configuration state.

## Does not own

- Feature business logic beyond the settings contract each feature registers.
- Secret detection and sink enforcement; Plan 18 consumes opaque references and protects outputs.
- Task assignment, agent steering, plan execution, or workflow scheduling.
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
  owns explicit confirmed remediation operations with authorization,
  dry-run/preview where applicable, idempotency/CAS, receipts,
  rollback/recovery, and audit.

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
   - Unknown and deprecated keys produce structured, actionable diagnostics.

2. Deterministic resolution
   - Explicit layers have one documented precedence order.
   - Reads return the effective value, source layer, revision, and whether a restart is required.
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
   - Doctor detects invalid persisted values, unknown keys, deprecated keys, unresolved credential
     references, precedence mistakes, and desired/observed revision drift.
   - Doctor detection and explanation is read-only. Automatic or implicit repair is forbidden.
   - PR14 remediation uses separate explicit confirmed operations with authorization,
     dry-run/preview where applicable, idempotency/CAS, receipts, rollback/recovery, and audit.

## Acceptance

- PR11 ships the typed registry, deterministic resolver, revisioned store, atomic daemon operations,
  compare-and-set behavior, direct activation, and opaque credential references.
- Cross-surface tests prove CLI, API, MCP, and UI observe identical values and errors.
- Concurrent writers cannot lose updates or partially commit a batch.
- Credential values never appear in reads, history, audit data, logs, diagnostics, or UI payloads.
- PR14 ships complete configuration UI, Doctor read-only checks, explicit configuration
  remediation operations, and desired-versus-observed state.
- Restart-required and failed-activation scenarios preserve the last working runtime configuration.
- No task steering, preview/apply pipeline, plan machinery, or workflow JavaScript is present.

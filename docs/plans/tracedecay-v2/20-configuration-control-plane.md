# TraceDecay V2 Configuration Control Plane Implementation Plan

> **For agentic workers:** implement this plan in the program order below. Every slice must preserve the contract, privacy, scope, transport-parity, and migration gates before the next slice becomes the default.

**Goal:** Replace TraceDecay's scattered files, flags, environment variables, dashboard toggles, provider metadata, hook defaults, daemon settings, and hidden constants with one typed, versioned configuration control plane. Every user-controllable non-secret setting is discoverable, searchable, explainable, and editable in the Brain Settings workspace and through generated CLI, MCP, HTTP, and SDK bindings.

**Architecture:** Generic configuration identity, value, provenance, impact, and version contracts live in `tracedecay-domain`; each owning subsystem contributes a typed module manifest; build-time generation produces one configuration registry; `tracedecay-application` is the only resolver and mutation owner; profile/project repositories persist immutable layer revisions and activation manifests; root composition supplies process/environment observations and applies runtime changes. All surfaces consume generated application contracts. Safety floors, especially redaction, are constraints over effective values and cannot be disabled or weakened by any lower layer.

**Normative dependencies:** [`01-domain-crate.md`](01-domain-crate.md), [`02-store-crate.md`](02-store-crate.md), [`06-policy-crate.md`](06-policy-crate.md), [`08-tool-catalog-crate.md`](08-tool-catalog-crate.md), [`09-application-crate.md`](09-application-crate.md), [`10-api-crate.md`](10-api-crate.md), [`11-dashboard-frontend.md`](11-dashboard-frontend.md), [`12-root-compatibility-migration.md`](12-root-compatibility-migration.md), [`16-cross-project-repository-worktree-scope.md`](16-cross-project-repository-worktree-scope.md), [`17-official-public-api-and-sdks.md`](17-official-public-api-and-sdks.md), [`18-secret-detection-redaction-and-private-data-safety.md`](18-secret-detection-redaction-and-private-data-safety.md), [`19-system-defragmentation-convergence-and-extensibility.md`](19-system-defragmentation-convergence-and-extensibility.md), the binding/presentation contract in [`21-cli-mcp-tool-surface-and-output-unification.md`](21-cli-mcp-tool-surface-and-output-unification.md), the optional scout controls in [`22-incremental-context-scout-and-suggestion-envelopes.md`](22-incremental-context-scout-and-suggestion-envelopes.md), temporal retrieval profiles in [`23-session-lcm-temporal-retrieval-and-evaluation.md`](23-session-lcm-temporal-retrieval-and-evaluation.md), and task/executor control families in [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md).

---

## 1. Contract lock

1. There is one configuration registry, one application resolver, one immutable history model, and one effective-snapshot format. CLI, MCP, HTTP, SDKs, hooks, daemons, dashboard, automations, and tests cannot own parallel defaults or precedence rules.
2. Every user-controllable non-secret setting is represented in the registry. It is searchable and explainable on every generated surface, editable at every legal writable layer, and never controllable only through a hidden file, environment variable, or dashboard-only toggle.
3. Built-in defaults, the non-disableable safety floor, process observations, and foreign-owned host state are visible but read-only. If a behavior is intended to be user-configurable, its registry descriptor must expose at least one supported writable layer rather than requiring an environment-only escape hatch.
4. Secret material is not configuration data. Configuration stores only opaque keyring/vault references and safe availability metadata. Values, prefixes, fingerprints, connection strings, tokens, headers, and environment expansions never appear in configuration reads, history, audit, logs, SSE, exports, diagnostics, URLs, or browser storage.
5. `ScopeSelectorV2` is the only selector used to resolve repositories, projects, checkouts, worktrees, providers, hosts, and related entities. Durable configuration ownership records `DeclaredScope`; no route, CWD, current branch, selected dashboard node, or last-used project silently supplies ownership.
6. A configuration layer can strengthen a safety invariant but cannot weaken its parent or the built-in floor. Unknown coverage, missing detectors, invalid policy, unavailable credential references, or incompatible runtime versions fail closed where safety is involved.
7. Ordinary configuration uses direct validate-and-save semantics with optimistic concurrency. The product does not force a `preview -> apply -> rollback` ceremony for routine edits.
8. Inline impact is informational and exact: hot reload, next request, new agent session, host restart, daemon restart, store reopen, rescan, reproject, reindex, migration, or unsupported. A separate destructive system operation may require explicit confirmation; saving a non-destructive setting does not.
9. Configuration history is append-only. Returning to prior non-secret values creates a new forward revision and revalidates the current schema and safety floor; history is never rewritten and an old unsafe effective state cannot be resurrected.
10. Curation is fully autonomous. Memory curation, session reflection, skill writing/evolution, fact reconciliation, and related self-improvement do not expose per-item preview, apply, reject, approval, or rollback queues. Configuration controls the global/scoped autonomy policy, schedules, budgets, quality floors, and failure behavior; the autonomous engine executes and audits items itself.
11. Replay labs are read-only. They can resolve historical versus current effective configuration and measure resulting policy behavior, but they cannot mutate settings or become an approval path for curation.
12. Generated bindings and the Settings workspace are projections of the same registry and application use cases. Hand-authored forms, CLI switches, MCP schemas, OpenAPI fields, or SDK options that introduce an unregistered setting fail CI.
13. The control plane records desired, activated, effective, and observed runtime state separately. A persisted value is not claimed effective until its consuming component acknowledges the exact configuration generation.
14. Cross-shard updates do not pretend SQLite provides distributed transactions. Revisions are staged in their owning shards and become visible together only through an atomically published activation manifest; failures before publication leave the previous generation effective.
15. V1 configuration readers are bounded migration inputs only. After cutover there is no live fallback to legacy files, dashboard state, plugin metadata, or stale daemon defaults.

## 2. Why this control plane exists

TraceDecay currently exposes behavior through many unrelated mechanisms: root/profile/project files, CLI flags, process environment, provider installation metadata, hook payload settings, dashboard mutations, daemon startup options, tool-specific defaults, memory and automation policy, database layout choices, and code constants. The result is hard to reason about:

- users and agents cannot enumerate what can be configured;
- the same concept can have different names and defaults across CLI, MCP, hooks, and dashboard;
- a displayed value rarely explains its source, precedence, or consuming process;
- settings can be accepted but not active until an undocumented restart;
- changing search, privacy, capture, or indexing behavior can leave incompatible projections behind;
- configuration copied between projects loses identity and scope;
- secrets can accidentally become printable config values;
- per-provider metadata can weaken protections that were expected to be global;
- automation and curation controls can be mistaken for item approval queues;
- hand-edited files and environment overrides create drift that doctor cannot explain.

The V2 control plane makes configuration part of the observable system. An agent or human can ask: “What controls this behavior, what is effective here, why, what changed, which components consumed it, and what must happen next?” and receive one stable answer across all surfaces.

## 3. Goals and non-goals

### 3.1 Goals

- Define every setting once with stable identity, type, constraints, legal layers, merge semantics, sensitivity, documentation, deprecation, and operational impact.
- Make Brain `/settings` the complete visual control surface, not a partial dashboard subset.
- Make `tracedecay config` an agent-friendly JSON API and a human-navigable terminal tree generated from the same registry.
- Explain every effective value as a complete source chain, including ignored, shadowed, clamped, invalid, stale, and pending layers.
- Support profile, project, repository, checkout/worktree, provider/source, and host/runtime targets without stringly paths or implicit CWD behavior.
- Make configuration updates compare-and-swap, idempotent, auditable, and safely visible to many simultaneous readers and writers.
- Publish bounded SSE changes so Settings, status, hooks, agents, and daemons converge without polling races.
- Expose exact restart, rescan, reproject, reindex, migration, credential, and compatibility consequences before save and track them after save.
- Keep redactor, detector, privacy, retention, and quarantine controls fully visible while preserving a non-disableable safety floor.
- Preserve autonomous curation while making its policy, budgets, schedule, health, and outcomes observable and configurable.
- Support safe non-secret import/export, declarative fleet setup, diff, drift detection, and version migration.
- Pin configuration digests in query, policy, replay, projection, sanitization, hook, and audit receipts.

### 3.2 Non-goals

- No general secret manager, plaintext credential editor, secret reveal endpoint, or encrypted-secrets-in-config-file feature.
- No second scope language. Human locators are resolved through `ScopeSelectorV2` and persisted targets use canonical IDs plus `DeclaredScope`.
- No dashboard-side precedence, cross-field validation, impact inference, or restart logic.
- No generic JSON map whose meaning is known only to a consumer.
- No setting that bypasses typed application commands through direct file/database edits.
- No remote control plane in the first V2 default. The official local API can later be bound remotely only under the security model in plans 10 and 17.
- No per-item curation proposal review, manual promotion inbox, approval gate, or item rollback workflow.
- No automatic destructive migration merely because a setting changed.
- No claim of all-or-nothing cross-shard database writes; only atomic effective-generation publication after every staged revision validates.

## 4. Canonical ownership and dependency flow

```text
owning crate manifests + domain config contracts
                    │
                    ▼
          generated registry artifact
                    │
     ┌──────────────┼────────────────┐
     ▼              ▼                ▼
 store revisions  application      generated schemas
 + activations    resolver/commands  + clients/forms/CLI
     │              │                │
     └──────────────┼────────────────┘
                    ▼
          effective snapshot/digest
                    │
     ┌──────────────┼───────────────────────────┐
     ▼              ▼                           ▼
 hooks/agents   daemon/projectors/query   Settings/status/doctor
     │              │                           │
     └──────── component acknowledgements ──────┘
```

| Concern | Canonical owner | Consumers | Forbidden duplicate |
|---|---|---|---|
| Generic config IDs/types/provenance/impact | `tracedecay-domain::config` | Every crate and generated schema | transport-local setting structs with different semantics |
| Subsystem setting definitions | Owning crate's `ConfigModuleManifestV1` | Registry generator | dashboard forms or root constants defining public keys |
| Registry validation/generation | build tooling plus schema registry | tool catalog, API, CLI, dashboard, docs | runtime plugin discovery inventing unvalidated core keys |
| Layer revision and activation persistence | `tracedecay-store` repositories | application | raw SQL/files from transports or consumers |
| Target/scope resolution | application scope resolver using `ScopeSelectorV2` | config queries/commands | config-specific project/path selector |
| Precedence, merge, constraint, effective digest | `tracedecay-application::configuration` | every runtime | provider/hook/daemon-local resolution |
| Runtime application/acknowledgement | root composition and owning runtime adapter | status/application | claiming effective from persisted desired state |
| Public use-case identity and binding | application plus tool catalog | CLI/MCP/HTTP/SDK/dashboard | hand-maintained transport commands |
| Privacy floor and eligible content types | plans 01 and 18 | registry/application/all outputs | user-disableable redactor or printable secret value |
| Settings rendering | generated schema/view model | Brain dashboard | hand-authored validation/default/precedence |

Do not create a broad `tracedecay-config` crate initially. The convergence contract in plan 19 remains: domain owns generic contracts and application owns resolution. Extract a narrow crate only if at least two independent binaries need the identical resolver without application and the extraction preserves the dependency DAG.

## 5. Domain contracts

Create `crates/tracedecay-domain/src/config.rs` with opaque validated identifiers and exhaustive enums:

```rust
pub struct ConfigKey(CatalogValue);
pub struct ConfigModuleId(CatalogValue);
pub struct ConfigRegistryVersion(u64);
pub struct ConfigRegistryDigest(ManifestDigest);
pub struct ConfigLayerId(EntityId);
pub struct ConfigRevisionId(EntityId);
pub struct ConfigActivationId(EntityId);
pub struct EffectiveConfigSnapshotId(EntityId);
pub struct EffectiveConfigDigest(ManifestDigest);
pub struct ConfigConsumerId(CatalogValue);
pub struct CredentialRefId(EntityId);

pub enum ConfigValueKindV1 {
    Boolean,
    SignedInteger,
    UnsignedInteger,
    Decimal,
    Duration,
    ByteSize,
    String,
    Enum,
    StringSet,
    OrderedList,
    TypedMap,
    ScopeReference,
    EntityReference,
    CredentialReference,
    Structured,
}

pub enum ConfigLayerKindV1 {
    BuiltInDefault,
    SafetyFloor,
    Profile,
    Project,
    Repository,
    Worktree,
    Provider,
    Host,
    EnvironmentObservation,
    RequestOverride,
}

pub enum ConfigChangeabilityV1 {
    ReadOnly,
    Writable,
    EphemeralOverride,
    Generated,
    ForeignObserved,
}

pub enum ConfigMergeStrategyV1 {
    Replace,
    AppendUnique,
    SetUnion,
    MapOverlay,
    Minimum,
    Maximum,
    ConstrainedByFloor,
}

pub enum ConfigImpactKindV1 {
    HotReload,
    NextRequest,
    NewAgentSession,
    RestartHost,
    RestartDaemon,
    RestartDashboard,
    ReopenStore,
    PrivacyRescan,
    Reproject,
    Reindex,
    StorageMigration,
    DataRetirement,
    UnsupportedWhileRunning,
}
```

These identifiers are genuinely opaque: inner values are private, and construction goes only through validated `parse`/`TryFrom` constructors, so no crate can mint an unvalidated key or ID.

`ConfigKey` uses stable dotted IDs such as `privacy.detectors.runtime.enabled`, `hooks.hints.max_per_turn`, and `query.search.lexical.max_candidates`. Display labels are localized metadata, never identity. Renames retain aliases and an explicit migration; a key cannot be silently reused for a different type or meaning. When a key leaves the registry entirely, stored layer revisions that still contain it remain immutable history: the resolver excludes the orphaned entries from effective resolution and surfaces them as typed `orphaned_key` items in `config.status`, `config.history`, and `config.diff` with migration guidance; they are never silently dropped, reinterpreted, or revived by re-registering the same name with different semantics. Extension-owned orphans additionally follow Section 19.

### 5.1 Module descriptor

Each owning crate exports a static/generated manifest:

```rust
pub struct ConfigDescriptorV1 {
    pub key: ConfigKey,
    pub module_id: ConfigModuleId,
    pub schema_id: SchemaId,
    pub value_kind: ConfigValueKindV1,
    pub default: CatalogValue,
    pub allowed_layers: Vec<ConfigLayerKindV1>,
    pub precedence: Vec<ConfigLayerKindV1>,
    pub merge: ConfigMergeStrategyV1,
    pub sensitivity: ConfigSensitivityV1,
    pub changeability: ConfigChangeabilityV1,
    pub constraints: Vec<ConfigConstraintV1>,
    pub consumers: Vec<ConfigConsumerId>,
    pub impacts: Vec<ConfigImpactRuleV1>,
    pub ui: ConfigUiMetadataV1,
    pub docs: ConfigDocumentationV1,
    pub introduced_in: SchemaVersion,
    pub deprecated: Option<ConfigDeprecationV1>,
}

pub struct ConfigModuleManifestV1 {
    pub module_id: ConfigModuleId,
    pub owner_crate: CatalogValue,
    pub version: SchemaVersion,
    pub descriptors: Vec<ConfigDescriptorV1>,
    pub cross_field_constraints: Vec<ConfigConstraintProgramV1>,
    pub migrations: Vec<ConfigMigrationRefV1>,
}
```

Rules:

- Defaults are typed canonical values, not JSON snippets or values parsed independently by each consumer.
- Constraints are deterministic, bounded, side-effect-free programs with stable reason codes.
- A descriptor explicitly lists legal layers and a total precedence for the dimensions it accepts, subordinate to the normative Section 6.1 skeleton: the `precedence` vector orders only the step 3 dimension layers. Repository, worktree, provider, and host are not assigned an accidental global order.
- Merge strategies are closed enums. Arbitrary code callbacks cannot make effective resolution nondeterministic.
- `String` and structured text descriptors declare maximum sizes and pass plan 18 sanitization before persistence, history, rendering, or export.
- `CredentialReference` stores only an opaque reference and safe provider/status metadata.
- Every impact rule names the consuming component, trigger predicate, required operation capability, and whether the old value remains effective while work is pending.
- UI grouping, labels, examples, documentation, enum options, and accessibility descriptions are generated from the descriptor. They do not redefine semantics.

### 5.2 Registry generation and validation

The build generator combines all manifests into one registry artifact, `generated/config-registry-v1.json`: typed descriptors, JSON Schema fragments, and the `ConfigRegistryDigest`. The pipeline runs in exactly one direction from there: plan 08's catalog build consumes that file as an input manifest, pins `ConfigRegistryDigest` in `ToolCatalogSnapshot`, and is the sole emitter of config surface metadata — OpenAPI fragments, CLI metadata, MCP schemas, SDK types, dashboard form metadata, docs, and conformance fixtures; plan 21 renders only from those plan 08 catalog artifacts. The registry generator emits no second surface-metadata set. Registry generation is byte-identical across platforms, path syntax, time zones, locales, and map insertion order; CI runs the generator twice from a clean tree and compares digests. In program order, PR 22A lands the catalog consuming the frozen Phase-0 registry subset; PR 22C completes the registry, and every registry change regenerates the plan 08 catalog in the same commit — registry before catalog in every build.

Generation fails when:

- a key, alias, module ID, consumer ID, or schema ID is duplicated;
- a writable setting has no writable layer or generated mutation capability;
- a user-controllable setting is environment-only;
- a secret-bearing type is printable/exportable or lacks `CredentialReference` semantics;
- a safety-critical key permits a layer that can weaken its floor;
- precedence omits or ambiguously orders an allowed layer pair;
- a consumer is unknown or has no acknowledgement protocol;
- an impact lacks a status/operation mapping;
- a deprecated key lacks replacement, migration, and removal policy;
- dashboard, CLI, MCP, HTTP, SDK, docs, and registry key inventories differ;
- a configuration example fails the privacy scan.

CI also inventories legacy config reads, direct environment access, root flags, provider metadata, dashboard forms, and constants. Every retained public behavior must map to a registry key or an explicitly documented non-config runtime observation.

## 6. Scope, targets, and ownership

Configuration needs both query scope and durable ownership. They are not interchangeable.

```rust
pub struct ConfigTargetV1 {
    pub layer_kind: ConfigLayerKindV1,
    pub target: ConfigTargetRefV1,
    pub declared_scope: DeclaredScope,
    pub resolution_id: ScopeResolutionId,
}

pub enum ConfigTargetRefV1 {
    Profile(ProfileId),
    Project(ProjectId),
    Repository(EntityRef),
    Worktree(EntityRef),
    Provider(EntityRef),
    Host(EntityRef),
}
```

- Reads accept `ScopeSelectorV2`, resolve it once through the application resolver, and return every eligible target plus ambiguity, stale, unavailable, quarantined, or missing coverage.
- Mutations require exactly one canonical `ConfigTargetV1` per layer patch. A multi-target request is a batch workflow, not a string wildcard.
- `declared_scope` controls canonical shard ownership exactly as plan 01 specifies. The application verifies that the target/entity evidence supports it; it never derives ownership from a route or CWD.
- Profile/provider/host settings are normally profile-owned. Project settings are project-owned. Repository/worktree settings require explicit project, cross-project, or zero-project ownership according to their canonical relation evidence; the same repository path cannot be guessed into an arbitrary project.
- Cross-project settings use the exact versioned `DeclaredScope::CrossProject` membership digest. Membership changes do not silently widen a previously saved config layer.
- A repository or worktree locator entered in UI/CLI is a sanitized `ScopeTargetV2::Locator` inside `ScopeSelectorV2`; the stored target is the resolved canonical `EntityRef`.
- `CurrentInvocation` is allowed only when the caller deliberately chooses it. `tracedecay config set` does not narrow to CWD by omission.
- The default Settings workspace reads explicit active-profile `AllAuthorized`; project routes add a visible filter without changing ownership.

### 6.1 Layer precedence

The resolver evaluates, for each key, only layers declared by its descriptor:

1. typed built-in default establishes a complete value;
2. profile layer establishes the user's baseline;
3. applicable host/provider/project/repository/worktree layers merge in the descriptor's explicit order;
4. allowed request override applies only to keys marked ephemeral and never persists;
5. the safety-floor constraint clamps or rejects the result and records why;
6. cross-field constraints validate the complete snapshot;
7. runtime compatibility can hold a desired value pending rather than pretending it is effective.

This seven-step skeleton is normative. A descriptor's `precedence` vector orders only the step 3 dimension layers it accepts (`Host`, `Provider`, `Project`, `Repository`, `Worktree`); it cannot reorder `BuiltInDefault`, `Profile`, `RequestOverride`, the safety floor, or cross-field validation, and the generator rejects a vector that lists a layer outside step 3 or leaves an allowed step 3 pair ambiguously ordered. `EnvironmentObservation` layers evaluate at the end of step 3 — after every persisted scope layer and before `RequestOverride` — and only for keys whose descriptor allows the layer; `RequestOverride` is always step 4.

The safety floor is logically highest authority even though it validates last. A source chain distinguishes `selected`, `merged`, `shadowed`, `clamped`, `invalid`, `pending`, and `ignored_not_applicable`. Every discarded value has a stable reason.

Environment variables become typed `EnvironmentObservation` layers only for bootstrap and automation compatibility. They are visible with process/host provenance, cannot contain secrets in returned views, and cannot be the sole supported control for user behavior. Persistent UI/CLI edits create an explicit writable override; they do not rewrite the parent process environment.

## 7. Effective values, provenance, and impact

```rust
pub struct EffectiveConfigValueV1 {
    pub key: ConfigKey,
    pub value: CatalogValue,
    pub source: ConfigSourceRefV1,
    pub source_chain: Vec<ConfigSourceStepV1>,
    pub registry_version: ConfigRegistryVersion,
    pub activation_id: ConfigActivationId,
    pub effective_snapshot_id: EffectiveConfigSnapshotId,
    pub validation: ConfigValidationStateV1,
    pub sensitivity: ConfigSensitivityV1,
    pub changeability: ConfigChangeabilityV1,
    pub impacts: Vec<ConfigImpactV1>,
    pub consumers: Vec<ConfigConsumerStateV1>,
}

pub struct EffectiveConfigSnapshotV1 {
    pub snapshot_id: EffectiveConfigSnapshotId,
    pub digest: EffectiveConfigDigest,
    pub registry_digest: ConfigRegistryDigest,
    pub activation_id: ConfigActivationId,
    pub target_resolution: ScopeResolutionV2,
    pub values: Vec<EffectiveConfigValueV1>,
    pub generated_at: UtcMicros,
    pub coverage: CoverageReportV1,
}
```

`coverage` is the canonical `CoverageReportV1` defined in plan 01's domain contracts (searched/skipped/unavailable/stale/truncated/redacted shard lists, freshness watermarks, and the unknown-coverage flag); this plan consumes that shared type unchanged rather than forking a config-local variant. `EffectiveConfigDigest` is computed over the canonical sorted encoding of `registry_digest`, `activation_id`, the target-resolution identity, and every `(key, value, selected source)` tuple; `snapshot_id` and `generated_at` are excluded from the digest, so identical effective states produce identical digests regardless of when they are materialized.

Every value view answers:

- configured value and canonical unit;
- selected source, source owner, layer revision, author/actor class, and time;
- complete precedence chain and why other candidates did or did not win;
- default and effective safety constraint;
- writable target layers and authorization state;
- validation and deprecation state;
- desired versus activated versus acknowledged-effective value;
- affected consumers and their acknowledged generation;
- required restart/reopen/rescan/reproject/reindex/migration operation;
- pending operation IDs, progress, failure, blocked dependencies, and safe remediation capability;
- retrieval anchors to the audit revision, operation receipt, and relevant status evidence.

Policy decisions, query plans, hook evaluations, sanitization receipts, projection manifests, search index versions, replay records, exports, and automation runs pin `EffectiveConfigDigest`. Reproduction never substitutes “current config” for a missing historical snapshot.

### 7.1 Impact rules

Impact is computed by the application from the old and proposed typed snapshots before save and returned inline with validation. It is not a second implementation in the dashboard.

| Impact | Save behavior | Effective-state behavior |
|---|---|---|
| Hot reload | save and publish generation | consumer acknowledges asynchronously; old generation remains visible until ack |
| Next request | save and publish | new requests pin new digest; in-flight requests keep old digest |
| New agent session | save and publish | existing session stays pinned and status says restart/new session required |
| Host/daemon/dashboard restart | save desired generation | component reports pending until restart handshake acknowledges it |
| Store reopen | save desired generation | new operation waits for lease-safe reopen receipt |
| Privacy rescan | stricter ingress behavior activates immediately | old descendants remain partial/quarantined until scan and rebuild receipts close |
| Reproject/reindex | source-of-truth config publishes | old immutable generation remains served only when compatible and explicitly labeled stale; unsafe generation is blocked |
| Storage migration/data retirement | validate config, then require separate system operation | no destructive effect occurs on save; exact confirmation is confined to that operation |
| Unsupported while running | reject or persist pending according to descriptor | never claim effective; provide exact upgrade/restart guidance |

No general “restart everything” guidance is permitted. Each impact identifies exact component instances and the operation that clears it.

## 8. Persistence, versions, atomicity, and concurrency

Add store repositories for:

- immutable `ConfigLayerRevisionV1` records keyed by canonical target and layer;
- immutable normalized key/value entries plus sanitization receipts;
- `ConfigActivationManifestV1` pointing to exact layer revision IDs;
- `EffectiveConfigSnapshotV1` metadata/digests where a durable pin is required;
- `ConfigConsumerAcknowledgementV1` by component instance/generation;
- audit/outbox events, migration receipts, drift observations, and operation links;
- credential-reference metadata only, never secret material.

The three PR 6E persistence records are fully shaped:

```rust
pub struct ConfigLayerRevisionV1 {
    pub revision_id: ConfigRevisionId,
    pub target: ConfigTargetV1,
    pub parent_revision: Option<ConfigRevisionId>,
    pub registry_version: ConfigRegistryVersion,
    pub registry_digest: ConfigRegistryDigest,
    pub entries: Vec<ConfigRevisionEntryV1>,
    pub actor: ActorRefV1,
    pub reason: Option<CatalogSafeText>,
    pub idempotency_key: IdempotencyKeyV1,
    pub state: ConfigRevisionStateV1, // Staged | Activated | Abandoned
    pub created_at: UtcMicros,
}

pub struct ConfigRevisionEntryV1 {
    pub key: ConfigKey,
    pub operation: ConfigEntryOperationV1, // Set | Unset
    pub value: Option<CatalogValue>,       // canonical typed value for Set
    pub sanitization_receipt: Option<SanitizationReceiptId>, // content-bearing values only
}

pub struct ConfigActivationManifestV1 {
    pub activation_id: ConfigActivationId,
    pub previous_activation: Option<ConfigActivationId>,
    pub registry_version: ConfigRegistryVersion,
    pub registry_digest: ConfigRegistryDigest,
    pub members: Vec<ConfigActivationMemberV1>,
    pub actor: ActorRefV1,
    pub idempotency_key: IdempotencyKeyV1,
    pub published_at: UtcMicros,
}

pub struct ConfigActivationMemberV1 {
    pub target: ConfigTargetV1,
    pub revision_id: ConfigRevisionId,
    pub revision_digest: ManifestDigest,
}

pub struct ConfigConsumerAcknowledgementV1 {
    pub consumer: ConfigConsumerId,
    pub instance: ConsumerInstanceId, // opaque per-process component instance
    pub activation_id: ConfigActivationId,
    pub effective_digest: EffectiveConfigDigest,
    pub runtime: ConfigConsumerRuntimeV1, // component version + safe process identity class
    pub state: ConfigConsumerRuntimeStateV1, // Applied | PendingRestart | PendingOperation | Failed
    pub acknowledged_at: UtcMicros,
}
```

Storage contracts:

- `ConfigLayerRevisionV1`: primary key `revision_id`; uniqueness on `(target, idempotency_key)` and a single legal `Staged -> Activated` or `Staged -> Abandoned` transition per row; indexes on `(target, created_at)` and `state`. Rows referenced by any activation manifest, receipt, export, or replay pin are retained permanently; unreferenced `Staged` rows are garbage-collected after the abandonment window (Section 8.2 step 7). Entry values are bounded by descriptor maximum sizes and one revision stays <=1 MiB canonical encoding. Owning shard follows the target per Section 6: profile shard for Profile/Provider/Host layers, project shard for Project/Repository/Worktree layers.
- `ConfigActivationManifestV1`: primary key `activation_id`; uniqueness of one member per target per manifest and one successor per `previous_activation` (a linear append-only chain); index on `published_at`. Manifests are append-only and retained while any snapshot, receipt, or the rollback window references them; member count is bounded by resolved target count and one manifest stays <=1 MiB. Owning shard: the profile shard, matching the profile-owned publication in Section 8.2.
- `ConfigConsumerAcknowledgementV1`: primary key `(consumer, instance, activation_id)`; the latest acknowledgement per `(consumer, instance)` is authoritative; indexes on `activation_id` and `(consumer, acknowledged_at)`. Retention keeps the current acknowledgement per instance plus history bounded to the activation rollback window; rows are <=4 KiB and contain no values, paths, or consumer error text. Owning shard: the profile shard.

### 8.1 Revision semantics

- Every patch includes expected layer revision, registry version, target resolution ID, idempotency key, and actor/access context.
- Validation resolves the full proposed snapshot, not just the changed keys.
- Unknown, removed, wrong-type, out-of-range, floor-weakening, ambiguous-target, stale-resolution, and incompatible-consumer changes fail before persistence.
- A successful single-owner patch appends one immutable revision and audit record transactionally.
- Retrying the same idempotency key and canonical patch returns the stored receipt. Reusing it for different bytes returns `idempotency_conflict`.
- Competing expected revisions return a typed conflict with safe current revision, changed key IDs, and a fresh diff; no last-write-wins overwrite.
- History contains actor class, target, key IDs, before/after safe values, reason, registry version, impacts, activation, and anchors. Secret refs expose only reference identity/status changes.

### 8.2 Atomic activation across targets

A batch import or policy change can affect multiple owning shards. Implement a durable workflow:

1. resolve every target at one registry/catalog watermark;
2. validate the combined effective snapshots and safety constraints;
3. append staged immutable revisions to each owning shard with expected versions;
4. verify all staged revision digests and availability;
5. publish one profile-owned activation manifest that references every staged revision;
6. emit one activation outbox event and consumer notifications;
7. leave unactivated staged rows eligible for safe garbage collection if any pre-publication step fails.

Resolvers ignore staged revisions until activation publication, so readers observe either the previous manifest or the complete new manifest. This is atomic visibility, not a distributed database transaction. If an activated shard later becomes unavailable, coverage is `Partial/Unavailable`; the resolver never silently falls back to an older layer.

### 8.3 Desired, activated, effective, and observed

- **Desired:** latest valid saved revision requested by the authorized actor.
- **Activated:** revision included in the current activation manifest.
- **Effective:** exact generation acknowledged by the consuming component.
- **Observed:** externally inspected behavior/file/process state, which can disagree with effective claims.

Status and UI never collapse these states. Drift is the typed difference between activated/effective/observed state, with owner and remediation.

## 9. Application use cases

Implement in `crates/tracedecay-application/src/configuration/`:

```text
configuration/
├── catalog.rs
├── resolve.rs
├── explain.rs
├── validate.rs
├── impact.rs
├── patch.rs
├── batch.rs
├── history.rs
├── import_export.rs
├── credentials.rs
├── consumers.rs
├── drift.rs
├── status.rs
└── migration.rs
```

Read use cases:

| Use case | Result |
|---|---|
| `config.catalog.get/search` | registry/module/key metadata, legal layers, constraints, docs, deprecations, consumers, impacts |
| `config.targets.resolve` | canonical config targets from unchanged `ScopeSelectorV2` plus complete resolution coverage |
| `config.effective.get/list` | typed effective snapshot/value views with source chain and consumer state |
| `config.explain` | why a value won, lost, was clamped, is pending, or is unavailable |
| `config.layers.get/list` | authorized non-secret immutable layer revisions and activation membership |
| `config.diff` | key/source/impact differences between revisions, targets, or effective snapshots |
| `config.history.list/get` | append-only revision and activation history with safe audit anchors |
| `config.validate` | side-effect-free type/cross-field/floor/compatibility validation and inline impact |
| `config.status` | registry, desired/activated/effective/observed, consumer ack, pending work, drift, migration health |
| `config.export` | classified, sanitized, non-secret declarative bundle with schema/target identities |

Commands:

| Use case | Semantics |
|---|---|
| `config.patch` | validate and atomically append one target-layer revision, then publish activation |
| `config.unset` | append a revision removing selected layer entries so inherited values become explicit |
| `config.batch.commit` | stage validated multi-target revisions and atomically publish one activation manifest |
| `config.import.commit` | validate a versioned non-secret bundle and invoke batch commit; conflicts are explicit |
| `config.history.restore_values` | copy selected historical non-secret values into a new forward revision under current validation |
| `config.credential.bind/unbind` | attach or remove an opaque keyring reference; secret entry happens through protected host integration |
| `config.consumer.acknowledge` | component acknowledges exact activation/effective digest and runtime state |
| `config.drift.reconcile` | execute the exact non-destructive registered reconciliation capability |

Ordinary updates do not use preview/apply. `config.validate` is optional linting and is also executed inside every commit. Destructive consequences remain separate cataloged system commands such as storage migration, protected data retirement, or quarantine release; those commands can require explicit confirmation and audit under plans 09 and 18.

Application handlers return `CatalogSafeText`/`LogSafeText`, typed catalog values, opaque IDs, or explicit redacted/denied/unknown states. They never render arbitrary config files, environment expansions, keyring content, or raw consumer errors.

## 10. Generated transport surface

Plan 08's capability catalog declares each configuration use case once. Plans 10 and 17 generate identical schemas and clients.

### 10.1 HTTP and SSE

Minimum HTTP surface:

```text
GET  /api/v2/config/catalog
GET  /api/v2/config/catalog/{key}
POST /api/v2/config/catalog:search
POST /api/v2/config/targets:resolve
POST /api/v2/config/effective:query
POST /api/v2/config/explain
POST /api/v2/config/diff
POST /api/v2/config/history:query
POST /api/v2/config/validate
POST /api/v2/config/status
POST /api/v2/config/exports
POST /api/v2/commands/config/patch
POST /api/v2/commands/config/unset
POST /api/v2/commands/config/batch:commit
POST /api/v2/commands/config/imports:commit
POST /api/v2/commands/config/history:restore-values
POST /api/v2/commands/config/credentials/{bind,unbind}
POST /api/v2/commands/config/drift:reconcile
```

All requests carry explicit request context, `ScopeSelectorV2` where resolution is needed, target `DeclaredScope` for mutations, expected revision, registry version, and idempotency. Errors use plan 10's `ApiProblem`; no config parser string becomes a public error.

SSE event types:

- `config.registry_changed`;
- `config.activation_published`;
- `config.target_revision_changed`;
- `config.consumer_acknowledged`;
- `config.effective_changed`;
- `config.impact_progress`;
- `config.drift_changed`;
- `config.credential_reference_status_changed`;
- `config.resync_required`.

Events include safe IDs, key IDs when authorized, versions, impact/status, and snapshot cursors. They omit credential material, environment values, protected paths, arbitrary consumer messages, and large snapshots. Slow consumers receive `resync_required` and reload a frozen snapshot; frames are never silently dropped.

### 10.2 MCP and SDKs

MCP tools are generated from the same capability entries and use the same request/result schemas. Human-facing MCP output defaults to concise markdown with effective value, source, target, impact, pending state, and exact next command; `format=json` returns the stable agent contract. Rust, TypeScript, and Python SDKs expose the same typed use cases, pagination, conflicts, SSE events, and credential-reference states.

No SDK constructor takes a plaintext secret as a configuration field. Protected credential installation uses a host/keyring integration that returns `CredentialRefId`, after which configuration binds only that reference.

## 11. CLI: navigable for humans, deterministic for agents

`tracedecay config` with no subcommand opens an interactive terminal tree when attached to a TTY. The tree and noninteractive commands are generated from the registry.

```text
tracedecay config
tracedecay config tree [--scope <selector>] [--target <id>]
tracedecay config search <terms> [--scope <selector>] [--json]
tracedecay config list [--module <id>] [--changed-only] [--json]
tracedecay config get <key> [--target <id>] [--effective|--layer <kind>] [--json]
tracedecay config explain <key> [--target <id>] [--json]
tracedecay config set <key> <typed-value> --target <id> --expected-version <n> [--json]
tracedecay config unset <key> --target <id> --expected-version <n> [--json]
tracedecay config edit --target <id>
tracedecay config validate [<file>] [--target <id>] [--json]
tracedecay config diff <left> <right> [--json]
tracedecay config history [<key>] [--target <id>] [--json]
tracedecay config status [--scope <selector>] [--json]
tracedecay config watch [--scope <selector>] [--jsonl]
tracedecay config export --scope <selector> --format json|yaml
tracedecay config import <file> --expected-manifest <digest> [--json]
tracedecay config credential bind <key> --target <id> --keyring-ref <ref>
tracedecay config credential unbind <key> --target <id>
```

Interactive tree anatomy:

```text
All / Profile
├── Capture
│   ├── Providers
│   ├── Hosts and hooks
│   └── Session and tool events
├── Privacy and redaction
├── Search, retrieval, and graphs
├── Hints and coordination
├── Memory and autonomous curation
├── Automations and skills
├── Git, code, and delivery
├── Storage, indexing, and retention
├── API, MCP, CLI, and dashboard
├── Costs and observability
└── Extensions and updates
```

The detail pane shows typed editor, effective value, target/layer, default, source chain, floor/constraints, consumers, desired/effective state, impact, history, drift, docs, and exact noninteractive command. Search covers keys, aliases, labels, descriptions, modules, consumer IDs, and impact terms. Keyboard navigation, screen-reader labels, narrow-terminal layout, and no-color mode are required.

Agent rules:

- `--json` never emits prose around the envelope and has stable error codes.
- `watch --jsonl` emits one bounded event per line with resume cursor.
- Values have canonical units and JSON types; duration/size text is accepted only at CLI parsing and returned canonically.
- Ambiguous locators return candidates and a retry token; CLI never chooses the first project/worktree.
- Omitted target is an error for mutation. Reads default only when the command explicitly documents active-profile `AllAuthorized`.
- `config edit` writes a protected temporary draft, validates before commit, scans content, and deletes the draft. It does not invoke an external editor with credential values.
- Shell completion derives keys, modules, enums, and legal layers from the registry and never completes secret values.

## 12. Brain Settings workspace

Expand plan 11's `/settings` into the complete configuration workbench. It uses the same command/status bar, scope tree, time-independent target resolution, inspector, and status semantics as the Brain.

Desktop anatomy:

```text
┌ scope/target · search · changed/drift/pending filters · registry/status ┐
├ module tree ┬ setting list/form ┬ effective source + impact inspector   ┤
│ counts/state│ grouped controls  │ precedence/history/consumers/status  │
└ activation · desired/effective · pending operations · audit anchors ───┘
```

Required behaviors:

- Search all registry descriptors without loading every setting value.
- Navigate All → profile → project → repository → checkout/worktree → provider → host with canonical disambiguated labels and coverage.
- Filter by modified, shadowed, clamped, invalid, pending restart, pending rescan/reindex, drifted, deprecated, unavailable credential, and safety-critical.
- Render generated controls for booleans, enums, numbers, durations, byte sizes, sets, maps, structured schemas, scope/entity references, and credential references.
- Show default, desired, activated, effective, observed, and source chain together; never show only a toggle.
- Display inline validation and exact operational impact before Save. Save invokes one direct CAS patch, not preview/apply.
- On conflict, show changed key IDs and safe base/current/user values, then let the user rebase the draft explicitly; never overwrite.
- Show pending consumers and progress from SSE, with exact restart/new-session/rescan/reproject/reindex/migration action.
- Provide history/diff and “use these historical values” as a new forward revision; do not rewrite or silently reactivate an old generation.
- Keep unsaved drafts local, encrypted/profile-bound when content-bearing, versioned against the registry, and purged on lock/sign-out/schema incompatibility.
- Never place setting values, paths, provider metadata, or credential references in URLs. URLs may contain only opaque target/key IDs and nonsensitive filter state.
- Provide copyable CLI, MCP, HTTP, and SDK examples generated from the exact current target and key, with secret fields represented only as opaque reference placeholders.
- Meet keyboard, mobile, table/outline, high-contrast, reduced-motion, error/partial/offline, and Playwright visual gates from plan 11.

There is no second “advanced config file” route. Raw import/export is an action within Settings and uses the same schema, validation, authorization, and audit.

## 13. Complete configuration inventory

Phase 0 generates an inventory from source and blocks cutover until every public control maps to a descriptor. At minimum the registry covers:

| Module | Representative controls |
|---|---|
| Profile and identity | active profile behavior, privacy domain, labels, locale/time display, retention class defaults |
| Capture and providers | enabled sources, provider adapters, transcript/tool/reasoning capture classes, framing limits, polling/watch behavior |
| Hosts and hooks | installed host integration, hook enablement, latency budgets, fail-closed mode, hint delivery budgets, session pinning |
| Privacy/redaction | detector sets, thresholds, structured field policies, actions, decode/archive limits, custom manifests, retention/quarantine roles, scan schedules |
| Sessions and activity | attribution modes, message views, compaction/summary policy, workflow/goal capture, evidence retention |
| Code/Git/delivery | index modes, graph generation triggers, refs/worktrees, ignore policy, delivery refresh, diagnostics capture |
| Query/search | lexical/fuzzy/vector/rerank profiles, exact-match floor, candidate budgets, graph expansion, diversity, temporal current/as-of/evolution/forensic policy, authority/supersession/conflict rules, copy/summary-horizon policy, fusion/calibration, time/coverage/no-answer defaults, corpus/promotion gates; signed representation artifact IDs/sources, explicit automatic-download authorization, offline-only mode, allowed residency/device/runtime, 4 GiB default disk and 2 GiB default resident-memory budgets, cold-load concurrency, idle unload, pin/eviction/revocation/rebuild/fallback policy per plan 05 §11.2A |
| Hints/coordination/scout | classifier bundles, routing, scout off/shadow/deterministic/model-assisted mode, discovered model capability/credential reference, read/egress grants, coalescing/concurrency/tool/model/cost budgets, evidence/silence/dedupe/cooldown/expiry/delivery thresholds, proximity/task-materiality, terminal horizons |
| Tasks/plans/executors | task graph/decomposition limits, legal work/gate/acceptance kinds, scheduler pause/concurrency/fairness/aging/batches, lease/heartbeat/start/cancel timeouts, executor adapters/hosts/capacity/workspace modes, provider/model/reasoning effort/routes/fallback, tool/effect grants, privacy/egress, worktree/branch policy, budgets/schedules/retries/circuit breakers, context-packet limits/expiry/materiality, saved task views/notifications |
| Memory/knowledge | retrieval/trust/conflict/retention policies, autonomous curation cadence and quality constraints |
| Automations/skills | scheduler, run budgets, autonomous curator/reflector/skill-writer policies, installation authority, health pauses |
| Storage/projectors | data locations by allowed location class, WAL/lease budgets, blob/backup retention, projection/index generations, compaction |
| API/MCP/CLI/dashboard | loopback bind, session lifetime, request/page/budget caps, SSE caps, renderer preferences, dashboard preferences |
| Costs/observability | pricing catalog version, sampling, safe metrics, log levels, tracing budgets, accounting horizons |
| Updates/migrations | update channel, daemon drain policy, compatibility windows, import schedules, retirement holds |
| Extensions | enabled manifests, sandbox/resource budgets, privacy/egress permissions, version pins |

Hard-coded correctness constants and safety maxima are not mislabeled as user settings. They still appear in capability/status documentation when relevant, but are not writable. Conversely, a behavior marketed or documented as configurable cannot remain an unregistered constant.

### 13.1 Canonical task/executor liveness descriptors

Plan 24 §8.7 owns the liveness/sentinel policy semantics; this registry is the only configuration publication/resolution authority. The generated descriptors must match these baseline values and constraints exactly:

| Key | Type/default | Validation and impact |
|---|---|---|
| `scheduler.attempt_liveness.lease_ttl` | duration / `5m` | `30s..30m`; hot-reload for new extensions, active leases retain their issued bound until the next heartbeat revalidation. |
| `scheduler.attempt_liveness.heartbeat_expected` | duration / `60s` | `10s..10m`; visibility/diagnostic threshold only, never death authority. |
| `scheduler.attempt_liveness.heartbeat_stale_backstop` | duration / `60m` | must be `>= 3 × heartbeat_expected` and `<= default_max_runtime`; active attempts re-evaluate and may enter cancel/reconcile, so activation requires an impact operation receipt. |
| `scheduler.attempt_liveness.probe_timeout` | duration / `2s` | `100ms..10s`; applies to bounded adapter probes outside writer transactions. |
| `scheduler.attempt_liveness.alive_extension` | duration / `2m` | `10s..lease_ttl`; preserves the same attempt/epoch and cannot exceed maximum runtime. |
| `scheduler.attempt_liveness.default_max_runtime` | duration / `4h` | `5m..24h`; attempt override may only narrow or use an explicitly authorized higher value within the floor/ceiling. |
| `scheduler.attempt_liveness.cancel_grace` | duration / `30s` | `1s..10m`; adapter manifest may request a value within this policy ceiling. |
| `scheduler.rate_limit.default_backoff` | duration / `2m` | `1s..1h`; used only without valid provider `Retry-After`. |
| `scheduler.rate_limit.max_backoff` | duration / `1h` | `>= default_backoff`, `<=24h`; bounded by attempt deadline/budget. |
| `scheduler.repair_poll_interval` | duration / `30s` | `5s..5m`; repair-only journal/checkpoint fallback, never normal board/task scanning. |

All ten descriptors are profile defaults with optional initiative/executor/provider narrowing only where the descriptor declares that scope. Deny/safety floors win. Settings shows desired/activated/effective/observed values, source, generation, affected active-attempt count, and whether activation is hot, next-heartbeat, or workflow-mediated. Tests compare the generated registry values to plan-24 policy fixtures so a renamed key, unit drift, or conflicting default blocks both PRs.

## 14. Privacy, redactor, detector, and credential controls

The entire plan 18 policy is present in Settings and CLI, not hidden behind files or provider metadata.

### 14.1 Visible privacy controls

- effective `PrivacyPolicyV1` version/digest and non-disableable floor version/digest;
- enabled built-in detector set and versions;
- optional detector plugins/custom manifests, sandbox state, budgets, and health;
- confidence/action thresholds by typed secret class;
- structured provider field maps and unsupported/unknown coverage;
- drop, sanitized-marker, or protected-short-lived-quarantine action where legally configurable;
- normal/sensitive/reasoning/secret retention policies;
- bounded decode/archive/record/field sizes and timeout/fail-closed behavior;
- allow decisions by rule/field structure, expiry, owner, and synthetic regression coverage, never candidate value;
- authorized quarantine roles and hold/release policy;
- scheduled/full/resumable scan policy and last verified coverage;
- required rescan/reproject/reindex/backup/restore impact after changes.

### 14.2 Non-disableable safety floor

The floor enforces:

- built-in runtime detector always active on every ingress, including hooks;
- parse/field-scan boundaries and fail-closed behavior;
- no plaintext secret in search, prompts, indexes, embeddings, logs, analytics, errors, audit, exports, fixtures, or ordinary UI/API output;
- no provider, source record, project, worktree, host, environment, request, or plugin option that disables scanning;
- no threshold below the floor's minimum protection;
- no broad exclusion that skips structural scanning;
- no unbounded decoder/archive/plugin execution;
- no protected quarantine without key service, authorization, retention, and audit;
- only plan 18 eligible wrappers at content sinks.

Settings renders floor-controlled fields as a source-chain constraint, not as a misleading disabled toggle. A rejected weakening explains the invariant and legal stronger values. CLI/API returns `config_floor_violation` with key IDs and safe constraint metadata.

### 14.3 Privacy change activation

- A stricter policy takes effect for new ingress immediately through the hot runtime floor.
- Existing content receives `legacy_or_prior_policy` coverage until a rescan proves it under the new digest.
- Search/prompt/export hydration blocks records whose required receipt does not satisfy the active floor.
- Rescan, descendant invalidation, quarantine, reproject, reindex, backup verification, and restore eligibility run as explicit observable operations.
- A weaker but still floor-compliant false-positive adjustment applies only after validation and cannot reconstruct deleted plaintext or automatically rehydrate V1 sources.
- Privacy configuration history contains rule IDs, versions, classes, actions, and counts only; never candidate bytes or equality-leaking cross-domain fingerprints.

### 14.4 Credentials

Use a narrow protected key service/keyring port:

```rust
pub struct CredentialReferenceViewV1 {
    pub reference_id: CredentialRefId,
    pub provider_kind: CatalogValue,
    pub availability: CredentialAvailabilityV1,
    pub owner: ConfigTargetV1,
    pub created_at: UtcMicros,
    pub rotated_at: Option<UtcMicros>,
    pub expires_at: Option<UtcMicros>,
    pub consumers: Vec<ConfigConsumerId>,
}
```

The reference has no `Display` of secret material and does not expose secret-derived fingerprint, length, prefix, account URL, username, query, or scope beyond safe declared metadata. Protected entry uses host-native prompt/stdin/keyring APIs that suppress echo and logs; configuration receives only the resulting ID. Import/export preserves an unresolved reference alias and reports binding required on the destination host.

## 15. Autonomous curation and self-improvement

The configuration system must encode the user's explicit product rule: curation is autonomous, not proposal-driven.

Applies to:

- memory/fact curation, deduplication, contradiction resolution, trust updates, and retirement;
- session reflection and summary/memory extraction;
- skill writer generation, validation, evolution, installation, supersession, and retirement within granted authority;
- schedule selection and self-improvement cycles;
- retrieval/hint outcome learning and policy calibration where enabled;
- safe maintenance curation such as stale derived-state cleanup.

Settings exposes autonomy policy, not individual candidates:

- enabled workflows and authoritative scope;
- schedule/cadence and concurrency;
- source eligibility, evidence/quality/trust thresholds, and retention horizons;
- compute/token/time/cost budgets;
- model/provider/credential reference;
- sandbox/capability grants and repository-write boundaries;
- retry/backoff, circuit breaker, health pause, and incident behavior;
- evaluation corpus/version and promotion quality gates;
- notification/summary verbosity;
- audit retention and outcome measurement.

There are no “pending curation proposals,” Approve, Reject, Apply, or Roll Back controls. The autonomous workflow evaluates, validates, commits, supersedes, or retires under its active policy and writes a complete decision/effect receipt. Brain/Evolution surfaces show what happened, evidence class, policy/config digest, impact, quality, and failure state for investigation—not authorization after the fact.

Changing autonomy configuration applies to future workflow decisions at the next safe boundary. In-flight runs remain pinned to their starting digest or stop at a declared cancellation boundary; they do not mix generations. Re-evaluating historical material is a new autonomous run with a new manifest, not manual per-item replay/apply.

Safety floors remain mandatory: secret-like/quarantined content cannot be curated into searchable facts, fixtures, prompts, or skills; extension and repository writes remain within explicit authority; system-destructive effects can require a separate confirmation. These constraints do not create a curation approval queue.

## 16. Drift, status, doctor, and reconciliation

Add a `configuration` component family to `SystemStatusSnapshot`:

```text
registry: configured/loaded version and digest
activation: desired/current activation and timestamp
targets: complete/partial/stale/unavailable/ambiguous coverage
consumers: expected/acknowledged generations and lag
impacts: pending/running/failed operations
drift: activated/effective/observed mismatch by owner
migration: legacy inputs, imported, blocked, retired
privacy: floor/policy/detector coverage and last verified scan
credentials: available/missing/expired/foreign without values
```

Drift detectors use registered safe observations:

- process environment differs from recorded bootstrap observation;
- host/provider config was modified outside TraceDecay;
- generated hook/skill/service files do not match the activation manifest;
- daemon/dashboard/agent session runs an older generation;
- store/index/projection manifest pins another config digest;
- registry/schema version differs across client/server;
- a credential reference is missing, expired, locked, or foreign-owned;
- a legacy config reader remains active after its cutoff.

Doctor reports source, owner, first/last observed time, severity, safe evidence, affected components, and exact registered remediation. It does not suggest blind file deletion or print raw configuration. Reconciliation is allowed only for TraceDecay-owned non-destructive state; foreign-owned state is informational unless the user explicitly grants authority.

`tracedecay config status` and `/settings` consume exactly the same status model. A green Settings toggle cannot contradict doctor.

## 17. Import, export, declarative configuration, and migration

### 17.1 Export

Exports contain:

- bundle schema/registry version and digest;
- canonical target identities plus portable safe aliases;
- explicit `DeclaredScope` and project-set version where applicable;
- selected non-secret layer values in canonical units;
- credential reference aliases marked unresolved/required, never host IDs when nonportable;
- source revision and activation metadata when requested;
- deprecation/migration requirements;
- sanitizer/export receipt and privacy manifest.

Exports exclude built-in defaults unless requested, safety-floor internals that are not public controls, environment values, runtime observations, secret material, protected paths, consumer error details, and ephemeral request overrides.

### 17.2 Import

- Parse into typed `Unclassified` fields under size/depth/count budgets and sanitize before validation.
- Resolve targets through `ScopeSelectorV2`; ambiguity blocks import with candidates.
- Require explicit mapping for missing projects/repositories/worktrees/providers/hosts.
- Validate registry compatibility, types, constraints, floor, credentials, consumers, expected revisions, and impact.
- Commit through the staged revision/activation workflow. A failure before activation changes no effective values.
- Unknown keys fail with migration guidance; they are not silently ignored.
- A config-only import does not execute a destructive migration. Required system operations remain pending and separately authorized.

### 17.3 V1 migration

Inventory and import:

- root/profile/project config files and legacy database rows;
- CLI-persisted values and environment-variable behavior;
- provider/hook installation metadata;
- daemon/service and dashboard settings;
- memory, automation, scheduler, curator, reflector, and skill-writer config;
- search/index/embedding/ranking settings;
- privacy/redaction/retention/quarantine settings;
- data directory, backup, update, and migration flags;
- plugin/extension manifests and host-owned foreign state.

Named V1 anchors (a human audit anchor in the plan 08 §5 style; plan 12's generated root inventory is authoritative and the Phase-0 inventory generator is validated against these names):

- project `.tracedecay/config.json` and profile `~/.tracedecay/config.json` (`CONFIG_FILENAME` under `TRACEDECAY_DIR`, relocated by `TRACEDECAY_DATA_DIR`);
- project `.tracedecay/enrollment.json` and legacy settings rows in project/profile `.tracedecay/tracedecay.db` (dashboard project/user settings, automation config, branch autotrack state);
- environment reads including `TRACEDECAY_DATA_DIR`, `TRACEDECAY_GLOBAL_DB`, `TRACEDECAY_SYNC_*`, `TRACEDECAY_DIAGNOSTICS_PREWARM`, `TRACEDECAY_OFFLINE`, `TRACEDECAY_TOOLS`, and `TRACEDECAY_MEMORY_INJECTION`; internal worker/test variables are classified as non-config runtime observations, not user settings;
- provider/host hook and MCP installation metadata: Claude `settings.json` hook/MCP entries, Codex `config.toml` entries, Cursor hook configuration, and Kiro hook entries as foreign-observed state.

For each legacy input record source, owner, parser version, value classification, mapped key, target resolution, selected precedence, semantic difference, and import receipt. Secrets are converted to keyring references or quarantined; they never enter V2 layer history. Ambiguous ownership is `ImportUnresolved` and cannot become effective.

Run V1 and V2 resolution against a sanitized fixture corpus, compare effective values and operational behavior, explicitly accept intentional differences, then cut over one module at a time. Remove old readers, env-only code paths, direct dashboard mutations, provider-local defaults, and file watchers after parity. Stale clients receive typed registry/version guidance, not a live V1 fallback.

## 18. Replay and evaluation

Add a read-only Configuration Lab under `/playgrounds/configuration` and generated CLI/API equivalents.

Inputs:

- historical/current registry version;
- historical/current activation;
- exact `ScopeSelectorV2` and target resolution;
- host/provider/project/repository/worktree context;
- selected key/module or complete bounded snapshot;
- historical runtime/consumer acknowledgement manifest.

Outputs:

- complete effective values and source chains;
- old/current resolution diff;
- validation/floor/compatibility decisions;
- impact and consumer acknowledgement difference;
- resulting pinned policy/query/hook/privacy bundle IDs;
- missing historical input, substitutions, coverage, and fidelity label.

The lab is useful for questions such as:

- Why did this agent receive different hints than another worktree?
- Which search/retrieval configuration produced this result?
- Would the current detector policy classify this synthetic canary differently?
- Which project/provider/host layer changed capture behavior?
- Did a curation run use the expected autonomous policy and budgets?
- Does a configuration change improve replay/evaluation metrics without violating floors?

Privacy detector replay accepts only synthetic canaries or retained sanitizer-eligible fixture references. Curation replay shows policy results and historical autonomous effects but has no approve/apply path. All replay outputs pin config, policy, catalog, index, model, scope, time, and registry versions.

Evaluation suites measure:

- resolution correctness across every layer combination and scope;
- transport parity and round trips;
- exact source-chain explanations;
- stale/partial/ambiguous/foreign/locked behavior;
- concurrent patch conflict and idempotent retry;
- activation atomic visibility under crash/fault injection;
- consumer convergence and SSE resync;
- privacy-floor mutation resistance;
- no secret/reference leakage in every sink;
- V1 differential behavior and accepted migration deltas;
- configuration-induced hint/search/curation/privacy outcome changes on real local sanitized corpora, reported only in aggregate/redacted form.

## 19. Extension configuration

Extension manifests can contribute namespaced configuration descriptors only through the owner SPI in plan 19.

- Key namespace is bound to signed extension ID/version.
- Descriptor schemas, legal layers, merge strategies, impacts, and UI metadata pass registry validation.
- Extensions cannot shadow core keys, alter precedence globally, register a weaker privacy constraint, request plaintext secret serialization, or invent a new transport.
- Credential fields are opaque references with declared capability requirements.
- Disabling/removing an extension leaves immutable history and a typed orphaned-config state; values cannot be reassigned to another extension ID.
- Sandbox/resource/egress/privacy permissions are core-owned settings whose safety floor the extension cannot edit.
- Upgrade migrations are deterministic, versioned, reversible only as a new forward revision, and tested against retained sanitized fixtures.
- Remote extensions remain outside first-default support and cannot become a reason to weaken local loopback/privacy constraints.

## 20. Security and privacy invariants

- All content-bearing labels, descriptions, notes, imported strings, and custom detector metadata cross plan 18's `Unclassified -> Classified -> Sanitized -> sink-eligible` path.
- Secret values and secret-derived identifiers never enter SQLite config rows, history, audit, SSE, logs, metrics, error text, response handles, browser state, URLs, exports, fixtures, or search indexes.
- Authorization precedes target expansion, layer reads, history, export, mutation, credential status, and drift observation.
- Same-profile access does not imply access to protected quarantine or foreign host state.
- Configuration mutations require loopback-authenticated/current-session access in the first V2 default, CSRF protection for browser commands, idempotency, expected revision, and audit.
- Request overrides cannot alter privacy floor, authorization, storage ownership, audit, retention holds, extension capability grants, or destructive-operation confirmation.
- Safe floor manifests are build/release integrity inputs and are signed/digested in runtime handshakes.
- A stale client cannot submit an unknown old enum/default and have the server reinterpret it. Registry mismatch returns a typed refresh/new-session/update error.
- Imports, exports, generated docs/examples, saved drafts, migration fixtures, and staged revisions receive secret scans and bounded archive handling.
- Config logs use key IDs, layer IDs, versions, result codes, counts, and durations only. Values are excluded by default even when nominally non-secret.

## 21. Testing strategy

### 21.1 Domain and registry

- ID grammar, canonical units, serialization, unknown enum, schema compatibility, and migration golden tests.
- Descriptor completeness, duplicate keys, legal layer/precedence exhaustiveness, writable-layer requirement, consumer existence, impact mapping, docs/UI metadata, and privacy classification.
- Property tests generate every legal layer combination and prove deterministic resolution independent of input order.
- Compile-fail tests reject plaintext credential/string sinks and alternate config/scope types.

### 21.2 Resolver and application

- Built-in/profile/project/repository/worktree/provider/host/request precedence matrices.
- Safety floor clamps/rejections and cross-field constraints.
- Exact source-chain reason and canonical effective digest.
- `DeclaredScope` ownership and `ScopeSelectorV2` ambiguity/stale/partial tests across multiple repos/worktrees/projects.
- CAS conflicts, idempotency, retries, cancellation, crash points, staged garbage, and activation publication linearizability.
- Desired/activated/effective/observed state and consumer acknowledgement.
- Inline impact correctness and separate destructive operation boundary.
- History forward-restore under newer schema/floor.

### 21.3 Transport parity

For every use case, run one fixture through in-process application, CLI JSON, MCP JSON, HTTP, Rust SDK, TypeScript SDK, Python sync/async SDK, and dashboard client. Assert identical:

- key/target identity and scope resolution;
- values/source chains/coverage;
- validation, conflict, and error codes;
- impact and consumer state;
- pagination/order/filter/search;
- audit/retrieval anchors;
- absent sensitive fields.

Generated artifacts must leave a clean tree. An inventory test compares every registry key against CLI completion, MCP/OpenAPI schemas, SDKs, dashboard renderer coverage, and docs.

### 21.4 UI and CLI

- Full keyboard/tree/search/edit/save/conflict/history/diff/import/export/status flows.
- Large registry virtualization and search latency.
- Mobile, narrow terminal, screen reader, high contrast, no color, reduced motion, localization expansion, offline/partial/locked states.
- Restart/rescan/reindex/migration progress and SSE reconnect/resync.
- Secret-reference non-rendering, URL/storage/log/clipboard scans, and synthetic canaries.
- Copy-command round trips between UI and CLI JSON.

### 21.5 Autonomous curation

- No generated capability, route, form, CLI command, or MCP tool exposes item approve/reject/apply/rollback.
- Runs pin one policy/config digest and cross generation only at a safe boundary.
- Policy/budget/schedule changes affect future autonomous runs deterministically.
- Failures pause/retry/circuit-break according to policy and remain observable.
- Secret/quarantine/floor tests prove autonomous curation cannot promote unsafe content.
- Outcome and Evolution views are read/audit surfaces, not hidden authorization paths.

### 21.6 Fault and scale

- Thousands of keys/targets, hundreds of concurrent readers, many simultaneous agent writers, slow consumers, daemon restart, store lock, shard unavailability, registry upgrade, clock skew, and disk-full faults.
- Resolver p95 and allocation budgets on profile All and exact target reads.
- SSE queue/backpressure and snapshot reload bounds.
- Cross-shard batch staging failure at every step; previous activation remains effective.
- Consumer acknowledgement timeout and safe degraded behavior.
- Privacy rescan configuration change while capture/query/projectors remain active.

Representative commands:

```bash
cargo test -p tracedecay-domain config
cargo test -p tracedecay-store config
cargo test -p tracedecay-application configuration
cargo test -p tracedecay-tool-catalog config_registry
cargo test -p tracedecay-api config_conformance
cargo nextest run --workspace --no-fail-fast
pnpm --dir dashboard test -- settings
pnpm --dir dashboard exec playwright test settings configuration-lab
gitleaks git --redact --no-banner
gitleaks dir dashboard packages python docs tests --redact --max-archive-depth 2
```

## 22. Migration and reviewable PR slices

These slices extend the master program without forming a separate architecture:

### PR 4C — Domain configuration contracts and registry schema

- Add IDs, descriptors, layer/precedence/merge/value/impact/history/effective contracts.
- Add config target references tied to `DeclaredScope` and `ScopeSelectorV2` resolution.
- Generate registry/schema golden fixtures and architecture lints.

### PR 6E — Immutable configuration revisions and activation manifests

- Add profile/project repositories, audit/outbox, staged revisions, atomic activation publication, consumer acknowledgements, and fault tests.
- Store credential references only.

### PR 22C — Generated configuration registry and capability inventory

- Collect owning-crate manifests.
- Generate catalog, schemas, docs, CLI/MCP/HTTP/SDK/dashboard metadata.
- Add full legacy/public-setting inventory and drift gates.

### PR 24I — Application resolver, commands, API, CLI, MCP, and SDKs

- Implement resolve/explain/validate/impact/patch/batch/history/import/export/status/drift use cases.
- Ship navigable CLI tree and deterministic JSON/JSONL.
- Add transport parity and configuration SSE.

### PR 25E — Complete Brain Settings workspace

- Replace partial settings/plugins with the generated profile-wide workspace.
- Add target tree, search/forms, provenance, impact, conflicts, history, status, drift, and credential references.
- Keep all old write behavior until module parity passes, then remove old bindings atomically.

### PR 31N — Configuration and autonomy replay lab

- Add historical/current resolution, impact, consumer, hint/search/privacy/autonomy comparisons.
- Enforce read-only and synthetic-only privacy fixtures.
- Prove there is no per-item curation approval capability.

### PR 33C — Legacy configuration import and cutoff

- Execute the configuration slice of plan 12's PR 33 family: plan 12's root inventory generator produces the V1 file/flag/environment source inventory; this PR runs the import itself through the Section 8.2 staged revision/activation workflow and reports receipts into plan 12's cutover checklists.
- Import every V1 config source with provenance, scope, secret conversion, and differential receipts.
- Cut over one module at a time and delete live legacy readers, hidden environment-only controls, direct dashboard mutations, and provider-local default forks.

### PR 37G — Configuration convergence gate

- Require zero unregistered public settings, zero duplicate resolvers, complete transport/UI coverage, all consumers acknowledged, privacy floor active, and no V1 live fallback.
- Publish the final registry/activation/status manifest and deletion receipt.

Each PR updates the master plan/index, architecture ownership table, schema inventory, capability catalog, migration matrix, and relevant crate plan. No slice may land as an isolated settings subsystem.

## 23. Definition of done

- [ ] Every user-controllable non-secret setting is registered, searchable, explainable, and editable through Brain Settings and generated CLI/MCP/HTTP/SDK surfaces.
- [ ] No public behavior is configurable only through an environment variable, hidden file, direct database write, provider metadata, dashboard-only toggle, or code constant.
- [ ] One typed resolver produces identical effective values, source chains, coverage, and errors across all consumers and transports.
- [ ] Every value exposes default, desired, activated, effective, observed, source/precedence, validation, history, drift, consumer, and exact operational impact.
- [ ] Profile/project/repository/worktree/provider/host targets resolve through `ScopeSelectorV2` and persist explicit `DeclaredScope`; no CWD/route/first-match ownership exists.
- [ ] Single-layer updates are CAS/idempotent; multi-target changes have atomic effective activation and exhaustive crash/fault tests.
- [ ] Ordinary edits are validate-and-save, without mandatory preview/apply/rollback ceremony; destructive system effects remain separate explicitly confirmed commands.
- [ ] Configuration history is immutable and historical values can only return as a new revision valid under the current schema and safety floor.
- [ ] Redactor/detector/privacy/retention/quarantine configuration is complete in UI/CLI and the safety floor cannot be disabled or weakened by any layer.
- [ ] Credentials remain opaque protected references; no secret or secret-derived identifier leaks through any config sink.
- [ ] Curation and self-improvement are fully autonomous with policy/schedule/budget configuration and audit, and no per-item approval/apply/reject/rollback surface exists.
- [ ] Every consuming runtime acknowledges the exact activation/effective digest; pending restart/session/rescan/reproject/reindex/migration is visible and actionable.
- [ ] Configuration SSE, status, doctor, and Settings agree under slow clients, restarts, stale clients, split identity, locked stores, and partial shards.
- [ ] Import/export is typed, scoped, versioned, sanitized, non-secret, and atomic at activation; V1 inputs have complete migration/differential receipts.
- [ ] Configuration Lab replays historical/current resolution and policy effects without mutation or unsafe fixture access.
- [ ] Registry generation leaves a clean tree and parity tests cover CLI, MCP, HTTP, SDKs, dashboard, hooks, daemons, automations, and extensions.
- [ ] Legacy live config readers, duplicate defaults, transport-local settings, env-only controls, and fallback paths are deleted after verified cutover.
- [ ] Full workspace, dashboard, fault, accessibility, performance, privacy, and secret-scan gates pass.

# TraceDecay V2 Configuration Control Plane Implementation Plan

**Plan 32 integration:** register every Plan-32 compiler/engine profile and placement, runtime/history/schema/job limits, concurrency/budget/fork/cache/source-root/watcher/retention/taskgraph-candidate/remote setting with the same owner, scope, source, validation, restart/reindex impact, and four-axis state used elsewhere. UI/CLI/API expose typed policy choices, never raw engine flags or hidden SDK/plugin defaults.

> **For agentic workers:** implement this plan in the program order below. Every slice must preserve the contract, privacy, scope, transport-parity, and migration gates before the next slice becomes the default.

**Goal:** Replace TraceDecay's scattered files, flags, environment variables, dashboard toggles, provider metadata, hook defaults, daemon settings, and hidden constants with one typed, versioned configuration control plane. Every user-controllable non-secret setting is discoverable, searchable, explainable, and editable in the Brain Settings workspace and through generated CLI, MCP, HTTP, and SDK bindings.

**Architecture:** Generic configuration identity, value, provenance, impact, and version contracts live in `tracedecay-domain`; each owning subsystem contributes a typed module manifest; build-time generation produces one configuration registry; `tracedecay-application` is the only resolver and mutation owner; profile/project repositories persist immutable layer revisions and activation manifests; root composition supplies process/environment observations and applies runtime changes. All surfaces consume generated application contracts. Safety floors, especially redaction, are constraints over effective values and cannot be disabled or weakened by any lower layer.

The simplification audit treats every independently parsed default, environment alias, provider install mutation, dashboard-only toggle, runtime cache, and subsystem-local validation branch as a migration candidate in one generated inventory. V2 does not centralize configuration by wrapping all of those paths: the typed registry generates schemas/forms/help/bindings, the application resolver is the only merge/validation/impact engine, host manifests generate irreducible file mutations, and cutover deletes the predecessor parser/default/validation path in the same slice.

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
11. Replay evaluators have no production write ports. A generic experiment may persist immutable run artifacts and explicitly granted model/egress cost while resolving historical versus current effective configuration, but it cannot mutate settings or become an approval path for curation.
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
- No mandatory hosted/vendor control plane or cloud dependency in the first V2 default. A Brain may remain standalone-local or use plan 28's first-class protected remote topology through the official authenticated HTTPS/mTLS application/API protocol under plans 10 and 17; remote binding never weakens local-first operation, authorization, privacy, fencing, or explicit placement.
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
pub struct ConfigKey(NativeKindCode);
pub struct ConfigModuleId(NativeKindCode);
pub struct ConfigRegistryVersion(u64);
pub struct ConfigRegistryDigest(ManifestDigest);
pub struct ConfigDescriptorRefV1 {
    pub key: ConfigKey,
    pub registry_digest: ConfigRegistryDigest,
}
pub struct ConfigLayerId(EntityId);
pub struct ConfigRevisionId(EntityId);
pub struct ConfigActivationId(EntityId);
pub struct EffectiveConfigSnapshotId(EntityId);
pub struct EffectiveConfigDigest(ManifestDigest);
pub struct ConfigConsumerId(NativeKindCode);
pub struct CredentialRefId(EntityId);

pub enum ConfigValueV1 {
    Boolean(bool),
    SignedInteger(i64),
    UnsignedInteger(u64),
    DecimalMicros(i64),
    DurationMicros(u64),
    ByteSize(u64),
    Text(SchemaBoundValueRef),
    Enum(NativeKindCode),
    StringSet(SchemaBoundValueRef),
    OrderedList(SchemaBoundValueRef),
    TypedMap(SchemaBoundValueRef),
    Scope(ScopeSelectorV2),
    Entity(EntityRef),
    Credential(CredentialRefId),
    Structured(SchemaBoundValueRef),
}

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
pub enum ConfigSensitivityV1 {
    CatalogSafe,
    Protected,
    CredentialReference,
}

pub struct ConfigDescriptorV1 {
    pub key: ConfigKey,
    pub module_id: ConfigModuleId,
    pub schema_id: SchemaId,
    pub value_kind: ConfigValueKindV1,
    pub default: ConfigValueV1,
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
    pub owner_crate: NativeKindCode,
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
    HostIntegration { host_profile: HostProfileRef, host_instance: HostInstanceId },
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
    pub value: ConfigValueV1,
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
- `ConfigActivationManifestV1` pointing through ordered `ConfigActivationMemberV1` rows to every exact `(owning_shard, layer_id, revision_id, revision_digest)` member;
- one profile-owned `config_activation_heads` compare-and-swap pointer that makes only a complete manifest/member set resolver-visible;
- `EffectiveConfigSnapshotV1` metadata/digests where a durable pin is required;
- `ConfigConsumerAcknowledgementV1` by component instance/generation;
- audit/outbox events, migration receipts, drift observations, and operation links;
- credential-reference metadata only, never secret material.

The three PR 6E persistence records are fully shaped:

```rust
pub struct ConfigLayerRevisionV1 {
    pub revision_id: ConfigRevisionId,
    pub layer_id: ConfigLayerId,
    pub target: ConfigTargetV1,
    pub parent_revision: Option<ConfigRevisionId>,
    pub registry_version: ConfigRegistryVersion,
    pub registry_digest: ConfigRegistryDigest,
    pub entries: Vec<ConfigRevisionEntryV1>,
    pub actor: ActorRef,
    pub reason: Option<CatalogSafeText>,
    pub idempotency_key: IdempotencyKeyV1,
    pub created_at: UtcMicros,
}

pub struct ConfigRevisionAbandonmentV1 {
    pub abandonment_id: ManifestId,
    pub revision_id: ConfigRevisionId,
    pub reason: ReasonCode,
    pub actor: ActorRef,
    pub abandoned_at: UtcMicros,
}

pub struct ConfigRevisionPreparationV1 {
    pub preparation_id: ManifestId,
    pub activation_id: ConfigActivationId,
    pub target: ConfigTargetV1,
    pub owning_shard: ShardId,
    pub revision_id: ConfigRevisionId,
    pub revision_digest: ManifestDigest,
    pub effective_snapshot_id: EffectiveConfigSnapshotId,
    pub effective_digest: EffectiveConfigDigest,
    pub lease_epoch: u64,
    pub expires_at: UtcMicros,
    pub receipt_digest: ManifestDigest,
}

pub enum ConfigPreparationReleaseOutcomeV1 {
    Published,
    Failed,
    Superseded,
    Expired,
}

pub struct ConfigPreparationReleaseV1 {
    pub release_id: ManifestId,
    pub preparation_id: ManifestId,
    pub outcome: ConfigPreparationReleaseOutcomeV1,
    pub actor: ActorRef,
    pub released_at: UtcMicros,
    pub receipt_digest: ManifestDigest,
}

pub struct ConfigRevisionEntryV1 {
    pub key: ConfigKey,
    pub operation: ConfigEntryOperationV1, // Set | Unset
    pub value: Option<ConfigValueV1>,      // canonical typed value for Set
    pub sanitization_receipt: Option<SanitizationReceiptId>, // content-bearing values only
}

pub struct ConfigActivationManifestV1 {
    pub activation_id: ConfigActivationId,
    pub previous_activation: Option<ConfigActivationId>,
    pub registry_version: ConfigRegistryVersion,
    pub registry_digest: ConfigRegistryDigest,
    pub member_set_digest: ManifestDigest,
    pub source_resolution_watermark: VectorWatermark,
    pub members: Vec<ConfigActivationMemberV1>,
    pub actor: ActorRef,
    pub idempotency_key: IdempotencyKeyV1,
    pub published_at: UtcMicros,
}

pub struct ConfigActivationMemberV1 {
    pub ordinal: u32,
    pub target: ConfigTargetV1,
    pub owning_shard: ShardId,
    pub layer_id: ConfigLayerId,
    pub revision_id: ConfigRevisionId,
    pub revision_digest: ManifestDigest,
    pub preparation_id: ManifestId,
    pub effective_snapshot_id: EffectiveConfigSnapshotId,
    pub effective_digest: EffectiveConfigDigest,
}

pub struct ConfigConsumerAcknowledgementV1 {
    pub consumer: ConfigConsumerId,
    pub instance: ConsumerInstanceId, // opaque per-process component instance
    pub activation_id: ConfigActivationId,
    pub activation_member_set_digest: ManifestDigest,
    pub runtime: ConfigConsumerRuntimeV1, // component version + safe process identity class
    pub state: ConfigConsumerRuntimeStateV1, // Applied | PendingRestart | PendingOperation | Failed
    pub acknowledged_at: UtcMicros,
}
```

Storage contracts:

- `ConfigLayerRevisionV1`: primary key `revision_id`; uniqueness on `(target, idempotency_key)`; rows are immutable and have no mutable activation state. Activation is derived only from membership in the manifest reached by `config_activation_heads`. Abandonment is a separate immutable `ConfigRevisionAbandonmentV1` event/row with uniqueness on `revision_id`. A revision with an unexpired preparation pin or any manifest membership cannot be abandoned/collected. Rows referenced by any activation manifest, receipt, export, or replay pin are retained permanently; an unreferenced revision can be collected only after a durable abandonment row and the abandonment window. Entry values are bounded by descriptor maximum sizes and one revision stays <=1 MiB canonical encoding. Owning shard follows the target per Section 6.
- `ConfigRevisionPreparationV1`: primary key `preparation_id`; unique `(activation_id, target)` and `(activation_id, revision_id)`; owner-shard transaction checks revision digest/no abandonment, materializes the target-specific effective snapshot, and pins both until expiry under the lifecycle lease epoch. It is immutable. Publication consumes its exact receipt. Release is a separate immutable `ConfigPreparationReleaseV1`, unique by preparation, whose digest covers the preparation/outcome/actor/time. Effective pin state is derived from preparation expiry, release, and manifest membership; neither preparation nor revision is updated in place. A released/expired loser remains history and makes only unreferenced revisions eligible for later abandonment.
- `ConfigActivationManifestV1`: primary key `activation_id`; uniqueness of one member per target per manifest and one successor per `previous_activation` (a linear append-only chain); index on `published_at`. Manifests are append-only and retained while any snapshot, receipt, or the rollback window references them; member count is bounded by resolved target count and one manifest stays <=1 MiB. Owning shard: the profile shard, matching the profile-owned publication in Section 8.2.
- `ConfigActivationMemberV1`: primary key `(activation_id, ordinal)`; unique target digest and `(activation_id, layer_id, revision_id)` within the manifest; exact canonical target, owning-shard, layer, revision/digest, preparation receipt, and target-specific effective snapshot/digest fields. The target encoding is rehashed on write; every unexpired preparation must match the activation/target/revision/snapshot tuple. Members are immutable and retained with their manifest. `member_set_digest` covers the complete sorted member tuples. A manifest with zero members, duplicate target, missing/expired preparation, missing/unavailable/abandoned revision, or any target/shard/revision/snapshot digest mismatch cannot publish.
- `config_activation_heads`: one row per profile with `(manifest_id, generation)`; advancement is compare-and-swap from `previous_activation` and commits atomically with the new manifest and all member rows. Resolvers read only through this head and reject a missing/member-count-mismatched target instead of scanning for the latest timestamp.
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
3. append candidate immutable revisions to each owning shard with expected versions;
4. under one lifecycle lease epoch, create an immutable preparation pin in each owner-shard transaction that checks revision digest/no abandonment, materializes the target-specific effective snapshot/digest, and reserves that tuple through publication expiry;
5. in one profile-shard transaction, revalidate every unexpired preparation receipt, insert the activation manifest plus its complete ordered member set referencing each preparation/revision/snapshot tuple, verify `member_set_digest`, then advance the profile's current-activation pointer;
6. emit one activation outbox event and consumer notifications;
7. append immutable preparation-release receipts after publication acknowledgement or failure; losers may receive abandonment records only after no unexpired/unreleased pin or manifest membership remains, and only those witnessed rows become eligible for safe garbage collection after the configured window.

Resolvers ignore every revision not referenced by the current activation manifest, so readers observe either the previous manifest or the complete new manifest. This is atomic visibility, not a distributed database transaction. If an activated shard later becomes unavailable, coverage is `Partial/Unavailable`; the resolver never silently falls back to an older layer.

The profile-owned current-activation pointer is the single visibility boundary; it can reference a manifest only after all member rows exist in that same transaction, and the manifest never carries a singular layer/revision/snapshot shortcut. Plan 02's physical schema is `config_revision_preparations` plus `config_activation_manifests`/`config_activation_members`. Activation validation proves set equality between resolved targets, preparations, revisions, target-specific snapshots, and members before publication. Later owner-shard unavailability yields typed partial coverage; it never causes fallback to another target's snapshot.

Merged PR #425 (`de3d05dc`, final head `d3bb28b5`) reinforces the boundary between configuration and destructive identity/store workflows. Settings may select policy and show status, but split-store consolidation is a separate offline operation: freeze both canonical store families, identify holders by path plus file/inode, acquire reservations, create and verify dual backups, recompute deterministic confirmation under reservation, execute a restartable ledger/staging workflow, preserve remapped LCM edges, verify the complete destination, and publish cutover only after proof. Saving a configuration key cannot start, bypass, weaken, or mark that workflow effective.

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

Configuration exposes Section 13.4's target-scoped host component set, install scope, registration/profile selection, enablement, trust, approval, update, and credential-reference settings, while returning generated profile IDs/digests/grant ceilings/definition budgets as read-only integrity state. MCP registration files are generated projections of that state, not another config source. A widening commit can become desired/activated but remains `pending_reconnect` until the named host acknowledges a fresh connection with the exact profile/catalog/credential digests; it is never reported effective merely because a client accepted `tools/list_changed`.

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

The generated descriptor registry assigns each setting one task-oriented group (`capture`, `retrieval-and-hints`, `privacy-and-redaction`, `storage-and-retention`, `automation`, `integrations`, `remote-brain`, or `interface`) and one visibility tier (`basic`, `advanced`, or `operator`). Default navigation progressively discloses basic settings by user task; advanced/operator tiers require an explicit filter and authorization, while exhaustive search always returns every authorized descriptor with its tier, source chain, safety floor, impact, and restart/rescan/reindex consequences. These are presentation metadata on the one descriptor, not alternate defaults or a second config hierarchy.

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
| Hosts and hooks | installed host integration, hook enablement, latency budgets, fail-closed mode, hint delivery budgets, session pinning, source-broker registered-source classes, user-effect-broker per-operation policy ceilings, service-manager isolation-probe identity/interval/health (no path or grant body) |
| Privacy/redaction | detector sets, thresholds, structured field policies, actions, decode/archive limits, custom manifests, retention/quarantine roles, scan schedules |
| Sessions and activity | attribution modes, message views, compaction/summary policy, workflow/goal capture, evidence retention |
| Code/Git/delivery | index modes, graph generation triggers, refs/worktrees, ignore policy, delivery refresh, diagnostics capture |
| Query/search | lexical/fuzzy/vector/rerank profiles, exact-match floor, candidate budgets, graph expansion, diversity, temporal current/as-of/evolution/forensic policy, authority/supersession/conflict rules, copy/summary-horizon policy, fusion/calibration, time/coverage/no-answer defaults, corpus/promotion gates; signed representation artifact IDs/sources, explicit automatic-download authorization, offline-only mode, allowed residency/device/runtime, 4 GiB default disk and 2 GiB default resident-memory budgets, cold-load concurrency, idle unload, pin/eviction/revocation/rebuild/fallback policy per plan 05 §11.2A |
| Hints/coordination/scout | classifier bundles, routing, scout off/shadow/deterministic/model-assisted mode, discovered model capability/credential reference, read/egress grants, coalescing/concurrency/tool/model/cost budgets, evidence/silence/dedupe/cooldown/expiry/delivery thresholds, proximity/task-materiality, terminal horizons |
| Tasks/plans/executors | task graph/decomposition limits, legal work/gate/acceptance kinds, scheduler pause/concurrency/fairness/aging/batches, lease/heartbeat/start/cancel timeouts, executor adapters/hosts/capacity/workspace modes, provider/model/reasoning effort/routes/fallback, tool/effect grants, privacy/egress, worktree/branch policy, budgets/schedules/retries/circuit breakers, context-packet limits/expiry/materiality, the complete lowering-only steering payload/batch/Turn/rate/cooldown descriptor set, saved task views/notifications |
| Memory/knowledge | retrieval/trust/conflict/retention policies, autonomous curation cadence and quality constraints |
| Automations/skills | scheduler, run budgets, autonomous curator/reflector/skill-writer policies, installation authority, health pauses |
| Storage/projectors | desired `StoreIsolationModeV1` (`DedicatedServiceIdentity`/`RemoteAuthorityOnly`/`SameUserDegraded`), read-only observed `StoreIsolationStatusV1` proof with validity/receipts, data locations by allowed location class, WAL/lease budgets, blob/backup/log retention, projection/index generations, compaction |
| API/MCP/CLI/dashboard | loopback bind, session lifetime, request/page/budget caps, SSE caps, task-graph edit-bundle TTL/byte/file/item/sweeper bounds, host component set/install scope/registration-profile selection, optional context/work/operator MCP enable/narrow/approval/credential settings, renderer preferences, dashboard preferences; generated MCP IDs/digests/grant ceilings/definition budgets are visible immutable state |
| Costs/observability | pricing catalog version, sampling, safe metrics, log levels, tracing budgets, accounting horizons, diagnostic segment rotation/retention/quota/hold policy, saved diagnostic producer-version filters and an explicit default view (`all`, `current_runtime_set`, or `compatible_protocol`) |

`StoreIsolationModeV1` is desired configuration. `StoreIsolationStatusV1` is observed, expiring operational evidence and is never editable configuration: each platform probe writes a successor proof variant or a degraded finding without mutating history. UI/CLI derive convenience statements such as “client database read denied” only from the active variant and its unexpired receipts. `RemoteAuthorityOnly` validates local-absence/cache evidence and does not pretend to own local database/key receipts; `DedicatedServiceIdentity` requires service identity, database-root, endpoint, and key-authority receipts; `SameUserDegraded` carries reasons, not false booleans.

The producer version field itself, its emission requirement, and legacy-unknown truth are hard invariants, not settings. A default log-view filter may change presentation only: every response echoes it and reports excluded/unknown counts, and no setting may delete, rewrite, or relabel old records.
| Updates/migrations | update channel, daemon drain policy, compatibility windows, import schedules, retirement holds |
| Extensions | enabled manifests, sandbox/resource budgets, privacy/egress permissions, version pins |

The generated session-summary descriptors are explicit:

| Key | Built-in profile default | Contract |
|---|---|---|
| `sessions.summary.model` | catalog entry `gpt-5.6-terra` | `ModelCapabilityRefV1`; TraceDecay user-profile-wide across every host/Hermes profile; requested and actual model/revision are receipted. |
| `sessions.summary.reasoning_effort` | `extra_high` | Canonical `ModelReasoningEffortV1::ExtraHigh`; validated against the selected runtime capability. |
| `sessions.summary.fallback_policy` | explicit eligible fallback, otherwise anchored evidence-only | Never silent downgrade. Records unavailability/privacy/budget reason, selected actual route, or `synthesis_unavailable`. |
| `sessions.summary.anchor_policy` | consequential-claim markers required | Requires plan-23 source coverage, validated marker manifest, maximum 256 entries, authorization and sanitizer digests. Cannot permit model-minted anchors. |
| `task_graph.lifecycle_checkpoint.enabled` | `true` for supported/trusted local hook bindings | Advisory same-agent stop checkpoint; absence/disablement fails open to lease reconciliation and is visible in integration status. |
| `task_graph.lifecycle_checkpoint.materiality_policy` | cataloged progress/block/terminal-debt policy | Versioned eligibility only; cannot infer completion, create work, widen grants, or compete with ordinary hints. |

Provider/project/session layers may narrow privacy, budgets, or disable synthesis, but cannot create a host-profile-specific TraceDecay store or silently replace the profile default. Settings, CLI, MCP, API, and SDK show desired, activated, effective, requested, and last-actual values with override provenance and capability gaps.

### Optional semantic code-search and rerank descriptors

Epoch one registers no semantic-backend selector. Optional native semantic code search is FastEmbed-only and uses an exact benchmark-promoted embedding artifact plus a separately promoted optional `BGERerankerV2M3` artifact, both managed by `representations.artifacts.*`; generations remain managed by `representations.generations.*`. `JinaEmbeddingsV2BaseCode` is the primary embedding candidate and `GTELargeENV15Q` the required comparator, not a runtime fallback. `search.universal`/`code.search_symbols` and the separately qualified `code.redundancy` contribution consume the same resolved artifact/runtime profile but have independent desired and promotion gates: either may be effective while the other is disabled or rejected. These descriptors are the sole writable intent. Activated, effective, and observed state are read-only daemon/application evidence with exact artifact, model, runtime, device, generation, coverage, and receipt provenance; no resolver derives one state from another or selects an alternate model.

| Key | Built-in default | Contract |
|---|---|---|
| `query.search.semantic.enabled` | `false` | Desired native semantic contribution to search/context; remains disabled until the search benchmark promotion receipt qualifies it. It does not gate `code.redundancy`. |
| `query.search.semantic.embedding_artifact` | none | Exact verified FastEmbed-compatible, benchmark-promoted embedding `RepresentationArtifactId`; required before activation, never a family/latest selector. |
| `query.search.semantic.redundancy.enabled` | `false` | Independent desired use of the same compatible artifact/runtime and vector generation by `code.redundancy` when the pair/cluster promotion receipt qualifies it. It does not require semantic search activation and adds no model/index lifecycle; `false` forces structural-only redundancy. |
| `query.search.semantic.redundancy.neighbors_per_entity` | promoted-profile default | Positive benchmark-qualified bound no greater than the signed profile hard cap; request budgets may narrow but never widen it. |
| `query.search.semantic.native_rerank.enabled` | `false` | Separate desired native BGE rerank stage. |
| `query.search.semantic.native_rerank.artifact` | none | Exact verified BGE rerank artifact; no alternate on absence/incompatibility. |
| `query.search.semantic.rerank_top_n` | `25` | Integer `1..25`; hard cap 25 applies to every native or model-assisted route. |
| `query.search.semantic.strict` | `false` | `true` returns a typed semantic/rerank availability error; `false` preserves the byte-stable lexical result/order when optional stages cannot run. |
| `representations.artifacts.automatic_download_authorized` | `false` | Explicit consent for allowlisted artifact download; install/import still pins manifest, digest, license, and exact model. |
| `representations.artifacts.offline_only` | `true` | Forbids artifact network fetch; verified local import/cache remains usable. |
| `representations.runtime.device` | `cpu` | Exact allowed device; no silent device or runtime change. |
| `representations.runtime.cpu_threads` | bounded auto | Positive host-budgeted thread ceiling recorded as requested/actual. |
| `representations.runtime.batch_size` | benchmark-qualified bounded auto | Positive batch ceiling; requested/actual batch is observed. |
| `representations.runtime.max_resident_bytes` | `2 GiB` | Shared representation-runtime residency ceiling. |
| `representations.runtime.max_disk_bytes` | `4 GiB` | Verified artifact/cache/generation disk ceiling. |
| `representations.runtime.idle_unload` | enabled bounded duration | Daemon-owned residency policy; clients cannot keep a native session alive. |

Optional model-assisted reranking is a different registered route, never an embedding backend or fallback model for the promoted FastEmbed embedding or native BGE reranker. `query.search.model_rerank.enabled` defaults `false`; `capability`, `credential`, and `model` have no default and must resolve to one discovered Codex Spark/app-server-style capability, credential reference, and exact model. `privacy_egress_policy`, `max_cost`, `max_input_tokens`, `deadline_ms`, and `top_n` (default 25, range `1..25`) are required bounded profile fields. Status exposes desired/activated/effective/observed plus discovered capability and requested/actual route/model/cost/tokens/deadline. Unavailable, denied, timed-out, or malformed output preserves the pre-rerank order. Search Quality Lab replay/ablation and plan 22 active hinting/scout consume these same descriptors and receipts; neither creates a hidden override or grants model access merely because the capability was discovered.

The lifecycle checkpoint's maximum inward continuation is a correctness constant of one, not a writable setting. Claude's larger native stop-block cap or environment override never widens it.

Hard-coded correctness constants and safety maxima are not mislabeled as user settings. They still appear in capability/status documentation when relevant, but are not writable. Conversely, a behavior marketed or documented as configurable cannot remain an unregistered constant.

### 13.0 Provider freshness descriptors

These profile defaults configure plan 09's daemon operation; they never make a search/read perform ingestion. Provider/host overrides may only narrow budgets or disable eligible sources, and the current effective values/provenance are visible in Settings plus CLI/MCP/API/SDK:

| Key | Type/default | Validation and impact |
|---|---|---|
| `capture.refresh.background_policy` | enum / `on_source_change` | `off`, `on_source_change`, or `bounded_periodic`; periodic wakeups still skip unchanged frontiers and cannot create project×source rescans. |
| `capture.refresh.max_concurrent_sources` | integer / `2` | `1..16`; global across joined requesters and projects for one profile authority. |
| `capture.refresh.max_records` / `max_input_bytes` | integer + bytes / `5_000_000` / `16GiB` | Per operation hard bounds; partial completion returns a resumable frontier and explicit coverage. |
| `capture.refresh.max_wall_time` / `max_rss` | duration + bytes / `60s` / `2GiB` | Current cold-history target and resource ceiling; exceeding either cancels at a committed boundary rather than advancing an uncommitted cursor. |
| `capture.refresh.yield_every_records` | integer / `10_000` | `100..100_000`; cooperative cancellation/progress cadence, not a commit-size promise. |
| `capture.refresh.required_freshness` | enum / `bounded_stale` | Default read requirement only; an authoritative caller starts/joins `capture.refresh` explicitly and receives an operation ref. |

`source-open count <= eligible SourceInstanceId count`, one sweep per committed frontier, query-write sentinel zero, and the FM-153 30-project ≤60-second gate are non-disableable invariants rather than settings.

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
| `query.cursor.interactive_ttl` | duration / `15m` | `1m..24h`; catalog-bound interactive cursors only. Export/bulk continuations use their declared job lifetime; key retirement covers the maximum outstanding declared lifetime. |

The ten liveness descriptors plus the cursor-lifetime descriptor are profile defaults with optional initiative/executor/provider narrowing only where the descriptor declares that scope. Deny/safety floors win. Settings shows desired/activated/effective/observed values, source, generation, affected active-attempt count, and whether activation is hot, next-heartbeat, or workflow-mediated. Tests compare generated liveness values to plan-24 fixtures and cursor expiry/rotation values to plans 01/05/10/17 so a renamed key, unit drift, or conflicting default blocks both PRs.

### 13.1A Live steering lowering descriptors

Plan 01 owns the non-configurable absolute ceilings and Plan 08 publishes the
single `SteeringLimitsV1` catalog record. This registry owns only the effective
narrowing descriptors below. Every key is a profile default; an initiative may
narrow any key, while a `HostIntegration` target may narrow only batch/Turn
render limits and cooldown. Maxima merge by minimum and cooldown merges by
maximum. No project, CWD, task comment, adapter payload, API request, plugin, or
environment value can widen them.

| Key | Type/default | Range, scope, and activation |
|---|---|---|
| `task_graph.steering.payload_max_bytes` | bytes / `16KiB` | `256B..16KiB`; Profile/Initiative; hot for new admission and the next unhanded claim. |
| `task_graph.steering.payload_max_tokens` | integer / `2048` | `32..2048`; Profile/Initiative; measured by the pinned tokenizer before admission and again before claim. |
| `task_graph.steering.batch_max_members` | integer / `8` | `1..8`; Profile/Initiative/HostIntegration; next unhanded claim, never truncation. |
| `task_graph.steering.batch_max_bytes` | bytes / `32KiB` | `256B..32KiB`; Profile/Initiative/HostIntegration; effective value may be below payload maximum, in which case the directive is explicitly blocked before handoff. |
| `task_graph.steering.batch_max_tokens` | integer / `4096` | `32..4096`; Profile/Initiative/HostIntegration; same blocked-not-truncated rule. |
| `task_graph.steering.turn_max_directives` | integer / `4` | `1..4`; Profile/Initiative/HostIntegration; next safe boundary/Turn accounting snapshot. |
| `task_graph.steering.turn_max_tokens` | integer / `4096` | `32..4096`; Profile/Initiative/HostIntegration; shared across all steering batches in that Turn and cannot borrow hint/scout budget. |
| `task_graph.steering.rolling_60s_max_directives` | integer / `16` | `1..16`; Profile/Initiative; per target over an authority-clock 60-second window. |
| `task_graph.steering.advisory_cooldown_ms` | duration / `250ms` | `250ms..60s`; Profile/Initiative/HostIntegration; narrowing means increasing the minimum interval. Required directives are rate-limited at admission but never silently suppressed by advisory cooldown. |

Each effective snapshot contains all nine values, source chain, target scope,
Plan-08 catalog/config/tokenizer digests, activation generation, and measured
counts. Cross-field validation requires positive values and a batch/Turn token
and byte budget that can represent at least one legal payload; a stricter batch
below an already-admitted payload is allowed only because the blocked workflow
below is explicit. Unknown or partial resolution fails closed.

Activation is hot at the admission and delivery-claim boundaries. A directive
retains its admitted snapshot for identity/audit; a claim that has already
issued its handoff token completes under its claim-pinned limits because bytes
may already be model-visible. Before handoff, the daemon re-resolves the current
effective snapshot. If lowering now conflicts with an admitted directive, it
records Plan 01 `BlockedByLimitChange` plus typed
`steering_blocked_by_limit_change`, renders zero bytes, and leaves a required
directive fenced. Legal remediation is a bounded higher-sequence superseding
directive or controller-authorized pre-delivery cancel. It never waives,
truncates, splits hidden text, retries into a larger prompt, or grandfather-
delivers above the new limit. Later loosening affects new admission/claims only
and cannot enlarge a pinned directive or batch.

Brain Settings exposes one **Steering limits** panel with absolute/effective
values, source chain, current Turn/rate counters, affected pending directives,
activation generation, blocked reason, and the exact supersede/cancel actions;
it never displays protected payload. Generated CLI `tracedecay config
get|set|history|diff task_graph.steering.*`, HTTP/SDK config bindings, optional
operator MCP bindings, dashboard forms, status, doctor, and SSE consume the
same descriptors/view. Fixtures cover every default/range/scope merge,
max-decrease/cooldown-increase rule, forbidden widening, cross-field failure,
hot activation before/after handoff, already-admitted required/advisory state,
blocked remediation, restart/replay, slow-client resync, and byte-semantic
CLI/MCP/HTTP/Rust/TypeScript/Python/dashboard parity.

### 13.2 Autonomous automation admission descriptors

These descriptors govern curator/reflector/skill-writer/profile-learning admission. The catalog-owned trigger class, input contract, relevant event/projection channels, materiality predicate, self-origin exclusion, and active-writer/coverage safety floor are visible but not weakenable settings; a plugin may extend them only through a versioned validated manifest.

| Key | Type/default | Validation and impact |
|---|---|---|
| `automation.admission.event_driven` | bool / `true` | Safety/performance floor for production loops; clock ticks may wake bounded dirty scopes but cannot enable periodic all-scope scans. |
| `automation.admission.session_quiet_period` | duration / `5m` | `0..2h`; terminal thread/session boundary or registered high-value event may satisfy early. |
| `automation.admission.project_quiet_period` | duration / `15m` | `0..6h`; coalesces related project activity before cross-session curation. |
| `automation.admission.max_dirty_age` | duration / `6h` | `5m..7d`; the maximum debounce boundary prevents perpetual postponement under continuous activity without pretending active/unknown writers are idle or bypassing input-digest dedupe. |
| `automation.admission.minimum_events` | integer / `1` | `1..10_000`; combined with task-specific eligible-token/pattern gates, never counts the scheduler's own events. |
| `automation.admission.minimum_tokens` | integer / `256` | `0..1_000_000`; high-value correction/failure/feedback/boundary events may bypass; exact bypass reason is receipted. |
| `automation.admission.max_scopes_per_batch` | integer / `32` | `1..256`; fairness/oldest-dirty ordering and per-owner caps prevent one project starving others. |
| `automation.admission.maximum_pending_scopes` | integer / `10_000` | `100..1_000_000`; overflow coalesces by owner/task and raises degraded coverage, never drops the source events. |
| `automation.admission.max_input_characters` / `max_input_tokens` | integer + integer / `1_000_000` / `200_000` | Hard preflight ceilings within backend/model context limits; bounded deterministic selection/chunking must fit both before launch. |
| `automation.admission.max_evidence_items` / `max_source_bytes` | integer + bytes / `10_000` / `64MiB` | Stable evidence-order selection with explicit excluded coverage; a large source cannot create an unbounded prompt or hide the job. |
| `automation.admission.max_run_rss` / `max_run_wall_time` | bytes + duration / `2GiB` / `30m` | Enforced by the shared operation worker; breach quarantines only the effective input digest under the pinned version. |
| `automation.admission.dependency_reevaluation` | enum / `future_evidence_only` | `future_evidence_only`, `dirty_scopes`, or bounded historical window; a version change cannot silently rescan all history. |
| `automation.admission.retry_backoff_initial` / `max` | duration / `5m` / `6h` | Exponential+jittered, `initial<=max<=7d`; retry binds the same failed input digest and preserves new concurrent dirty generations. |
| `automation.admission.retry_attempt_cap` / `deadline` | integer + duration / `5` / `24h` | `1..32`, `5m..7d`; implemented by the shared operation attempt substrate, never a curation-only retry loop. |
| `automation.admission.circuit_failure_threshold` / `cooldown` | integer + duration / `3` / `1h` | `1..32`, `1m..7d`; poison input quarantines and uncertain effects require reconciliation rather than blind retry. |
| `automation.admission.skip_episode_rollup_interval` | duration / `1h` | `1m..24h`; equivalent interval/lock/no-change observations update one episode/metric rollup instead of appending receipts or fake run rows. |

Per-task descriptors select authoritative scope and eligible minimums, but cannot change a job's registered trigger class, turn schedule time into sufficient admission, bypass identical-terminal-input suppression, infer idle from unknown/partial writer state, include self-generated scheduler/run events as new evidence, or erase dirty state on failure/reconciliation. A config change dirties only jobs whose generated dependency/input-contract digest changes. Evidence-driven jobs remain dormant after terminal `NoChange` until a relevant frontier advances; the rollup interval refreshes observability only and performs no model/tool work.

### 13.3 Task-graph edit-bundle descriptors

Plan 24 owns task-graph semantics and plans 10/17 own the public transport/SDK workflow. This registry owns the bounded temporary-edit policy consumed unchanged by CLI, HTTP, SDK, and optional MCP bindings:

| Key | Type/default | Validation and impact |
|---|---|---|
| `task_graph.edit_bundles.ttl` | duration / `2h` | `5m..24h`; applies to unsubmitted bundle generations and ordinary failed-validation repair windows. Shortening retires already-expired bundles immediately. |
| `task_graph.edit_bundles.max_total_bytes` | bytes / `64MiB` | `1MiB..256MiB` hard ceiling over observed uncompressed bytes; checked while streaming, never after full buffering. |
| `task_graph.edit_bundles.max_file_bytes` | bytes / `2MiB` | `64KiB..8MiB`, and `<= max_total_bytes`; forces sharding instead of one unbounded frontmatter document. |
| `task_graph.edit_bundles.max_files` | integer / `4096` | `1..16384`; includes manifest and every archive entry, declared or observed. |
| `task_graph.edit_bundles.max_items` | integer / `50000` | `1..100000`; counts canonical graph items across all shards before referential validation and cannot exceed the domain/transport vector ceiling. |
| `task_graph.edit_bundles.sweep_interval` | duration / `5m` | `1m..1h`; startup sweep is mandatory and cannot be disabled. Sweeping performs no parsing/model work and follows no link. |

Archive depth eight, normalized-name length 128 bytes, strict YAML/CommonMark grammar, owner-only `0700`/`0600` modes, no-follow/inode containment, complete secret scanning, immediate successful-submit purge, and purge on secret/unknown/containment failure are non-weakenable floors, not settings. The managed runtime root is composition-owned; no registry key, environment alias, API field, CLI flag, or plugin manifest accepts a server path. UI/CLI display desired/effective bounds, current bundle counts/bytes/oldest expiry, sweep state, and safe retirement receipts without content or paths.

### 13.4 Plugin installation and MCP registration profiles

Host integration is a first-class configuration target, not a collection of provider-specific booleans. `ConfigTargetRefV1::HostIntegration { host_profile: HostProfileRef, host_instance: HostInstanceId }` resolves an opaque host target through the application; install scope is a typed desired value, and config keys/values never contain a path or raw host config body. Plan 20 solely owns `HostIntegrationDesiredStateV1`, `DesiredPackageStateV1`, `DesiredComponentStateV1`, `HostHookComponentPolicyV1`, `McpRegistrationNarrowingV1`, `HostTrustPolicyV1`, and `HostBundleUpdatePolicyV1`; plan 08 owns `HostInstallSetV1` and MCP profile specs, while plan 27 only consumes and projects them:

```rust
pub struct HostIntegrationDesiredStateV1 {
    pub host_profile: HostProfileRef,
    pub host_instance: HostInstanceId,
    pub install_scope: HostInstallScopeV1,
    pub packages: BTreeMap<RegistryEntryId, DesiredPackageStateV1>,
    pub install_set: HostInstallSetV1,
    pub roles: BTreeMap<RegistryEntryId, DesiredComponentStateV1>,
    pub hook_policy: HostHookComponentPolicyV1,
    pub mcp_narrowing: BTreeMap<McpLogicalRegistrationId, McpRegistrationNarrowingV1>,
    pub trust_policy: HostTrustPolicyV1,
    pub update_policy: HostBundleUpdatePolicyV1,
    pub credential_ref: Option<CredentialRefId>,
}

pub struct HostBundleUpdatePolicyV1 {
    pub channel: HostBundleUpdateChannelV1,
    pub automatic: BoundedAutomaticUpdatePolicyV1,
}

pub struct HostHookComponentPolicyV1 {
    pub enabled_intents: BTreeSet<RegistryEntryId>,
    pub maximum_delivery: HostHookDeliveryBudgetV1,
    pub trust_requirement: HostHookTrustRequirementV1,
}
pub struct HostHookDeliveryBudgetV1 {
    pub max_model_visible_bytes_per_turn: u32,
}
// Imported unchanged from plan 01's tracedecay-domain::hooks_v1:
// HostConfigSourceV1, HostHookDefinitionObservationV1,
// HookDefinitionRepresentationV1, HostHookTrustRequirementV1, and child axes/receipts.
```

`host_profile` identifies only the installed/configured host artifact owner. Discovery records how that opaque host target was proven—from the installed plugin manifest/receipt or explicit host configuration—and never derives it from a session workspace, process CWD, ambient `HOME`/`HERMES_HOME`, provider helper, or prior invocation. After authorization resolves desired state, application mints plan 01's sealed `HostIntegrationRuntimeRefV1`; its mandatory `tracedecay_profile_id` is the authenticated user-global data owner and is excluded from editable desired config, descriptors, host files, request fields, and provider metadata. Runtime adapters receive that sealed binding plus separate per-invocation declared scope/workspace; a session may not rewrite either owner. Zero, one, or many Hermes host profiles all receive runtime refs with the same `tracedecay_profile_id`, while each retains its own deployment/trust/effective-state receipt.

`McpRegistrationNarrowingV1` can only remove exact plan-08 `BindingId`s or lower scope/sensitivity/grant ceilings for a registration already present in `install_set`; the selected `McpSurfaceProfileId` exists only inside that set. `BoundedAutomaticUpdatePolicyV1` carries enablement, maintenance window, and restart behavior under the target's authority. The remaining child types are closed validated value/policy records with no path, body, prompt, executable, URL, or secret field.

For Claude, desired TraceDecay output is only the signed plugin `hooks/hooks.json` generated from the pinned 30-event catalog; config cannot author arbitrary events/handlers, shell commands, URLs, MCP calls, prompts, agents, watch paths, environment writes, or async behavior. Observed inventory covers user `~/.claude/settings.json`, project `.claude/settings.json`, local `.claude/settings.local.json`, managed policy, enabled plugin JSON, active skill/agent frontmatter, and observable session/built-in definitions. Sources compose additively and retain component activation lifetime, agent Stop conversion, and skill-only `once` evidence.

Claude `disableAllHooks` and `allowManagedHooksOnly` resolve through host-native precedence. Lower layers never disable managed hooks; managed-force-enabled plugin exemptions remain visible. `/hooks` is read-only and there is no individual retained-definition disable or Codex-style hash trust. Each `HostHookDefinitionObservationV1` keeps handler kind, event/type support disposition, sync/async/rewake, matcher/`if`, host dedupe, control/managed/component state, run visibility, and version evidence as orthogonal axes. HTTP headers/URLs, MCP input, prompt/agent bodies, command/args, paths, and environment values remain protected non-rendered evidence.

For Codex, desired TraceDecay output selects only the generated plugin-default `hooks/hooks.json` representation; it never writes both that file and inline `[hooks]` in one layer. Observed `HostConfigSourceV1` inventory covers system/cloud/MDM/`requirements.toml`, user `~/.codex/hooks.json`, user `~/.codex/config.toml`, trusted-repository `<repo>/.codex/hooks.json`, trusted-repository `<repo>/.codex/config.toml`, session sources, and every enabled plugin default/manifest-declared source. Active sources compose additively—higher layers do not replace lower hooks. One layer containing JSON plus inline hooks records `dual_hook_representation` and the Codex startup warning; repair never deletes or rewrites the foreign representation. Untrusted repositories omit only their project-local source while user/system/plugin sources remain eligible.

Codex feature resolution canonicalizes `[features].hooks`, defaults enabled when no key or policy override exists, and treats `codex_hooks` as an import-only deprecated alias rather than a second effective key. Managed requirements may force hooks true/false and `allow_managed_hooks_only`; the effective view records the winning policy lock and every skipped ordinary definition. Managed system/MDM/cloud/requirements hooks are `ManagedTrusted`, non-disableable, and read-only to ordinary TraceDecay install/repair. `managed_dir`/`windows_managed_dir` are externally deployed absolute-command roots observed without exposing paths or bodies. TraceDecay never turns a hash into `Trusted`, never automates `/hooks`, and never persists the one-off `--dangerously-bypass-hook-trust`; exact host trust hash and separate TraceDecay content digest bind through `HostHookTrustReceiptRefV1`, so changed bytes create `NeedsReview` plus `ChangedSinceReview` until the user acts in Codex. Trust, eligibility, handler support, and freshness remain orthogonal typed axes.

| Key | Typed value | Contract |
|---|---|---|
| `host_integrations.packages` | `BTreeMap<RegistryEntryId, DesiredPackageStateV1>` | Closed signed base/companion package set, exact/version-channel constraints, and generated skill/component enablement or intentional omission; no URL/path/arbitrary package entry. |
| `host_integrations.install_scope` | `HostInstallScopeV1` | One adapter-documented user, machine, or managed-host scope; unsupported or privilege-escalating scope is a typed incompatibility, never silent fallback. |
| `host_integrations.install_set` | `HostInstallSetV1` | Canonical `CoreSkillsCli` plus zero/one/many context/work/operator facade components; dependencies and conflicts are manifest-validated. |
| `host_integrations.roles` | `BTreeMap<RegistryEntryId, DesiredComponentStateV1>` | Generated specialist-role enablement constrained to roles supplied by selected signed packages; no arbitrary prompt/body. |
| `host_integrations.hook_policy` | generated hook-component policy | Per canonical hook-intent enablement, model-visible byte limit, and host trust requirement; the one plugin-default JSON layout and one-advisory-effect invariant are signed catalog metadata, not writable settings. Cannot add arbitrary executable/event definitions, mark Codex trust, edit managed hooks, persist bypass, or weaken feature/policy locks. |
| `host_integrations.mcp_narrowing` | `BTreeMap<McpLogicalRegistrationId, McpRegistrationNarrowingV1>` | Optional narrowing only for registrations/profiles selected exactly once by `install_set`; cannot select or switch a profile. |
| `host_integrations.trust_policy` | `HostTrustPolicyV1` | Ownership acceptance, signature/publisher policy, foreign-state behavior, and host-interaction approval class; cannot waive safety/privacy floors. |
| `host_integrations.update_policy` | `HostBundleUpdatePolicyV1` | Pinned/current/channel selection plus bounded automatic-check policy, maintenance window, and restart behavior; automatic mutation remains bounded by the target's authority. |
| `host_integrations.credential_ref` | `Option<CredentialRefId>` | Opaque least-privilege credential reference and safe status only; no secret or provider config payload. |

The desired value is declarative. Saving it publishes configuration impact and legal next actions but never installs packages, edits a host file, registers MCP, reloads a host, or claims success. Only plan 09's authorized `integrations.install|update|repair|uninstall|verify` workflows cross the root `HostDeploymentPort`; their operation receipts advance observed/effective state.

The published host bundle is a generated component set, not two mutually exclusive install modes. `core` installs generated skills, CLI recipes, and thin hooks without registering MCP and is the default on a host with an available shell and compatible TraceDecay CLI. Independently installable `context`, `work`, and `operator` facade companions may be added in any supported subset. A host without a shell/CLI prerequisite receives typed `shell_or_cli_unavailable` and must explicitly install one or more facade companions; a facade-only deployment is not a second workflow implementation. Every component records the same `HostIntegrationManifestV1` and catalog digest, and signed `HostBundleManifestV1` package metadata carries no duplicate workflow prose.

The facade companions expose three generated logical server registrations, all invoking the thin `tracedecay` integration binary and the same private `tracedecayd` application/catalog authority:

| Registration | Immutable registration-manifest ID | Generated grant ceiling | Generated profile/definition budget |
|---|---|---|---|
| `tracedecay-context` | `tracedecay.mcp.context.v1` | Read-only sanitized context/search/graph/session/memory/task reads; no protected quarantine plaintext or mutation. | exact profile ceilings from plan 08: `agent-core` 12/8k, `research` 24/18k, `developer` 32/24k tools/estimated-definition tokens; registration hard cap 32 tools/24k tokens/128 KiB serialized definitions, 8 KiB per input schema, 512 UTF-8 bytes per description |
| `tracedecay-work` | `tracedecay.mcp.work.v1` | Owner-scoped task/plan/coordination edits and edit-bundle workflow; no config/privacy/automation/storage administration. | `task-worker` 24/16k and `orchestrator` 32/24k tools/estimated-definition tokens; same 32/24k/128-KiB registration hard cap, 8 KiB per input schema, 512 UTF-8 bytes per description |
| `tracedecay-operator` | `tracedecay.mcp.operator.v1` | Explicit administrative operations within existing confirmation/privacy floors; never a generic unrestricted write or quarantine-plaintext grant. | `operator` 24/18k and `admin-lab` 32/24k tools/estimated-definition tokens; same 32/24k/128-KiB registration hard cap, 8 KiB per input schema, 512 UTF-8 bytes per description; explicit opt-in only |

Each release consumes plan 08's exact `McpSurfaceProfileV1` and generated profile manifest, which bind the profile/version/registration, exact ordered `BindingId` set, host features, execution modes, required grants and grant ceiling, tools-only fallbacks, tool/token/serialized-definition/input-schema/description ceilings, estimator, catalog/protocol compatibility, and definition digest. Plan 20 defines no `McpRegistrationProfileV1`. Those generated fields and the table's registration names are integrity inputs, not writable configuration. CI canonicalizes and digests the exact ordered definitions and fails if a profile exceeds any plan-08 ceiling; a release must simplify/coalesce the surface or introduce a reviewed new profile version rather than silently raise a ceiling.

Writable host-target settings are exactly the descriptor set above plus each selected registration's allowlist/scope/sensitivity/grant narrowing fields; legacy `integration.mcp.profiles.<context|work|operator>.enabled` values migrate into `host_integrations.install_set` plus `host_integrations.mcp_narrowing` and are not a second live resolver. Facades are absent/disabled in a core-only desired set; adding a facade or switching its one profile is an explicit install-set update, `context` may be recommended, and `work`/`operator` remain opt-in. No setting can select a second profile for a registration, move an operation between profiles, widen a grant ceiling, raise a definition budget, change an ID/digest, inject a tool schema/description, create another server implementation, trust foreign ownership implicitly, or convert an opaque credential reference into a secret value.

Enablement, operation addition, restored availability, any scope/effect/sensitivity widening, profile/catalog/component digest change, or credential-ceiling change takes effect only after a fresh MCP connection and capability handshake. Disablement, token revocation, capability loss, or narrowing becomes authoritative immediately: the effective set may shrink through a generated list-generation change when supported, otherwise the stale connection terminates; every subsequent call is still reauthorized. A client may support `notifications/tools/list_changed` or deferred tool search, but MCP does not guarantee that behavior; some clients eagerly collect every paginated tool schema. The configured surface therefore remains correct and bounded without progressive disclosure. The pinned profile membership ceiling never changes within a connection, and effective visibility never varies by selected board, thread, task, prompt, or previous call.

Every host target exposes all state planes together:

- `desired`: current target revision, package/component/registration selection, install scope, trust/update policy, and credential refs;
- `activated`: desired revision accepted by configuration and its pending integration operation, if any;
- `observed`: last sanitized host probe, adapter/manifest version, cache age/staleness, installed/enabled versions and digests, ownership/trust evidence, and host reload state;
- `effective`: intersection of desired, generated support, trust/authorization, observed installation, live registration/profile handshake, and current credential availability;
- `drift`: typed missing/extra/version/digest/config/ownership/trust/registration/profile/cache difference with exact remediation authority;
- `restart`: none, agent reconnect, MCP reconnect, host reload, daemon restart, or unsupported/unknown, with affected components and operation reference.

Observed data is probe evidence, never a writable layer. Status cannot report effective from desired alone, and stale/unknown cache cannot become healthy. Foreign-owned files or registrations remain visible but immutable unless a separately authorized adoption workflow exists; `repair` changes only TraceDecay-owned state. Settings, CLI, HTTP/SDK, MCP, doctor, and the Integrations workspace render this same generated status/difference view.

### 13.5 Shared Brain node, placement, sync, and transport descriptors

Plan 28 supplies topology semantics; this registry owns desired configuration and provenance. Secrets remain credential references and node enrollment is a distinct application workflow, never a settings file edit.

| Key | Typed value/default | Contract |
|---|---|---|
| `brain.role` | `Standalone | Authority | RemoteClient | ReadReplica | Standby` / `Standalone` | Restart/store-open impact; role cannot self-promote or imply placement. |
| `brain.authority.endpoint` | protected authority reference / none | HTTPS/mTLS authority; credential-free display. Tailscale/MagicDNS is an optional URL profile, never required semantics. |
| `brain.transport.profile` | `DirectTls | LanTls | PrivateOverlay | ReverseProxy` / `DirectTls` | Reachability and proxy/authority pins only; cannot widen application grants or disable TLS/auth. |
| `brain.consistency.default` | `Authoritative | BoundedStale` / `Authoritative` | UI/query default only; explicit requests and authority-only commands remain stronger. `OfflineCache` is observed state, not desired truth. |
| `brain.cache.enabled/max_bytes/max_age` | false / bounded | Encrypted read-only cache; never authority/backup. Effective age is capped by signed grant `not_after`, policy/revocation generation, retention, and pending purge frontier; configuration cannot raise those floors. |
| `brain.sync.batch_bytes/backoff/spool_budget` | bounded values | Narrows plan-28 hard ceilings; cannot bypass local sanitize-before-spool, durable ack, or reserved non-content lane. |
| `brain.placements` | versioned map of shard/privacy domain to authority/replica policy | Saving validates a desired plan; `brain.placements.apply` performs the fenced resumable effect. No database path/URL. |
| `brain.privacy.sync_classes` | per-domain `NeverSync | MetadataOnly | SanitizedEncrypted | FullEligible` | Non-disableable floor; protected quarantine defaults `NeverSync`; relaxation requires fresh scan/activation. |
| `brain.replica/standby` | eligibility, lag/RPO/RTO bounds | Desired membership only; seeding/promotion requires signed manifests and recovery receipts. |
| `brain.failover.fence_provider` | `GracefulOnly | ExternalExclusiveResource | IndependentQuorumLease` / `GracefulOnly` | Declares how positive exclusivity is proven; endpoint/capability/credential refs are opaque. `None`, unreachability, wall-clock expiry, or operator assertion can never enable promotion. |
| `brain.recovery.key_provider` | offline recovery-bundle or external KMS/escrow reference / required before standby | Separately wraps retained data-key epochs; never stores unwrap secret in config/export/backup or weakens restore privacy scan. |

`BrainId`, `BrainNodeId`, node keys, authority epochs, memberships, revocations, observed topology, causal frontiers, and recovery receipts are not writable config values. `/settings/brain`, CLI, API/SDK, and optional operator MCP render the same desired/activated/observed/effective/drift/restart model. Local-only users see no remote warning merely because no endpoint exists.

### 13.6 Diagnostic storage descriptors

| Key | Typed value/default | Contract |
|---|---|---|
| `observability.logs.segment_max_bytes` | bounded bytes / 64 MiB | Rotation ceiling; a lower layer cannot raise the release hard ceiling. |
| `observability.logs.segment_max_age` | bounded duration / 1 day | Rotation age; crash recovery seals or safely resumes the current segment. |
| `observability.logs.retention` | bounded duration / 90 days | Applies atomically to event rows, safe-message blobs, and pre-store segments; legal/incident holds may extend, never shorten, it. |
| `observability.logs.total_quota_bytes` | bounded bytes / platform profile | Admission/GC budget with reserve; pressure reduces verbosity before deleting held or in-horizon evidence. |
| `observability.logs.default_version_view` | `All | CurrentRuntimeSet | CompatibleProtocol` / `CurrentRuntimeSet` | Presentation default only; responses disclose selected runtime/compatibility manifest and included/excluded/legacy-unknown coverage. |

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
    pub provider_kind: NativeKindCode,
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
- schedule/cadence and concurrency; cadence is an upper eligibility window, never permission to run unchanged input;
- registered trigger class, input contract, event/projection dependency channels, per-thread/project/profile dirty-scope policy, and bounded dependency-version reevaluation;
- finalized boundary, active-writer/coverage gate, quiet period, maximum debounce/dirty age, minimum eligible event/token delta, and high-value immediate triggers;
- indefinite identical-terminal-input suppression until relevant-frontier advance, terminal-`NoChange` frontier behavior, coalesced skip episodes, and self-effect loop suppression;
- source eligibility, evidence/quality/trust thresholds, and retention horizons;
- compute/token/time/cost budgets;
- model/provider/credential reference;
- sandbox/capability grants and repository-write boundaries;
- shared-operation retry/backoff/deadline/circuit, poison-input quarantine, uncertain-effect reconciliation, health pause, and incident behavior;
- evaluation corpus/version and promotion quality gates;
- notification/summary verbosity;
- audit retention and outcome measurement.

There are no “pending curation proposals,” Approve, Reject, Apply, or Roll Back controls. The autonomous workflow evaluates, validates, commits, supersedes, or retires under its active policy and writes a complete decision/effect receipt. Brain/Evolution surfaces show what happened, evidence class, policy/config digest, impact, quality, and failure state for investigation—not authorization after the fact.

Changing autonomy configuration applies to future workflow decisions at the next safe boundary. In-flight runs remain pinned to their starting digest or stop at a declared cancellation boundary; they do not mix generations. Re-evaluating historical material is a new autonomous run with a new manifest, not manual per-item replay/apply.

The generated defaults favor event-driven coalescing: session reflection becomes dirty only from new eligible thread activity and normally waits for a finalized terminal/quiet boundary; skill writing consumes new completed reflections, diagnostics/pattern evidence, or skill outcomes; memory curation consumes changed facts/relations/trust/feedback/conflicts/retention horizons. Dependency config/policy/catalog/model changes dirty only jobs whose registered semantic input contract changed and follow its reevaluation policy. Unknown/partial activity defers; no activity means no run. `run_now` shortens cadence for already-dirty scopes but cannot bypass identical successful/`NoChange` input suppression; unchanged/historical testing opens the hermetic lab. The UI and CLI show effective values, source, trigger/input contract, per-shard current/considered/consumed/included frontiers, quiescence/writer/coverage, dirty scopes, skip episode, semantic and evaluation digests, last-terminal input, retry/circuit/reconciliation state, and model/tool work avoided.

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
integrations: desired/activated/observed/effective package-component-registration state, ownership/trust, compatibility differences, cache freshness, drift, restart, and active operation refs
```

Drift detectors use registered safe observations:

- process environment differs from recorded bootstrap observation;
- host/provider config was modified outside TraceDecay;
- generated hook/skill/service files do not match the activation manifest;
- daemon/dashboard/agent session runs an older generation;
- store/index/projection manifest pins another config digest;
- registry/schema version differs across client/server;
- a credential reference is missing, expired, locked, or foreign-owned;
- a host-integration package/component/registration is missing, extra, stale, version/digest-incompatible, disabled, foreign-owned, untrusted, or awaiting reconnect/reload compared with the target's activated desired state;
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
- provider/host hook and MCP installation metadata: Claude `settings.json` hook/MCP entries; Codex user/repository `hooks.json`, inline `[hooks]` in `config.toml`, plugin default/manifest path-array/inline sources, system/session/cloud/MDM/`requirements.toml` managed sources, `[features].hooks`, deprecated `codex_hooks`, `allow_managed_hooks_only`, project trust, exact definition-hash review/disable state, and one-off bypass observation; Cursor hook configuration; and Kiro hook entries as foreign-observed state.

For each legacy input record source, owner, parser version, value classification, mapped key, target resolution, selected precedence, semantic difference, and import receipt. Secrets are converted to keyring references or quarantined; they never enter V2 layer history. Ambiguous ownership is `ImportUnresolved` and cannot become effective.

Run V1 and V2 resolution against a sanitized fixture corpus, compare effective values and operational behavior, explicitly accept intentional differences, then cut over one module at a time. Remove old readers, env-only code paths, direct dashboard mutations, provider-local defaults, and file watchers after parity. Stale clients receive typed registry/version guidance, not a live V1 fallback.

## 18. Replay and evaluation

Configuration replay does **not** create a fifteenth lab or `/playgrounds/configuration` route. Registry precedence/activation/policy-effect cases are a typed evaluator mode inside the existing Policy Diff Lab; scope-target resolution cases are a typed evaluator mode inside Scope/Federation Lab. Both use the generic experiment lifecycle and generated CLI/MCP/API equivalents. Settings links to a prefilled experiment draft, never a bespoke configuration runner.

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
- immutable preparation/release derivation under crash before/after owner pin, manifest publication, acknowledgement, release receipt, expiry, abandonment, and garbage collection; no released/expired pin can be mutated or collected while a manifest still references it;
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

Add host-profile fixtures for a core-only component set with no MCP dependency; explicit zero/one/many context/work/operator facade companions from the thin `tracedecay` integration binary connected to private `tracedecayd`; explicit headless facade-only deployment; target-scoped base/companion package selection; skills/roles/hooks/MCP component enablement; user/machine/managed install scopes; trust/update/credential-reference policy; immutable integration/profile/catalog/grant/budget digest verification; desired/activated/observed/effective separation; stale-cache/foreign-owner/version-digest/registration drift; every restart directive; allowlist/ceiling narrowing; immediate disable/revocation; pending reconnect on every widening; eager-all-tools, paginated-list, ignored-list-change, and deferred-tool-search clients. Hermes fixtures add zero/one/many named host profiles whose sealed runtime refs bind one `tracedecay_profile_id`, installed/configured owner derivation, misleading `HOME`/`HERMES_HOME`/CWD/provider helpers, registered repositories below a host home, and interleaved session resets; no config/request/provider field may select that profile ID or change the sealed data owner. Claude fixtures cover all source/frontmatter/component lifetimes, 30 event/type support cells, five handler kinds, matcher/`if`, sync/async/rewake, host dedupe, disable-all/managed-only/forced-plugin exemption, `/hooks` read-only state, and version gates without rendering protected definitions. Codex fixtures cover every additive source/layer, same-layer dual representation warning, trusted/untrusted repository, plugin default/manifest override forms, feature false/true/deprecated alias, requirements force/managed-only, managed immutable trust, exact-hash change/review/disable, ephemeral bypass, and unsupported handler state. Prove config save has no host effect, trust is never automated, managed state is never mutated, and every legal host effect has one integration operation receipt. No fixture may rely on protocol-guaranteed progressive disclosure.

### 21.4 UI and CLI

- Full keyboard/tree/search/edit/save/conflict/history/diff/import/export/status flows.
- Large registry virtualization and search latency.
- Mobile, narrow terminal, screen reader, high contrast, no color, reduced motion, localization expansion, offline/partial/locked states.
- Restart/rescan/reindex/migration progress and SSE reconnect/resync.
- Secret-reference non-rendering, URL/storage/log/clipboard scans, and synthetic canaries.
- Copy-command round trips between UI and CLI JSON.
- Redacted Codex hook inventory parity across Settings/CLI/API: source layer/representation, event/matcher behavior, definition digest, managed/project-trust/review/disable/effective/skip reason, overlap group, last run, and exact `/hooks` remediation; no command body or path renders.
- Redacted Claude hook inventory parity across Settings/CLI/API: source/component lifetime, event/matcher/`if`, handler kind, sync/async/rewake, support/version, disable/managed state, host dedupe, run/completion/delivery coverage, and owning-source edit guidance; no command/args, URL/header, MCP input, prompt/agent body, path, environment, or `/hooks` sensitive detail renders.

### 21.5 Autonomous curation

- No generated capability, route, form, CLI command, or MCP tool exposes item approve/reject/apply/rollback.
- Runs pin one policy/config digest and cross generation only at a safe boundary.
- Policy/budget/schedule changes affect future autonomous runs deterministically.
- Failures pause/retry/circuit-break according to policy and remain observable.
- Secret/quarantine/floor tests prove autonomous curation cannot promote unsafe content.
- Outcome and Evolution views are read/audit surfaces, not hidden authorization paths.

### 21.6 Fault and scale

- Thousands of keys/targets, hundreds of concurrent readers, many simultaneous agent writers, slow consumers, daemon restart, store lock, shard unavailability, registry upgrade, clock skew, and disk-full faults.
- Edit-bundle tests cover every configurable bound and hard ceiling, stream cancellation, ordinary failed-validation retention, secret/unknown immediate purge, success/delete/expiry/revocation cleanup, startup/periodic sweep, `0700`/`0600`, and proof that no config/request/receipt can carry the managed runtime path.
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
cargo test config_conformance
cargo nextest run --workspace --no-fail-fast
(cd dashboard && npm test -- settings)
(cd dashboard && npx playwright test settings policy-diff scope-federation)
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

- Add profile/project repositories, audit/outbox, immutable preparations/releases, staged revisions, atomic activation publication, consumer acknowledgements, and fault tests.
- Store credential references only.

### PR 22C — Generated configuration registry and capability inventory

- Collect owning-crate manifests.
- Generate catalog, schemas, docs, CLI/MCP/HTTP/SDK/dashboard metadata.
- Register all nine `task_graph.steering.*` lowering descriptors against Plan 08 `SteeringLimitsV1`; reject a widened maximum, shortened cooldown, incomplete snapshot, or conflicting unit/default.
- Add full legacy/public-setting inventory and drift gates.

### PR 24I — Application resolver, commands, API, CLI, MCP, and SDKs

- Implement resolve/explain/validate/impact/patch/batch/history/import/export/status/drift use cases.
- Ship navigable CLI tree and deterministic JSON/JSONL.
- Add transport parity and configuration SSE.
- Implement steering hot activation/re-resolution at admission and pre-handoff claim boundaries, including `BlockedByLimitChange`, required-fence preservation, and supersede/cancel remediation receipts.
- Add task-graph edit-bundle bound descriptors plus the target-scoped host-integration desired package/component/install-scope/trust/update/credential descriptors and three generated one-binary MCP registration profiles; profile widening remains pending until reconnect, host effects run only through plan 09 operations, and every host/profile receipt is content/path-free.

### PR 25E — Complete Brain Settings workspace

- Replace partial settings/plugins with the generated profile-wide workspace.
- Add target tree, search/forms, provenance, impact, conflicts, history, status, drift, credential references, and links into plan 11's `/settings/integrations` topology/difference/operation workspace.
- Add the generated Steering limits panel with absolute/effective/source/counter/blocked state and protected-payload-free remediation actions.
- Keep all old write behavior until module parity passes, then remove old bindings atomically.

### PR 31N — Configuration and autonomy replay extensions

- Extend the existing Policy Diff and Scope/Federation evaluators with historical/current resolution, impact, consumer, hint/search/privacy/autonomy comparisons; do not add another `LabKindV1`, route, lifecycle, or scheduler.
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
- [ ] All nine steering descriptors are lowering-only against Plan 08, hot activation is deterministic before/after handoff, and an already-admitted required directive that conflicts with lowering becomes explicitly limit-blocked until superseded/cancelled—never waived, truncated, or delivered above the effective bound.
- [ ] Configuration SSE, status, doctor, and Settings agree under slow clients, restarts, stale clients, split identity, locked stores, and partial shards.
- [ ] Import/export is typed, scoped, versioned, sanitized, non-secret, and atomic at activation; V1 inputs have complete migration/differential receipts.
- [ ] Policy Diff and Scope/Federation evaluator modes replay historical/current configuration resolution and policy effects without mutation or unsafe fixture access; no Configuration lab kind, route, runner, or lifecycle exists.
- [ ] Registry generation leaves a clean tree and parity tests cover CLI, MCP, HTTP, SDKs, dashboard, hooks, daemons, automations, and extensions.
- [ ] Legacy live config readers, duplicate defaults, transport-local settings, env-only controls, and fallback paths are deleted after verified cutover.
- [ ] Full workspace, dashboard, fault, accessibility, performance, privacy, and secret-scan gates pass.

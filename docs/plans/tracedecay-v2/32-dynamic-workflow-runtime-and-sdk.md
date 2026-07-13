# TraceDecay V2 Dynamic Workflow Runtime and SDK

**Status:** new bounded crate and cross-cutting product capability. This plan is the implementation authority for `tracedecay-workflow`, the native dynamic-workflow definition/run model, deterministic replay kernel, JavaScript/TypeScript authoring surface, workflow-specific application use cases, and Workflow Studio product requirements. Plans 01/02/08/09/10/11/12/17/19/20/21/24/26/27/28 remain authoritative for their shared domain, storage, catalog, application, transport, UI, root, SDK, convergence, configuration, taskgraph, accounting, host-bundle, and remote boundaries.

**Decision:** TraceDecay will support readable, reusable, code-defined workflows analogous in ergonomics to Claude Code dynamic workflows, but with stronger durable-execution and provenance semantics. JavaScript is an authoring surface over one canonical `WorkflowDefinitionV1`/`WorkflowGraphV1`/history model. It is not the storage schema, scheduler, executor, authorization system, taskgraph, or source of completion truth.

**Taskgraph relationship:** ordinary workflow definitions and runs are first-class workflow entities, not hidden boards or `WorkItemV1` rows. They share the daemon, `SchedulerKernelV1`, generic operations/steps, executor registrations/adapters, model/tool catalog, event/outbox path, accounting, and graph/query surfaces with task execution. An explicit `workflows.task_graph.compile_candidate` compiler may produce a candidate `PlanVersionV1` from an eligible bounded workflow. It never silently creates or activates tasks. A task may invoke one workflow run as a bounded execution step; workflow/task nesting is cycle-checked and provenance-linked.

---

## 0. Contract lock

1. `tracedecay-workflow` is a pure deterministic domain kernel. JavaScript engines, TypeScript compilers, stores, schedulers, executors, providers, transports, and UI remain outside it behind ports.
2. JavaScript and TypeScript are the only user-authored workflow languages, not durable state. The root-owned canonical compiler is the only implementation that produces `WorkflowSourceArtifactV1`; daemon/API compilation is authoritative, and any locally distributed validator/compiler component is the exact same release artifact behind a generated ABI adapter, never an SDK reimplementation. The engine executes only the immutable artifact; TypeScript is never interpreted directly.
3. The canonical command tape plus addressed result/effect history is run truth. Operation rows, projections, checkpoints, logs, transcripts, engine bytecode, and browser graph state are evidence or acceleration, never an alternate history.
4. Workflow work reuses the one scheduler, operation/step kernel, executor registry, execution-unit envelope, event/outbox, idempotency, effect reconciliation, retention, query, and subscription mechanisms. A workflow-local clone of any of them fails architecture review.
5. Ordinary workflow nodes are not task work items. Taskgraph compilation emits a candidate only, and the existing plan-24 review/activation commands remain the sole task authority.
6. External effects are not “exactly once.” TraceDecay guarantees exactly-once command admission and receipt identity; adapters declare idempotent, at-least-once, or non-repeatable effect semantics. Unknown delivery/effect state blocks automatic retry and terminal success.
7. Engine promotion selects an `{engine, version, features, placement, compiler ABI}` profile from TraceDecay measurements. No engine, wrapper, native feature set, allocator mode, or in-process placement is assumed safe before the fault/conformance gate.
8. The workflow realm has zero ambient I/O and zero ambient nondeterminism. Only SDK primitives can yield commands, recorded time/IDs, or addressed results. No host object, import resolver, environment, network, filesystem, shell, process, worker, timer, random source, locale, timezone, or mutable global enters replay.
9. JSON Schema 2020-12 is the only persisted data-contract dialect. A closed TraceDecay meta-schema/vocabulary profile, immutable bundled references, canonical value codec, and validator manifest make validation portable and replayable.
10. CLI, HTTP/SSE, public SDKs, compact MCP, plugins, and Workflow Studio are generated bindings/views over the same application use cases. A transport may add framing, never semantics or hidden workflow execution.
11. Structured steering uses the canonical live-steering protocol at safe host boundaries. Plain comments, notifications, hints, and scout suggestions cannot become directives implicitly or clear required completion fences.
12. Every definition/run/node/history/error/log/diagnostic record includes the TraceDecay build, workflow IR/schema version, compiler manifest, engine ABI, and adapter capability digest needed to filter or replay version-specific behavior.

These locks are acceptance criteria, not aspirations. Any implementation slice that cannot prove them remains a candidate and cannot cut over.

## 1. Product outcome

TraceDecay users and agents can:

- describe orchestration in a small JavaScript/TypeScript SDK using `meta`, global `args`, `phase()`, `agent()`, `parallel()`, `pipeline()`, `log()`, and a returned result;
- validate, save, version, diff, run, watch, pause, resume, retry, cancel, fork, and compare workflows through the daemon;
- fan work across registered Codex, Claude Code, Cursor, Hermes, or future executor routes without embedding provider logic in the script;
- require JSON-Schema-validated outputs and pass typed results between steps;
- survive daemon/host/process restarts by replaying deterministic workflow history rather than reconstructing intent from logs;
- inspect every phase, node, prompt-safe input, output, transcript, tool/artifact anchor, model/effort route, cost, token count, latency, cache decision, retry, failure, and coverage state in Workflow Studio;
- reuse a workflow as a CLI command, compact MCP capability, public SDK call, plugin command/skill, task step, automation recipe, or experiment fixture without creating parallel implementations;
- explicitly compile a suitable workflow into a reviewable taskgraph candidate when durable ticket ownership, human planning, or multi-day project execution is desired.

The capability must feel as lightweight as Claude's generated workflow scripts for one-off reviews and migrations, while retaining TraceDecay's exact scope, retrieval anchors, many-host awareness, live steering, replay, accounting, and cross-project graph.

## 2. Non-goals

V2 does not:

- implement a second scheduler, worker fleet, lease kernel, event journal, outbox, operation lifecycle, model gateway, tool registry, task database, or dashboard data model;
- treat every workflow node as a task card or every task DAG as executable JavaScript;
- run workflow JavaScript in the browser, hook process, MCP client, plugin host, or arbitrary agent checkout;
- expose filesystem, shell, network, environment, wall clock, unseeded randomness, native modules, dynamic imports, package installation, `eval`, or host objects to workflow code;
- let a script bypass catalog capability grants, scope resolution, privacy/sanitizer policy, budgets, model routing, executor registration, or effect receipts;
- infer completion from a provider response, child process exit, workflow script return, or cached text without canonical history and acceptance evidence;
- promise source-compatible execution of arbitrary npm packages or full Node/Deno/Web APIs;
- mutate an in-flight definition version or silently replay a running history against changed code;
- make cross-run model-output reuse the default or label memoized output as freshly executed;
- use hidden chain-of-thought as workflow input, output, state, or evidence.

## 3. Primary research and adopted lessons

Sources were retrieved on 2026-07-12. Implementation research must refresh versions and record exact source revisions before selecting dependencies.

| Source | Observed capability | TraceDecay adoption/disposition |
|---|---|---|
| [Claude Code dynamic workflows](https://code.claude.com/docs/en/workflows) | JavaScript orchestration, `agent`/`parallel`/`pipeline`/`phase`, background progress, saved `.claude/workflows`, global `args`, up to 16 concurrent and 1,000 total agents, no direct script filesystem/shell, no ordinary mid-run input, same-session-only resume, and current large-run warnings | Adopt the ergonomic primitives, background UX, readable scripts, structured args, phase/run inspector, strict script/effect separation, and explicit size visibility. Replace same-session cache resume and no-input limitation with durable command replay plus separately typed signals/steering; never import Claude permission or auto-edit semantics. |
| [Claude Agent SDK Workflow API](https://code.claude.com/docs/en/agent-sdk/typescript#workflow) | Agent SDK 0.3.149+ accepts `script | name | scriptPath | args | resumeFromRunId`; it returns background task/run/transcript/script refs, and syntax failure can still carry `status:"async_launched"` plus `error` | Keep familiar start/fork ergonomics but use disjoint typed commands. HTTP/MCP never accept a server path; CLI reads local bytes. Validation failure creates no run/operation. Resume controls the same durable run, while fork is a new run with explicit reuse receipts. |
| [Temporal workflow determinism](https://docs.temporal.io/workflow-definition) | Workflow code replays; commands must match history; I/O/LLM/database work belongs in activities; incompatible code changes require versioning | Adopt command/history replay, deterministic orchestrator constraints, immutable definition versions, nondeterminism detection, and effect separation. Do not introduce Temporal as a service or runtime dependency. |
| [Azure Durable orchestrator constraints](https://learn.microsoft.com/en-us/azure/durable-task/common/durable-task-code-constraints) | Event-sourced replay forbids ambient time/random/environment/network/I/O, arbitrary async work, threads, and replay-duplicated logging; its JavaScript guidance specifically rejects ordinary `async` orchestrators | Remove those capabilities and treat TraceDecay's top-level `await` as an evidence burden: only SDK-owned replay thenables and a host-driven deterministic job queue are permitted. General `Promise`, custom thenables, timers, and microtask APIs remain absent unless the 1,000-replay conformance gate proves an explicitly versioned subset. |
| [JSON Schema 2020-12 Core](https://json-schema.org/draft/2020-12/json-schema-core) | Dialect/vocabulary declarations, URI-based resources/references, dynamic references, and extensible keywords require explicit processor behavior | Publish a closed TraceDecay meta-schema using the 2020-12 dialect; reject unknown required vocabularies, remote resolution, and unsupported dynamic semantics. TypeScript helpers may compile richer authoring types only when they emit the exact bundled schema graph and digest. |
| [Boa 0.21.1](https://docs.rs/boa_engine/latest/boa_engine/) | Pure-Rust, explicitly experimental ECMAScript engine with parser/bytecompiler/VM and runtime-limit APIs | Benchmark as one candidate. Do not infer complete ECMAScript support, heap containment, async determinism, panic isolation, or production fitness from language or implementation choice. |
| [QuickJS C API](https://bellard.org/quickjs/quickjs.html), [rquickjs 0.12.1](https://docs.rs/rquickjs/latest/rquickjs/struct.AsyncRuntime.html), and [QuickJS-NG](https://github.com/quickjs-ng/quickjs) | QuickJS exposes runtime memory/stack limits and interrupt callbacks; bytecode is engine-version-bound and unsafe as untrusted durable input. rquickjs exposes interrupt/job/promise hooks and limits, but its documented memory limit is a no-op under custom/Rust allocator features. QuickJS-NG is an independently evolving fork. | Benchmark exact upstream/wrapper/feature/allocator combinations, not “QuickJS” generically. Persist source artifacts, never engine bytecode. Verify memory limits under the selected allocator, deterministic job draining, unwind/crash behavior, native packaging, and upstream provenance before promotion. |

The inspected local Claude-generated PR-review workflow provides the principal usability fixture: `Find` fans ten independent review angles, `Verify` pipelines candidates through independent reviewers, `Sweep` searches gaps, and `Rank` emits one schema-validated result. TraceDecay reproduces this shape without depending on Claude's private runtime.

## 4. Canonical ownership and crate boundary

### 4.1 New crate

Add `crates/tracedecay-workflow`.

It owns only:

- workflow-specific immutable IR and validation rules built from plan-01 canonical IDs/references;
- deterministic validation/normalization of compiler-produced source artifacts and callsite manifests; language parsing/transpilation and engine execution remain root adapters;
- the replay state machine that compares requested primitive commands with recorded history;
- stable call-path and dynamic-node identity derivation;
- pure readiness, fan-out/fan-in, phase, retry, cache-eligibility, and nondeterminism decisions;
- engine-neutral `WorkflowProgramPort`, `WorkflowHistoryPort`, `WorkflowActivityRequestV1`, and addressed result/receipt value contracts; the engine adapter implements the program port outside this crate;
- pure taskgraph eligibility and candidate compilation, never activation;
- deterministic Markdown/diagnostic source maps for script validation.

Allowed dependencies are `tracedecay-domain` plus repository-standard serialization, JSON-Schema, hashing, error, and bounded-collection crates. It cannot depend on store, projectors, query, policy, hooks, tool catalog, application, API, root, Axum, SQLite, Git, TypeScript/JavaScript parser or engine packages, model/provider SDKs, or dashboard code. The crate accepts only normalized IR/value contracts; source bytes cross its boundary through a compiler receipt, never an engine object or AST owned by a third-party crate.

### 4.2 Existing owners

| Concern | Owner | Workflow integration |
|---|---|---|
| IDs, entity refs, declared scope, actors, provenance, anchors, event/value types | plan 01 | Adds workflow definition/version/run/phase/node/call identities and legal workflow↔task/operation/agent relations. |
| Physical activity storage, blobs, event/outbox/idempotency, backup/restore | plan 02 | Stores workflow extensions through existing canonical activity and generic operation families; no workflow database or journal. |
| Projections and graph links | plan 04 | Builds run/node/phase/task/transcript/artifact/agent/time/cost projections from canonical events. |
| Search, graph, timeline, compare, as-of | plan 05 | Queries workflow entities through `TraceQueryV1`; no workflow query engine. |
| Pure routing/retry/fairness/cache policy | plan 06 | Evaluates registered workflow policies; does not execute scripts. |
| Hooks and safe host boundaries | plan 07 | Captures/delivers lifecycle and structured steering events; never runs the orchestrator. |
| Capability and generated binding catalog | plan 08 | Registers workflow use cases, executor/engine capabilities, schemas, effect/cost/privacy metadata, and discovery. |
| Commands, auth, operations, scheduler, executor, history transactions | plan 09 | Runs workflow application use cases over the shared scheduler/operation kernel. |
| HTTP/SSE/OpenAPI/generated TS core | plan 10 | Thin bindings over plan-09 use cases and views. |
| Workflow Studio and Run Graph | plan 11 | Renders generated views; never evaluates JavaScript or decides readiness/cache/task eligibility. |
| Source compiler and engine adapters, placement/supervision, ephemeral compile cache, migration | plan 12/root | Sole JavaScript/TypeScript parser/compiler and JavaScript-engine dependencies. In-process or supervised-helper placement is an adapter choice; neither placement owns workflow state. |
| Public Rust/TS/Python SDKs and authoring package docs | plan 17 | Generates direct API clients and `@tracedecay/workflow`. |
| Configuration | plan 20 | Owns engine/runtime/budget/cache/concurrency/UI settings and four-axis state. |
| CLI/MCP/presentation | plan 21 | Generated commands, compact MCP profile/resources, Markdown/JSON parity, progress/cancellation. |
| Taskgraph target schema/review/activation and executor attempts | plan 24 | Owns `PlanVersionV1`/`WorkItemVersionV1`, review/edit/activation, executor registration/adapters, and workflow↔task links. Plan 32 exclusively owns workflow eligibility, loss semantics, and candidate compilation implementation; no implicit conversion. |
| Accounting/SLOs | plan 26 | Owns workflow usage, token, cost, cache, queue, replay, latency, and failure measurements. |
| Host bundles | plan 27 | Projects workflow commands/skills/hooks consistently to Codex, Claude Code, Cursor, and Hermes. |
| Remote machines | plan 28 | Routes shared-Brain workflow authority and remote executor work under existing shard/lease rules. |

`architecture-boundaries.toml` rejects imports that would put the selected JavaScript engine, Axum, SQLite, host/provider SDKs, or tool execution inside `tracedecay-workflow`.

## 5. Domain and IR

### 5.1 Definition identity

```rust
pub struct WorkflowDefinitionV1 {
    pub id: WorkflowDefinitionId,
    pub name: WorkflowName,
    pub active_version: Option<WorkflowDefinitionVersionId>,
    pub declared_scope: DeclaredScope,
    pub owner: ActorRefV1,
}

pub struct WorkflowDefinitionVersionV1 {
    pub id: WorkflowDefinitionVersionId,
    pub definition_id: WorkflowDefinitionId,
    pub version: u64,
    pub source_artifact: WorkflowSourceArtifactRefV1,
    pub meta: WorkflowMetaV1,
    pub input_schema: JsonSchemaRefV1,
    pub output_schema: JsonSchemaRefV1,
    pub engine_abi: WorkflowEngineAbiV1,
    pub catalog_snapshot: CatalogSnapshotRefV1,
    pub config_snapshot: ConfigSnapshotRefV1,
    pub created_by: ActorRefV1,
    pub created_at: UtcMicros,
}
```

Versions are immutable. Saving modified source creates a new version. A run is permanently pinned to one definition version, engine ABI, the source artifact's compiler/SDK/schema manifests, input digest, catalog/config/policy/privacy snapshots, and declared scope. Activating a new version affects new runs only.

`WorkflowSourceArtifactV1` is the lossless definition payload:

```rust
pub struct WorkflowSourceArtifactV1 {
    pub source_language: WorkflowSourceLanguageV1, // JavaScript | TypeScript
    pub original_source: ProtectedBlobRefV1,
    pub original_source_digest: SanitizedOutputDigest,
    pub executable_module: ProtectedBlobRefV1,    // canonical UTF-8 JavaScript source, never bytecode
    pub executable_digest: SanitizedOutputDigest,
    pub source_map: WorkflowSourceMapV1,
    pub normalized_callsite_manifest: WorkflowCallsiteManifestV1,
    pub schema_bundle: WorkflowSchemaBundleRefV1,
    pub compiler_manifest: WorkflowCompilerManifestRefV1,
    pub sdk_abi: WorkflowSdkAbiV1,
    pub artifact_digest: ManifestDigest,
}
```

JavaScript still passes the pinned parser/normalizer and produces an artifact; TypeScript is transpiled ahead of time with types erased and source spans mapped back to the original. The root composition owns one canonical compiler implementation and release component. Daemon/API import, validation, and version creation call it in process or through its root-supervised framed ABI. `@tracedecay/workflow`, generated SDKs, CLI, plugins, and Studio contain no parser, transpiler, normalizer, callsite allocator, schema emitter, or artifact signer. They call the daemon, or local tools may invoke only the exact compiler component shipped by the same TraceDecay release after verifying component digest and compiler/SDK/schema ABI. Local output is advisory and cannot publish a definition version; the daemon recompiles the original bytes and its receipt/artifact digest is authoritative. Version/component mismatch returns `workflow_compiler_abi_mismatch` rather than falling back or rebuilding differently.

The compiler permits one virtual SDK binding, rewrites Claude-compatible globals to that binding when requested, bundles no arbitrary dependency, and emits one self-contained module. The only admitted generated host code is the exact synthetic prefix/suffix wrapper emitted by the canonical compiler, owned by the pinned compiler ABI, covered by the executable/source-map/callsite digests, and reproducible by its golden suite. Author-supplied or external-tool-generated host code, `import`, dynamic import, package resolution, declaration-file execution, decorators/plugins/macros, compiler callbacks, and any other generated shim are rejected. A future compiler ABI may define a different synthetic wrapper only as a new immutable artifact contract; it cannot broaden an existing artifact during replay. Engine bytecode, heap snapshots, native pointers, and third-party AST serialization are ephemeral cache material and cannot enter a definition/history/backup/export.

### 5.1A JSON value and schema contract

Every `args`, primitive input, checkpoint, signal, node result, and workflow result is a canonical JSON value: null/boolean/string/finite JSON number/array/object with unique object keys. `undefined`, bigint, symbol, function, promise/thenable, typed array, date, regexp, map/set, cyclic object, sparse array holes, accessors/proxies, nonfinite number, negative zero where canonicalization loses it, custom prototype, and host object fail conversion with an exact source/value path. Conversion snapshots the value once; getters or coercion hooks never run during hashing.

`WorkflowSchemaBundleV1` declares `https://json-schema.org/draft/2020-12/schema`, the exact TraceDecay meta-schema digest, understood vocabularies, immutable resource URNs/digests, root input/output/node schema IDs, validator implementation/version/options, format-assertion registry, regex/Unicode version, and normalized bundle digest. V1 supports the reviewed Core, Applicator, Validation, Unevaluated, Meta-Data, Content, and Format-Annotation/selected Format-Assertion profile only where conformance fixtures exist. Unknown required vocabularies/keywords reject; annotations cannot secretly enforce validation. `$ref` resolves only within the uploaded immutable bundle by canonical resource URI. Network/filesystem resolution, mutable URLs, `$dynamicRef`/`$dynamicAnchor`, unbounded recursive expansion, implementation-defined base URIs, and runtime schema loading are rejected in V1. `default` is annotation only and never mutates inputs.

Schema admission enforces depth/node/ref-cycle/regex/string/enum/required/property/item/diagnostic caps before a run exists. The same canonical bundle and validator manifest are used by daemon, local authoring validation, SDK fixtures, API schemas, replay, cache eligibility, and Studio. A client-side pass is advisory; the daemon's pinned validation receipt is authoritative.

### 5.2 Canonical workflow graph

`WorkflowGraphV1` is an incrementally discovered execution graph, not necessarily a fully static DAG at definition time. It contains:

- definition version and run identity;
- phase declarations/order and current phase;
- primitive command nodes (`Agent`, `ParallelGroup`, `PipelineMap`, `ChildWorkflow`, `Timer`, `SignalWait`, `Checkpoint`, `ContinueAsNew`, `Log`, `Return`);
- explicit data/control edges, fan-out group, stable item key, parent call path, and output-schema reference;
- lifecycle, retry, route, budget, cache, and effect metadata;
- exact source span/callsite, dynamic path, input/output/history digests, provenance, anchors, and coverage;
- links to generic operation/step, agent/session/thread/Turn/tool/artifact/cost entities;
- optional workflow↔taskgraph candidate-compilation refs.

The graph is rebuilt from the canonical run history and version pins. A browser layout, script AST, or agent transcript is never graph authority.

### 5.3 Stable primitive call identity

Every replay-visible primitive produces `WorkflowCommandV1` with:

```text
definition_version
engine_abi + compiler_manifest
command_tape_sequence
normalized lexical callsite id + source span digest
parent dynamic call path
phase id
primitive kind
pipeline/parallel stable item key and ordinal
canonical input/schema digest
```

Identity and cache eligibility are separate. Route/model/config/tool/context changes do not rewrite a historical call ID, but they change the effect signature and therefore prevent unsafe cross-run reuse. A `pipeline()` requires a deterministic unique item key. Defaulting to array position is legal only when the input manifest is frozen and order is part of its digest. Duplicate/unstable keys reject before scheduling.

The command tape is deterministic; result arrival is not required to be. `parallel`/`pipeline` enumerate factories synchronously in stable key order, append the complete ready command batch before any effect is dispatched, and return results in declared item order. Each effect result addresses its command/call ID and attempt; provider completion order is retained as evidence but never changes JavaScript-visible ordering. Replay compares the next tape sequence, call identity, primitive kind, normalized input/schema digest, and dependency-result digests. A call-path hash alone cannot excuse reordered, inserted, or omitted commands.

Dynamic identity is a path of `(callsite, stable item key, local occurrence)`, never an engine object address, array iterator timing, promise completion order, or source line alone. Loops must have a statically proven hard bound or iterate a frozen bounded value whose digest is pinned. Conditional branches may depend only on `args`, constants, or recorded primitive results. Enumeration/prototype order, locale collation, regex engine drift, floating-point edge cases, and map/set iteration cannot decide command order; the compiler either normalizes the construct into canonical JSON/key ordering or rejects it.

## 6. JavaScript/TypeScript authoring surface

### 6.1 File shape

```javascript
export const meta = {
  name: "review-branch",
  description: "Find, verify, sweep, and rank branch findings",
  inputSchema: { type: "object", required: ["base", "head"] },
  outputSchema: { type: "object", required: ["findings"] },
  phases: ["Find", "Verify", "Sweep", "Rank"],
}

phase("Find")
const candidates = await parallel(angles.map(angle => () =>
  agent(`Review ${args.head} from ${angle}`, {
    label: angle,
    schema: CandidateSchema,
    model: "route:review-high",
    effort: "high",
  })
))

phase("Verify")
const verified = await pipeline(candidates.flat(), candidate =>
  agent(`Verify this candidate: ${JSON.stringify(candidate)}`, {
    key: candidate.id,
    schema: VerdictSchema,
  }),
  { key: candidate => candidate.id, concurrency: 8 },
)

phase("Sweep")
const gaps = await agent("Find only issues missed above", { schema: CandidateListSchema })

phase("Rank")
return agent("Deduplicate and rank the verified set", {
  input: { verified, gaps },
  schema: FinalReportSchema,
})
```

The example is TraceDecay's restricted workflow-body authoring form, not a directly executable ECMAScript module: one top-level `export const meta = <static literal>` precedes a body where top-level `await` and `return` are allowed. Compiler ABI `tdwf-js-wrapper-v1` performs one frozen lowering before ordinary ECMAScript parsing:

```javascript
export const meta = /* canonicalized admitted meta literal */;
export async function __tracedecay_workflow_v1(__runtime, __args) {
  const { phase, agent, parallel, pipeline, childWorkflow, checkpoint,
          waitForSignal, continueAsNew, sleep, log, workflow } = __runtime;
  const args = __args;
  /* exact author body; terminal `return expr` is lowered to `return await expr` */
}
```

The lexical splitter accepts exactly one static `meta` export and rejects every other top-level import/export/declaration outside the body grammar. The generated entrypoint name, parameter order, helper destructuring, strictness, terminal-return rewrite, newline normalization, and synthetic prefix/suffix bytes are compiler-ABI inputs. Source maps mark wrapper bytes synthetic and map every body/meta token and diagnostic back to original UTF-8 byte/line/column spans; normalized callsite IDs derive from original spans plus compiler ABI, never generated line numbers. The wrapper-generated async function and its host-observed completion promise are the sole async-function/Promise exception. User-authored async functions and Promise APIs remain rejected. Changing this lowering requires a new compiler/SDK ABI and immutable definition version; replay never recompiles old history with a newer wrapper.

### 6.2 Canonical helpers

- `phase(name)`: selects a declared presentation/execution phase and records one replay-safe phase transition. It is not an authorization or transaction boundary.
- `agent(prompt, options)`: requests one registered agent activity. Options include label, JSON Schema, route/model capability, effort, tools, skills, context packet/anchor inputs, timeout, retry policy, effect class, cache policy, and phase. Application revalidates every option.
- `parallel(factories, options?)`: schedules independent factories through shared capacity/fairness limits and returns results in declared order. Failure policy is explicit `all | collect | threshold | fail_fast`.
- `pipeline(items, mapper, options)`: bounded keyed map over items; optionally follows with a reducer. Stable unique keys are mandatory for mutable/unordered inputs.
- `childWorkflow(definition, args, options)`: starts a version-pinned child run with cycle/depth/budget checks.
- `checkpoint(label, value?)`: records bounded JSON state and anchors; it cannot publish effects or claim acceptance.
- `waitForSignal(name, schema, options)`: suspends at an explicit deterministic command until one addressed, authorized, schema-valid signal is appended to history or a recorded timer wins. It is planned workflow input, not a comment, hint, notification, or steering directive.
- `continueAsNew(args, options?)`: terminally seals the current run generation and atomically proposes a successor pinned to an explicit definition version, carrying only schema-valid args and declared anchors. It is the bounded-history escape hatch, not history deletion or an in-place reset.
- `sleep(duration)` and `workflow.now()`: use recorded workflow time/timers. `Date`, `Date.now`, timer globals, and wall-clock APIs are absent.
- `workflow.uuid(label?)`: derives a replay-stable ID from run/call path; random globals are absent.
- `log(message, fields?)`: records one deduplicated replay-aware structured progress event; replay does not duplicate logs.
- top-level `return`: validates against the declared output schema and proposes completion. Application still requires history closure and terminal receipts.

`args` is the validated canonical input value. The realm has a frozen standard-library allowlist and no `require`, imports beyond the bundled SDK, dynamic import, WebAssembly, `eval`, `Function`, host reflection, environment, filesystem, network, shell, process, worker thread, or native extension.

Top-level `await` is syntax over TraceDecay-controlled replay suspension, not permission to use the engine as a general async runtime. SDK primitives return branded host thenables that can resolve only from the next matching history result. The compiler/runtime reject `Promise` construction/combinators, `.then/.catch/.finally`, custom thenables, `queueMicrotask`, `setTimeout`, `setInterval`, async generators, detached async functions, and pending jobs not attributable to an SDK primitive. At each replay turn the adapter drains the pinned engine job queue one job at a time to a declared quiescence/suspension point and records the job/tape digest. Compiler ABI `tdwf-js-wrapper-v1` ships only if both engine candidates reproduce the same command tape and output across the pinned async corpus; failure blocks that ABI from release. V1 never switches, retries, or falls back to generator/continuation lowering. Any future generator/continuation design is a separately named compiler ABI with a new immutable source artifact, engine contract, conformance/golden suite, and explicit new definition version; an existing run remains pinned to its original executable artifact and can only report incompatibility, never recompile or fall back.

Factory callbacks passed to `parallel` and `pipeline` are invoked by the SDK enumerator, never concurrently inside the JavaScript realm. Concurrency begins only after their command batch is durable. Closures may capture canonical JSON values, but not engine handles or unresolved SDK thenables from another branch. A reduction runs in stable declared order; associative/commutative claims are optimization metadata and never change replay order.

### 6.3 Structured outputs

JSON Schema is the persisted wire contract. Agent results are not available to script code until schema validation passes or the declared bounded repair policy terminates. TypeScript helpers can infer types and may accept a Zod-like authoring schema only when the build step emits the exact supported JSON Schema and digest; TraceDecay stores and validates the emitted schema, not executable validator code.

## 7. Deterministic execution and durable replay

### 7.1 Event-history algorithm

1. Application pins a validated immutable definition/source/schema/compiler/engine/config/catalog/policy/privacy manifest, canonical args digest, owner shard, generic operation, and `WorkflowRunV1` authority epoch in one admission transaction.
2. The root adapter instantiates a fresh bounded realm and evaluates the executable source from the beginning with the same canonical `args` and a read-only history slice.
3. Each SDK suspension produces one pure `WorkflowCommandV1`; `parallel`/`pipeline` can produce a deterministic ordered command batch.
4. The replay kernel compares the command tape sequence and complete identity/signature with the next expected scheduled-command history record. Completed addressed results are returned; pending commands suspend. Result arrival events may be out of order but cannot reorder the tape or output array.
5. A new command/batch is revalidated by application against the pinned run manifest plus current revocation/budget/authority state, then its history records, operation-step extension, idempotency/effect reservation, audit, and canonical outbox rows commit atomically before dispatch.
6. Shared scheduler/executor delivery uses the committed command/effect ID. Result, validation/repair attempt, external-effect receipt, and terminal node transition commit idempotently; an unknown delivery/effect never becomes an absent command or an automatic replacement.
7. A mismatched kind/order/call key/input/schema/dependency digest yields `WorkflowNondeterminismV1` with expected/actual tape positions and source spans. No new effect is admitted; the run becomes explicit incompatible-history evidence.
8. At suspension, the adapter must have zero unowned pending jobs/futures and returns a quiescence receipt. At return, application proves every scheduled command is terminal or explicitly detached, validates the final canonical value, resolves required steering/signals/effects, and commits workflow plus generic-operation terminal receipts atomically.

History replay is not an optimization cache. It is the run's execution truth. A daemon restart may repeat pure script evaluation but cannot repeat a committed agent/tool/model effect.

Replay snapshots are derived accelerators only. A snapshot binds a verified history-prefix sequence/digest, definition/source/compiler/engine ABI, canonical command/result lookup state, and next tape position; restore must replay/verify a configured overlap and can always discard the snapshot and replay from event zero. V1 still evaluates source from the beginning—no opaque JavaScript continuation is serialized. Snapshots never contain engine-native bytecode/pointers, replace history, authorize an effect, or cross an ABI. When history reaches the configured hard cap, the script must `continueAsNew` or fail `HistoryLimitExceeded`; the daemon never truncates an active run behind its ID.

### 7.2 Code changes

- Resume requires the exact definition version and engine/compiler ABI.
- Editing source always creates a new definition version.
- `fork_from_run` starts a new run and may import eligible prior results with explicit reuse receipts; it never splices changed commands into the old run history.
- A future migration/patch API must be versioned, reviewed, and prove command compatibility through replay fixtures before it can resume an old run under new code. V2 ships fork, not arbitrary in-place patching.
- Removing/reordering effect-producing calls, changing primitive kind/callsite identity, or changing stable pipeline keys is incompatible unless the run is forked.

### 7.3 Engine selection gate

The root-private engine SPI implements `WorkflowProgramPort`; the pure crate sees no engine type. PR 38A compares at least these exact reproducible candidates: current Boa; current rquickjs over original QuickJS; and, only when a maintained wrapper/provenance path is demonstrated, the same wrapper contract over QuickJS-NG. “QuickJS” without upstream commit/version, wrapper version, Cargo features, allocator, C toolchain, patches, and target triple is not a candidate.

Promotion selects the engine **and placement**. Test in-process realm-per-evaluation and a root-supervised local worker-process placement. The worker is a bounded pure evaluator mode of the TraceDecay installation, not a daemon, scheduler, executor, or history owner: it receives one framed source artifact plus replay slice, has no credentials/store/network/project filesystem, emits only typed program commands/diagnostics, and may be killed/restarted without losing authority. Prefer in-process only if native abort, allocator, stack overflow, panic/unwind, interrupt, and malformed-bytecode/source fault tests prove daemon containment; otherwise the supervised placement is the safe default.

Candidates are compared on:

- the required ECMAScript/async/promise/module subset and pinned Test262 cases;
- deterministic microtask ordering and host-callback behavior;
- source locations/source maps and repeatable normalized AST/callsite IDs;
- instruction/fuel interrupt, wall deadline, memory/stack/heap caps, cancellation, and runaway promise/loop containment, including uncatchable termination from JavaScript;
- proof that the configured memory limit is effective under the exact allocator/features (rquickjs documents that its limit is a no-op with custom/Rust allocator modes);
- zero ambient globals/import resolution and zero engine job/future after a declared suspension point;
- removal of nondeterministic/host capabilities;
- Linux/macOS/Windows build and release packaging;
- cold compile/evaluate and warm replay p50/p95;
- per-realm and many-realm peak RSS, leak slope, binary/build/cache size, crash/abort behavior, startup/restart, and upgrade compatibility;
- reproducible output/history across 1,000 identical replays.

QuickJS-family bytecode is explicitly non-durable: official documentation binds it to the engine version and warns against untrusted loading. TraceDecay recompiles the sanitized executable source under the pinned ABI; an optional local compiled-code cache is untrusted, ABI-keyed, integrity-checked, discardable, excluded from backup/sync/export, and never accepted as history or cross-machine input. Boa AST/bytecode serialization follows the same rule.

The benchmark publishes raw manifests/results and one frontier decision: required conformance/containment/portability first, then deterministic warm replay and interactive latency, then RSS/build cost. A faster candidate that cannot bound memory, interrupt loops, reproduce the command tape, or survive fault injection is ineligible. No silent engine fallback occurs: an unavailable pinned engine yields `IncompatibleRuntime`; a user may fork onto another promoted ABI after exact replay comparison. Each promoted adapter and exact engine/version/features/placement/compiler/target manifest is a runtime pin.

## 8. Effects, agents, routing, and capacity

An `agent()` call becomes a workflow-specific extension of a generic `operation_step`, not a `WorkItemV1` or `ExecutionAttemptV1`. It uses the same executor registration and host adapter catalog as task execution through one domain-owned envelope:

```rust
pub enum ExecutionUnitSubjectV1 {
    TaskAttempt { attempt: ExecutionAttemptId, lease: TaskLeaseFenceRefV1 },
    WorkflowActivity { run: WorkflowRunId, node: WorkflowNodeId, command: WorkflowCommandId },
}
pub struct ExecutionUnitV1 {
    pub unit_id: ExecutionUnitId,
    pub subject: ExecutionUnitSubjectV1,
    pub authority: ExecutionAuthorityRefV1, // task lease or workflow operation epoch, closed union
    pub route_request: RouteRequestV1,
    pub capability_grants: CapabilityGrantSetRefV1,
    pub context: ExecutionContextRefV1, // task context packet or workflow context manifest
    pub input_schema: JsonSchemaRefV1,
    pub output_schema: JsonSchemaRefV1,
    pub input_digest: ManifestDigest,
    pub budget: ExecutionBudgetV1,
    pub idempotency_key: EffectIdempotencyKeyV1,
    pub deadline: UtcMicros,
}
```

Plan 24 owns the shared envelope/authority vocabulary. A standalone workflow uses its generic-operation owner/epoch and never mints a `TaskLeaseId`; a task child workflow remains under the outer task fence but its workflow nodes still have operation-step identity. Provider/model/effort/tool/skill/context routing is requested in the script and selected/authorized by application/policy before reuse lookup and dispatch. The selected effective route becomes part of the immutable activity signature.

Every invocation records requested and actual executor/host/provider/model/effort, permission mode, tools/skills, context packet and retrieval anchors, input/schema/prompt-safe digest, definition/run/call path, token/cost/latency, transcript/session/Turn refs, tool/artifact/effect receipts, cancellation, retries, and output validation.

No script receives a lease proof, token, credential, raw provider error, private sibling prompt, or unrestricted tool. Node output contains the schema-validated value plus safe references; full evidence is resolved separately under authorization.

Dispatch is at-least-once only where the adapter declares a stable idempotency key and reconciliation probe. Non-repeatable adapters use at-most-once admission: a crash after send but before acknowledgement yields `EffectUnknown`, not automatic retry. A retry always creates a new `WorkflowActivityAttemptV1` beneath the same command, carries the prior effect evidence, and is legal only after the application proves the previous attempt failed before effect or the adapter confirms safe idempotent replay. Completed or effect-unknown commands can change only by forking a run.

Shared scheduler behavior:

- default concurrent agent cap is 16, configurable downward/upward within policy and host capacity;
- total nodes/agent calls default to a bounded policy profile and hard cap; Claude's 1,000-agent maximum is a research comparator, not an automatic default;
- nested depth, fan-out, pipeline cardinality, queued bytes, prompt/output bytes, tokens, cost, wall time, retries, and retained artifacts all have declared run budgets;
- workflow, task, automation, and interactive work share fairness/admission/circuit-breaker policy; workflows cannot monopolize the executor fleet;
- backpressure suspends new scheduling without re-evaluating effects or losing history;
- cancellation fences new effects, reconciles unknown external effects, and only then permits terminal/retry transitions.
- every activity declares an advisory target duration, durable progress/heartbeat schema, bounded provider/tool-call request deadline, and prompt/output/tool-result byte and token ceilings; connected-but-silent provider/tool calls can time out independently, while missing workflow progress opens an incident without automatically terminating the workflow;
- agent inputs are recipient-specific bounded context packets plus retrieval anchors/page cursors, never concatenated parent transcripts, workflow journals, repository dumps, or unbounded prior tool output;
- the scheduler adapts pipeline/fan-out width to measured capacity and critical-path value, starts independent work without waiting for unrelated stragglers, and fences/cancels the losing copies when policy permits speculative execution of pure snapshot-bound reads; mutating or effect-unknown activities are never duplicated speculatively;
- missing expected progress records one durable stall incident and awaits an explicit continue/cancel/reconcile/redecompose/block decision; no wall-clock, per-agent, workflow, or no-progress timer automatically changes node state, and retry cannot only increase context, request timeout, or token limits.

## 9. Result reuse and cache provenance

Three concepts stay distinct:

1. **history replay:** mandatory reuse of already committed results inside the same run;
2. **fork import:** explicit reuse of eligible calls from a source run into a new run;
3. **cross-run cache:** optional disabled-by-default memoization across unrelated runs.

They have separate commands, configuration, receipts, metrics, UI labels, and retention. Same-run history is mandatory and cannot be disabled, evicted while the run is retained, or counted as a cache hit. Fork import is requested with an immutable source-run/call-selection manifest and defaults to no reuse. Cross-run cache is consulted only after current authorization, scope/context assembly, effective route selection, and freshness evaluation produce the complete lookup key.

An exact `WorkflowReuseKeyV1` includes definition/compiler/engine ABI, primitive/call path, input and output schema, prompt-safe input, context packet/anchor set and source watermarks, scope, requested and actual executor/model/effort/tool/skill route, config/catalog/policy/privacy versions, dependency output digests, and effect classification.

Cross-run reuse is legal only when:

- the definition marks the call cache-eligible;
- application confirms the full key and source result/receipt are available and authorized;
- the source invocation has a terminal validated result;
- there are zero unknown effects;
- the activity is cataloged `Pure` or `ReadOnlySnapshotBound`; a mutating/external side effect is never skipped in a new run merely because its execution is idempotent;
- freshness/TTL and source watermarks remain acceptable;
- policy permits memoization for that model/data class.

Model output may be memoized as historical output, but it is never described as deterministic or fresh. The UI/result says `Executed`, `HistoryReplay`, `ForkReused`, or `CrossRunMemoized`, with source run/node, eligibility decision, pins, age, and reason. Prompt-text similarity alone is never a cache key.

`WorkflowReuseReceiptV1` binds source/target run and command, reuse class, complete source and target signatures, authorization/freshness decisions, source terminal/effect receipts, value/schema digest, policy/config/catalog/compiler/engine versions, decision reason, and occurred time. Reuse copies no authority or engine object. Revocation/tombstone makes future lookup ineligible but does not rewrite a retained target history. A cache miss, unavailable cache, or corrupt cache runs normally unless the caller explicitly selected strict diagnostic mode; there is no lower-quality/model fallback hidden behind “cache.”

## 10. Lifecycle and control

### 10.1 Definition lifecycle

`Candidate -> Validated -> Active -> Retired | Rejected`. Validation proves syntax, supported SDK/engine ABI, bounded static metadata, schemas, forbidden globals/imports, source-map/callsite manifest, catalog/config compatibility, and dry replay of non-effectful branches where possible. Activation selects a version for new invocations; old versions remain available while referenced by retained runs.

### 10.2 Run lifecycle

```text
Queued -> Admitted -> Running
Running <-> WaitingForActivity | WaitingForTimer | WaitingForSignal | WaitingForCapacity
Running/Waiting -> PauseRequested -> Pausing -> Paused -> Running
Running/Waiting/Paused -> CancelRequested -> Cancelling -> Cancelled | EffectUnknown
Running/Waiting -> Completed | ContinuedAsNew | Failed | BudgetExhausted | HistoryLimitExceeded | Nondeterministic | IncompatibleRuntime
```

Pause prevents new activity admission after a safe checkpoint; it does not freeze a side-effecting tool mid-call. Resume replays from canonical history. Retry creates a new activity attempt under the same command identity only when effect reconciliation permits. A completed, effect-unknown, or downstream-consumed command cannot be restarted in place; the UI/API offers a fork from the nearest safe checkpoint. Fork creates a new run/version relation. `ContinuedAsNew` atomically seals one run and creates its successor relation; it is terminal for the old run, not pause/resume. Every terminal state requires generic-operation closure plus command-tape/history/output/effect/required-steering receipts.

### 10.3 Controls

Humans and authorized agents can pause/resume/cancel the run, cancel/retry a safe node attempt, fork from a checkpoint, change future capacity/budget within policy, submit a schema-valid signal to an explicit wait, or submit structured live steering. Every control is a disjoint idempotent expected-version command with actor, reason, event, and receipt; there is no generic action bag in application code. It cannot rewrite history, mutate definition/config pins, retroactively change a completed node's route, or convert a comment into signal/steering implicitly.

### 10.4 Canonical source, file discovery, and names

Canonical definitions live only as immutable daemon-managed definition/version/source-artifact records. Repository or user files are readable authoring sources and export targets, never live execution authority. A run always names a persisted definition version and artifact digest; editing, Git switching, deleting, or shadowing a file cannot change an admitted run or active definition.

TraceDecay supports the conventional repository source directory `.tracedecay/workflows/` only after the repository/project is exactly registered. It also supports explicitly configured profile source roots through plan 20. No daemon scan starts from process CWD, walks parent directories, reads a “nearest” file, treats `.claude/workflows/` as live V2 authority, or silently prefers project over profile content. Each observed source is `WorkflowSourceCandidateV1 { candidate_id, source_root_id, declared_scope, repository/project/worktree/ref evidence, normalized relative locator digest, content digest, language, observed_at, watcher_generation, status }`.

Discovery is bounded and daemon-owned. Watchers publish create/change/remove observations and validation status; they do not create/activate definition versions or launch runs. Explicit `import_source_candidate` revalidates the exact candidate ID/content/watcher generation and creates a new immutable definition version with source provenance. Re-import of the same definition/content is idempotent; changed content creates a successor. Removal marks the candidate absent but retains canonical definitions/runs. A local-only uncommitted file is visibly local evidence; Git/ref/remote availability and cross-machine divergence remain separate coverage.

Names are scoped aliases, never authority. Canonical invocation uses `WorkflowDefinitionVersionId`. Name resolution requires an explicit profile/project/project-set scope and returns zero/one/many candidates with owner, source kind, active version, and exact IDs. Same-name profile/project/repository sources do not use ambient-CWD, “closest directory,” plugin load order, or newest-file precedence; ambiguity is a typed conflict. Generated plugin/slash commands bind a stable catalog binding plus definition/version policy and use disambiguated display namespaces when names collide.

Export streams the authorized original source or canonical executable/schema bundle from a definition-version payload route. CLI/SDK writes those bytes to an explicit client-side file/archive and records the content digest; the API never accepts or returns a server filesystem path. Exporting to `.tracedecay/workflows/` does not auto-import or activate it. A watcher later observes the file as a candidate, preserving the intentional two-step loop and preventing edit/write feedback cycles.

## 11. Persistence and recovery requirements

Plan 02 owns the exact physical schema. It must extend existing activity, blob, generic operation/step, event/outbox, idempotency, audit, retention, backup/restore, and anchor families for:

- workflow definitions and immutable versions;
- protected source/source-map/schema/compiler manifests;
- workflow-run extension rows over generic operations;
- phase/node/call projections over generic operation steps;
- canonical replay-command/history payloads in the existing activity event stream;
- child/fork/reuse/materialization relations;
- result/artifact/transcript/anchor and cache-eligibility receipts;
- current replay/checkpoint projections, never a second authority journal;
- definition/run indexes by owner/scope/state/version/time and node indexes by run/call path/phase/state;
- bounded retained source/result blobs and explicit deletion/tombstone behavior.

The canonical history vocabulary is closed and append-only: `RunAdmitted`, `CommandBatchScheduled`, `ActivityAttemptDispatched`, `ActivityAttemptDeliveryObserved`, `ActivityResultRecorded`, `TimerScheduled/Fired`, `SignalAccepted`, `SteeringObserved`, `ChildStarted/Closed`, `CheckpointRecorded`, `ContinueAsNewCommitted`, `OutputValidated`, `RunTerminal`, and typed failure/reconciliation events. Every record carries run/authority epoch, monotonic history sequence, causation/correlation, command tape position/call ID where applicable, schema/manifest/version pins, sanitized payload/receipt refs, TraceDecay build, and event digest. Commands and addressed results are distinct; arrival order is never inferred to be command order.

Plan 02 must lower this as workflow-specific extension rows/blobs attached to the existing activity canonical event/outbox infrastructure and generic operations/steps—not plan 24's task-only `task_graph_events`; it must not create `workflow.db`, `workflow_events`, `workflow_outbox`, `workflow_jobs`, or a second append service. The workflow history repository port performs registered bounded reads/writes through the activity owner shard. Project shards receive only canonical relations/anchors. Definition source, schemas, prompts, results, signals, and logs follow existing protected blob/sanitizer/retention contracts; safe indexed columns contain IDs/enums/digests/times only.

Run admission, command batch plus operation steps/effect reservations/outbox, activity result plus node state, signal/steering append, continue-as-new predecessor/successor, and terminal run plus operation closure each have one owner-shard transaction. Cross-shard evidence is referenced by receipt/anchor and never transactionally copied. Derived current/graph/checkpoint/cache views rebuild from history and carry source sequence/watermark; a failed projector or missing cache cannot stop canonical replay.

Crash tests cover before/after definition version publication, run admission, command/history append, effect outbox, activity result, retry, checkpoint, pause, cancellation, terminal commit, cache receipt, and taskgraph candidate publication. Recovery produces one command/effect/result or explicit unknown state, never a duplicate model-visible effect or false terminal success.

## 12. Taskgraph composition

### 12.1 Ordinary workflow mode

An ordinary workflow is optimized for ephemeral or reusable orchestration whose control structure lives in code. Its nodes are workflow nodes and generic operation steps. They appear in the Brain graph and timelines but not in board/task queues. This is the default for audits, research, migrations, broad reviews, fan-out verification, and repeatable agent pipelines.

### 12.2 Explicit taskgraph candidate compilation

Plan 32 and `tracedecay-workflow` exclusively own the workflow-to-taskgraph compiler implementation and its eligibility, mapping, omission, loss, identity, determinism, provenance, and idempotency semantics. `workflows.task_graph.eligibility.get` is a sealed read over an exact definition version or executed-run manifest. `workflows.task_graph.compile_candidate` performs `analyze -> validate -> candidate` and stops. Plan 24 supplies only the target `PlanVersionV1`/`WorkItemVersionV1` schemas and the existing review/edit/activation commands; it cannot reinterpret workflow nodes or implement another compiler. This feature has no `preview/apply/rollback`, direct activation, or generic materialize command.

Eligibility requires:

- a bounded statically discoverable subgraph or a frozen executed graph manifest;
- stable node identities, dependencies, ownership/route constraints, scopes, acceptance contracts, budgets, and exact artifacts;
- no unresolved dynamic loop/branch whose future node set is unknown;
- no workflow-only timer/signal/cache behavior lacking a taskgraph representation;
- no cycle through a task that invokes the source workflow;
- explicit disposition for phase/group/log/checkpoint nodes that do not become work items.

The compiler emits a candidate `PlanVersionV1`, proposed `WorkItemVersionV1` rows/edges, workflow↔task provenance, context packet inputs, acceptance/test/review/integration gates, and a loss/unsupported report. The candidate binds source definition/run manifest, compiler version, graph digest, every node mapping/disposition, omissions, and generated local-key map; retry with the same idempotency/source digest returns the same candidate. It never activates the plan or mutates the workflow. Taskgraph review can reject or edit the candidate without changing the source workflow or past runs; any edited taskgraph version records that it diverged from generated source rather than pretending it can round-trip to JavaScript.

### 12.3 Task invokes workflow

A task acceptance contract may invoke an exact workflow definition version as one bounded step. The task attempt remains the outer authority/lease; the workflow run receives a deterministic child-run idempotency key, child operation relation, task-attempt/fence ref, scope/grant/budget ceiling, context mapping, result schema, and cancellation/signal/steering propagation policy. No workflow node receives a task lease or becomes independently board-ready. Workflow children cannot outlive the outer fence; V2 does not support detached task-child workflows. Revocation/expiry fences new child effects and reconciliation must close before task retry. The task completes only after the workflow terminal receipt, mapped output/artifact anchors, zero required-steering/effect uncertainty, and independent task-specific acceptance evidence.

Cycle validation uses one combined typed invocation graph before admission and again at child start: task → workflow version/run, workflow → child workflow version, and workflow-candidate → task provenance edges. It rejects direct/indirect cycles, candidate self-materialization, run-generation loops without `continueAsNew`, depth/cardinality expansion beyond the frozen budget, and ambiguous “latest active version” child refs. Every child ref resolves to an exact immutable definition version before the parent run is admitted.

## 13. Live steering, comments, hints, and hooks

Plan 01 is the sole owner of `SteeringDirectiveV1`, `SteeringTargetV1`, revision, requirement, delivery claim/receipt, acknowledgement, and disposition types/state machine. Plan 32 owns only admission and lifecycle semantics for `SteeringTargetV1::{WorkflowRun,WorkflowNode}` under workflow operation authority/fences and consumes the exact Plan-01 envelopes; Plan 24 analogously owns task-attempt lifecycle, while Plan 07 only delivers already-authorized envelopes at host-safe boundaries. Every workflow directive pins definition/run/node/command where applicable, authority epoch, monotonic target steering sequence, actor/authority, expected run/node/history/graph/accepted-context revisions, bounded sanitized payload, requirement, priority, expiry, and idempotency. Plain comments are shared annotations/history and never become prompts implicitly; an authorized exact annotation revision must be explicitly promoted.

- A required directive is ordered, delivered at a declared safe adapter boundary, acknowledged, and resolved `Applied|Rejected|Superseded` before the affected node/run can terminally complete. Delivery expiry never silently clears the fence.
- Advisory guidance may be compacted/deduplicated and never fences completion.
- Delivery priority is provider-native addressed current-Turn interrupt when capability-proven, after tool result/before next model call, one bounded `Stop`/`SubagentStop` continuation, otherwise next-Turn-only. Unsupported paths and delivery unknown are explicit; no tool is interrupted mid-side-effect and uncertain model-visible delivery is never retried automatically.
- Run pause/cancel/steering are distinct commands.
- A schema-valid `waitForSignal` event is planned data consumed by JavaScript and recorded on the command tape. Steering changes agent/run guidance or requests a checkpoint but is not returned as script data. Comments, signals, steering, hints, and notifications are five distinct types and receipts.
- Plan 22 suggestions remain advisory evidence and cannot impersonate controller steering.
- Hooks capture lifecycle and deliver already-authorized envelopes; they do not execute workflow JavaScript or make readiness/cache decisions.

Workflow history pins the last observed steering sequence and complete delivery/ack/disposition receipt basis at node/run completion. Concurrent controllers allocate sequences by expected-version CAS. Late required steering and terminal completion race in the owner transaction: exactly one wins; a rejected late directive never mutates a closed history or successor generation. Workflow Studio/replay shows steering beside the exact model/tool/Turn boundary, but replay cannot redeliver or disposition it.

## 14. Generated API, SDK, CLI, and MCP

### 14.1 Catalog use cases

```text
workflows.definitions.list|get
workflows.definition_versions.list|get|diff|validate_source|create|activate|retire
workflows.source_candidates.list|get|discover|import_version
workflows.runs.list|get|compare|start|fork|pause|resume|cancel|signal
workflows.runs.history_page.get
workflows.nodes.list|get|retry|cancel
workflows.task_graph.eligibility.get|compile_candidate
```

This is the semantic family; generic operation/status/cancel, subscriptions, exports, retrieval anchors, experiments, and taskgraph review/activation stay in their existing families. There is no `workflows.events.subscribe` stream implementation: clients create a generic subscription over the workflow run view and receive deltas carrying canonical activity event/outbox ranges. Replay/engine-policy/cache/fault experiments use the one generic experiment lifecycle with `LabKindV1::Workflow`; they cannot mutate a live run.

`workflows.runs.history_page.get` is the sole sealed command-tape/history read. Its first request authorizes the run and freezes `WorkflowHistorySealV1 { run_id, authority_epoch, definition_version_id, maximum_history_sequence, maximum_command_tape_sequence, head_event_digest, history_schema_version, build_version, snapshot_watermark }`. Every opaque cursor binds that seal, page size, access/redaction digest, and prior page end; pages return ordered canonical event headers plus command identities/dispositions and protected payload/anchor refs, never writable/raw store records. A live run's later events require an explicit new seal and cannot drift into the current traversal. Cursor/authorization/retention mismatch fails closed; terminal history seals are indefinitely replay-stable within retention/tombstone rules.

`start` accepts exactly one sealed definition-version ref or bounded inline source input plus structured `args`. Inline source is author bytes, not a client-produced artifact: the authoritative daemon compiler validates it and atomically persists its retained `Ephemeral` immutable definition/version before the run. “Ephemeral” controls discoverability/retention, not replay authority. A successful run can explicitly save/activate a successor definition. HTTP/API/MCP never accept an arbitrary server filesystem path. CLI `--file` reads local bytes client-side and uploads source/media type; the daemon never opens the path. Every mutation uses canonical idempotency, actor/scope, expected version/authority, and typed receipt envelopes.

### 14.2 Workflow invocation shape

```typescript
type StartWorkflowRunV1 = {
  source:
    | { kind: "definition_version"; id: string }
    | { kind: "inline_source"; language: "javascript" | "typescript"; source: string }
  args: unknown
  routeProfile?: string
  budgets?: WorkflowBudgetOverrideV1
  idempotencyKey: string
}

type ResumeWorkflowRunV1 = {
  runId: string
  expectedRunVersion: number
  expectedAuthorityEpoch: number
  idempotencyKey: string
}

type ForkWorkflowRunV1 = {
  sourceRunId: string
  checkpoint?: string
  targetDefinitionVersion: string
  reuse: { mode: "none" | "eligible_selected"; commandIds?: string[] }
  argsPatch?: unknown
  routeProfile?: string
  budgets?: WorkflowBudgetOverrideV1
  idempotencyKey: string
}

type WorkflowRunAccepted = {
  disposition: "accepted"
  operation: OperationRef
  runId: string
  definitionVersion: string
  runVersion: number
  authorityEpoch: number
  summary: string
  eventCursor: string
}
```

Start, resume, and fork are different use cases and request types; optional fields cannot smuggle one into another. Resume advances the same run only from `Paused|Waiting*` under its pinned version/ABI. Fork always creates a new run and explicit lineage/reuse receipts. Invalid syntax/schema/forbidden capability returns a typed rejected command and no run/operation. A transport-level acceptance is not workflow success, and a generic protocol task ID is not a workflow run/node ID.

### 14.3 HTTP/SSE and generated clients

Plan 10 generates exact bindings from the catalog family:

```text
GET  /api/v2/workflow-definitions
GET  /api/v2/workflow-definitions/{id}
GET  /api/v2/workflow-definition-versions
GET  /api/v2/workflow-definition-versions/{id}
GET  /api/v2/workflow-definition-versions/{id}/source
POST /api/v2/workflow-definition-versions:diff
POST /api/v2/workflow-definition-versions:validate-source
POST /api/v2/workflow-definition-versions:create
POST /api/v2/workflow-definition-versions/{id}:activate|retire
GET  /api/v2/workflow-source-candidates
GET  /api/v2/workflow-source-candidates/{id}
POST /api/v2/workflow-source-candidates:discover
POST /api/v2/workflow-source-candidates/{id}:import-version
GET  /api/v2/workflow-runs
GET  /api/v2/workflow-runs/{id}
GET  /api/v2/workflow-runs/{id}/history
POST /api/v2/workflow-runs:compare
POST /api/v2/workflow-runs:start|fork
POST /api/v2/workflow-runs/{id}:pause|resume|cancel|signal
GET  /api/v2/workflow-runs/{id}/nodes
GET  /api/v2/workflow-nodes/{id}
POST /api/v2/workflow-nodes/{id}:retry|cancel
POST /api/v2/workflow-task-graph:eligibility
POST /api/v2/workflow-task-graph:compile-candidate
POST /api/v2/subscriptions
GET  /api/v2/subscriptions/{id}/events
```

List/detail/compare/eligibility and history-page are bounded sealed application views with cursors, scope, snapshot/watermark, coverage, version/build pins, and anchors. `GET .../{id}/history` accepts only `seal|cursor`, bounded `limit`, and presentation-safe include flags and returns `WorkflowHistoryPageV1`; generated Rust/TS/Python pagers preserve the seal and expose an explicit `refresh_seal` operation. Protected source/result/transcript bodies resolve separately under the existing payload/anchor contract. Mutations return command receipts and the shared `OperationRef` where asynchronous. There is no `PATCH`, generic workflow action route, server path, per-run event stream, engine endpoint, raw history append, or client-side readiness/cache claim.

The workflow subscription snapshot includes run/phase/node/group state, history and command-tape cursors, operation/projection watermarks, unresolved required steering/signals/effects, coverage, and limits. Deltas name their canonical source-event range and are idempotent by event ID. Completion, effect unknown, required-steering, signal, nondeterminism, gap, and terminal events never coalesce away. Slow consumers receive resync/close; reconnect with `Last-Event-ID` either resumes exactly or reloads one authoritative snapshot. SDK pagers/streams expose this state machine rather than hiding gaps behind callbacks.

### 14.4 CLI

```text
tracedecay workflow list|show|versions|validate|save|activate|retire|diff
tracedecay workflow source list|show|discover|import|export
tracedecay workflow run --definition-version <WorkflowDefinitionVersionId> --args <json>
tracedecay workflow run --scope <selector> --name <name> --version-policy <active|exact:WorkflowDefinitionVersionId> --args <json>
tracedecay workflow run --file ./review.js --args <json>
tracedecay workflow status|watch|pause|resume|cancel|fork <run>
tracedecay workflow history <run> [--seal <seal>|--cursor <cursor>] [--limit <n>]
tracedecay workflow signal <run> --name <signal> (--json <value>|--stdin)
tracedecay workflow node retry|cancel <run> <node>
tracedecay workflow task-graph eligibility|compile-candidate <definition-version|run>
```

Markdown is default; `--json` emits the same typed view. A run never accepts a bare name, mutable definition ID, CWD inference, or implicit active-version lookup. It either receives exact `WorkflowDefinitionVersionId`, or explicit scope plus name and a declared version policy; name resolution returns typed ambiguity and the accepted receipt records the resolved exact version. `watch` consumes the canonical subscription cursor and visibly resyncs on a gap. `history` traverses the sealed paged history use case and never aliases watch/SSE. `--file` uploads source and never makes the daemon open the client path. `--language` may override extension only with an explicit media type; stdin is bounded. `source discover` requires explicit source root/project scope; `source import` consumes a pinned candidate ID/generation/digest; `source export` writes response bytes client-side to explicit `--output` and never changes canonical state. Resume and fork are distinct commands. Node retry refuses completed/effect-unknown/downstream-consumed nodes with exact fork guidance. Candidate compilation prints the Plan-32 compiler receipt plus plan-24 candidate ID, loss report, and review continuation; no CLI command activates it.

### 14.5 MCP progressive disclosure

The optional MCP profile exposes at most three workflow tools:

- `workflow_run`: validate/start/fork one workflow through a closed tagged request; it never resumes or controls an existing run;
- `workflow_get`: retrieve a definition/run/node/phase summary view; it does not page canonical history;
- `workflow_control`: the sole MCP owner of pause/resume/cancel/signal and safe node retry/cancel through a closed action enum.

Definitions, source candidates, schemas, scripts, run graphs, transcripts, artifacts, and taskgraph eligibility/candidate reports are discoverable as authenticated resources/templates with handles. Canonical history uses the authenticated paged resource template `tracedecay://workflows/runs/{run_id}/history{?seal,cursor,limit}` backed only by `workflows.runs.history_page.get`; no MCP tool returns or mutates history. Source discovery/import/export and candidate compilation remain CLI/API/SDK/orchestrator-only unless a separately budgeted orchestrator profile revision proves need; they are not hidden inside `workflow_control`. Skills teach CLI/API fallback. Do not add one MCP tool per catalog operation, place every definition in initial tool schema, return a giant history/transcript, accept a server path, or let MCP sampling become the workflow executor. Tools-only hosts remain complete through compact tools plus resource links and CLI recipes.

### 14.6 SDKs and authoring package

- generated Rust, TypeScript, and Python public clients expose the same application operations, including the sealed history pager;
- `@tracedecay/workflow` provides authoring helpers/types, compiler-ABI declarations, diagnostics/source-map types, and the generated HTTP client. It calls authoritative daemon validation/compile; an optional local-validation command can invoke only the digest-verified canonical compiler component distributed by TraceDecay and labels results advisory. The package contains no compiler implementation, artifact signer, scheduler, runner, engine, or provider client;
- no Rust IR builder, public IR-construction API, or alternate user-authored workflow path exists. Rust/TypeScript/Python clients submit JavaScript/TypeScript source or exact definition-version IDs. Static system recipes remain Plan-09 `OperationWorkflowDefinitionV1` values and never enter Plan-32 IR;
- Python may call the public API but V2 does not add a second Python orchestration language/runtime;
- all clients expose separate start/resume/fork/control/stream types and surface event gaps, command receipts, operation refs, coverage, version pins, and typed problems unchanged;
- plugin bundles project saved workflows as commands/skills referencing stable definition/version/catalog bindings without copying source or host-specific orchestration logic. A plugin command cannot elevate its route/grants or shadow a nearer host-native workflow silently.

## 15. Workflow Studio and Run Graph

Plan 11 owns final information architecture and components. Required product views:

### 15.0 Product and state contract

Routes are `/workflows`, `/workflows/definitions/:definitionId/versions/:versionId`, `/workflows/runs/:runId`, and the generic `/playgrounds/workflow/:experimentId`; node/phase/call IDs are URL selections within the run, not route-local entities. All views share one generated `WorkflowInvestigationStateV1`: scope, live/frozen/as-of watermark, definition/run comparison set, phase/group/node selection, history playhead, overlay, search/filter, camera/table position, inspector tab, and coverage. Opening a linked Agent/Session/Turn/tool/code/Git/task/artifact/anchor in Brain, Explorer, or Causal Loom preserves that state and a typed backlink. The browser never evaluates source, reconstructs readiness/critical path/cache truth, joins transcripts to nodes by time, or mutates a local run graph.

The visual grammar is deliberate rather than one hairball:

- **Definition map:** code outline and source spans aligned to phase/control-flow/data-flow lanes, static versus runtime-discovered regions, fan-out cardinality estimates, schema ports, and forbidden/unknown constructs.
- **Run graph:** hierarchical phase → group → node → activity-attempt semantic zoom with stable object identity, collapsed fan-out bands, critical path/slack, dependency/data edges, and explicit unresolved/hidden counts.
- **Execution timeline:** aligned run/phase/agent/model/tool/signal/steering/effect lanes with queue/wait/execute/retry intervals, occurred-versus-ingested evidence, late events, and a command-tape/history playhead.
- **Resource views:** switchable route/executor Sankey, cost/token heatmap, concurrency/capacity chart, cache/reuse provenance graph, failure/retry matrix, and table/outline equivalents over the identical sealed result set.
- **Causal follow:** select one node or agent and follow input anchors → prompt-safe context → model/tool attempts → result/artifact/code/Git/task impact without implying causation from proximity.

Every color/edge/animation has text, table, keyboard, and screen-reader equivalents. Server layouts/aggregates own large-run grouping; renderer choice changes no membership/count. Semantic zoom and timeline bins are versioned projections with exact watermarks and retrieval continuations. Canvas/WebGL may render topology, but an accessible virtualized outline/table remains canonical for interaction.

### 15.1 Definition Studio

- definition list with active version, owner/scope, last run, health, usage, and legal actions;
- syntax-aware JavaScript editor, meta/schema inspector, validation diagnostics, forbidden-capability diagnostics, source map, and version diff;
- compiled/discovered graph analysis with static/dynamic/unsupported regions, command callsites, schema ports, bounded-loop/cardinality evidence, and taskgraph-candidate eligibility/loss report;
- typed args form generated from JSON Schema and exact raw JSON mode;
- route, tools/skills, budgets, concurrency, cache, and remote capability analysis from effective configuration;
- save new version, validate, activate, run, fork, and compare—never browser execution. Save is immutable version creation, not autosave mutation; dirty local editor bytes are visibly noncanonical and recover only through the ordinary contained draft policy.

### 15.2 Live Run Graph

- phase swimlanes plus zoomable DAG and aligned timeline; users can switch graph/time/list without losing selection;
- hierarchical parallel/pipeline groups with virtualization for hundreds/thousands of nodes;
- live node state, queue/capacity, host/model/effort, tokens/cost/latency, retries, result provenance, and coverage;
- node inspector linking prompt-safe input, schema, output, transcript/session/Turn, tools, artifacts, anchors, steering, errors, and operation receipts;
- planned versus dynamically discovered graph and current replay cursor/history watermark;
- synchronized command-tape inspector showing expected/actual command, source span, input/schema/dependency digests, addressed result, engine/compiler/build pins, and first nondeterminism mismatch;
- the inspector pages only through `workflows.runs.history_page.get`, pins and displays its `WorkflowHistorySealV1`, never mixes later live events into the traversal, and offers an explicit “refresh to new seal” action while the subscription continues separately;
- pause/resume/cancel/fork and node retry/cancel actions only when the application view exposes them;
- run/node steering rail and composer with exact target/authority/sequence/revision, actual safe delivery boundary, acknowledgement/disposition, required completion fence, advisory non-blocking, and plain-comment separation;
- signal inbox showing declared waits, schema, deadline/timer race, accepted value receipt, and unrelated comments/hints excluded;
- cache badges `history`, `fork`, `memoized`, or `executed`, with a complete explanation drawer;
- explicit `waiting`, `partial`, `unknown`, `effect unknown`, `nondeterministic`, and `incompatible runtime` states.

### 15.3 Compare and Replay Lab

- compare definition versions and two or more runs by source/callsite mapping, graph/data edges, command tape, route/model, inputs/outputs, failures, cost/tokens/latency, cache decisions, and result quality;
- replay a frozen history with a candidate engine/compiler/policy in the generic experiment runner without executing effects; show the first command/output/diagnostic divergence and every substitution/unavailable input;
- fork a run from a selected checkpoint into a hermetic experiment;
- analyze workflow-to-taskgraph eligibility and inspect the candidate/loss report, proposed ownership/dependencies/acceptance, source mapping, and provenance; activation remains in the canonical Work review experience;
- export a safe manifest/report through the shared export system, not raw database/script paths.

### 15.4 UX gates

Desktop, narrow/mobile, keyboard-only, reduced-motion, screen-reader, large-run, high-latency, reconnect, partial-data, and denied-data fixtures are mandatory. Initial run hydration is a bounded summary plus visible window; node/history/transcript bodies page independently. A 10,000-node retained fixture keeps initial payload within the plan-11 route budget, DOM rows below 1,000, interaction p95 below 100 ms after hydration, and exact aggregate/visible/hidden counts. A user must be able to answer within the comprehension test: what is running, why it is next/waiting, where and under whose authority, what data/effects it used, at what cost, what failed or is unknown, what was reused, what steering/signal is pending, what can safely be controlled, and whether a taskgraph candidate or activated graph exists.

Required scripted journeys: author TypeScript and fix a source/schema diagnostic; run Find/Verify/Sweep/Rank and follow one finding to its transcript/code/PR evidence; pause with an in-flight effect and observe safe reconciliation; steer an active node and see required versus advisory terminal behavior; reconnect across an SSE gap; compare engine/compiler replay and locate the first mismatch; explain a cross-run memoized result; compile a taskgraph candidate and continue into plan-24 review without losing source selection. Visual QA includes light/dark, 200% zoom, mobile, reduced motion, color-blind palettes, and deterministic export.

## 16. Configuration

Plan 20 registers typed settings; names below are candidate canonical IDs pending registry review:

```text
workflows.enabled
workflows.compiler.selected
workflows.engine.profile
workflows.engine.placement
workflows.engine.max_heap_bytes
workflows.engine.max_stack_bytes
workflows.engine.instruction_budget
workflows.engine.compile_timeout_ms
workflows.engine.evaluation_timeout_ms
workflows.engine.max_pending_jobs
workflows.replay.snapshot_interval_events
workflows.replay.max_history_events
workflows.replay.max_history_bytes
workflows.replay.verify_overlap_events
workflows.run.max_concurrent_runs
workflows.run.max_concurrent_agents
workflows.run.max_total_nodes
workflows.run.max_depth
workflows.run.max_wall_time
workflows.run.max_input_bytes
workflows.run.max_output_bytes
workflows.run.max_artifact_bytes
workflows.run.max_tokens
workflows.run.max_cost
workflows.run.max_pending_signals
workflows.run.max_signal_bytes
workflows.run.max_child_runs
workflows.run.max_command_batch
workflows.pipeline.default_concurrency
workflows.fork.reuse_enabled
workflows.cache.cross_run.enabled
workflows.cache.cross_run.max_age
workflows.cache.max_bytes
workflows.source.repository_convention_enabled
workflows.source.profile_roots
workflows.source.watch_enabled
workflows.source.watch_debounce_ms
workflows.retention.source
workflows.retention.history
workflows.retention.results
workflows.task_graph.candidate_compilation_enabled
workflows.remote.execution_enabled
```

Defaults remain conservative: no engine profile activates before conformance; cross-run cache and fork reuse are disabled; profile source roots are empty; watchers do not import; remote execution is separately gated; taskgraph candidate compilation activates only when plan-24 capability exists. Plan 20 defines types/units/ranges, hard safety floors, source layers, target scope, restart/recompile/replay impact, and desired/activated/effective/observed state. Per-definition/run overrides can only tighten the effective ceiling unless a separately authorized controller grant explicitly permits a bounded increase.

Configuration changes never mutate historical pins. Engine/compiler/schema changes apply to new definition versions/runs; capacity/budget reductions stop new admission and let current effects reconcile; source-root changes affect future discovery only; cache disable prevents new lookup and does not relabel prior reuse; retention follows existing hold/tombstone operations. Dashboard/CLI/API show why a requested value is capped or pending and the exact run/definitions affected. There is no raw engine flag bag, arbitrary compiler option, environment passthrough, or config key that enables forbidden globals/imports.

## 17. Failure and recovery contract

| Failure | Required behavior |
|---|---|
| Syntax/schema/unsupported SDK | Reject version/run creation with source diagnostics; no run/operation effect. |
| Source candidate changed between discovery/import | Expected generation/content digest conflicts; no version is created and the new candidate is returned. |
| Engine unavailable or ABI mismatch | Existing pinned runs enter explicit unavailable/incompatible state; no alternate engine silently resumes them. |
| Engine/helper panic, abort, OOM, stack overflow, interrupt failure | In-process profile fails promotion if daemon containment is not proven. Supervised placement records typed evaluator loss, kills/restarts the helper, discards ephemeral engine state, and replays from canonical history without admitting a replacement effect blindly. |
| Nondeterministic command sequence | Stop before new effect, preserve history, show first mismatch/source locations, offer fork only. |
| Daemon/host crash | Replay canonical history and resume pending work; committed command admission is not repeated, while sent-without-receipt effects enter reconciliation/unknown according to adapter semantics. |
| Executor/provider unavailable | Shared route/retry/fallback policy applies; requested/actual route and unavailable coverage remain visible. |
| Executor/provider connected but silent | A bounded provider/tool-call request deadline yields a typed timeout; missing workflow progress opens one visible incident and requires explicit cancellation or continuation before effect reconciliation and any reroute. TCP/process liveness alone is not progress, and elapsed workflow time alone is not cancellation authority. |
| Activity result lost/unknown | Reconcile through executor/effect receipts; never schedule a replacement effect until safe. |
| Schema-invalid model output | Bounded repair/retry under the same node history; terminal failure when exhausted. |
| Budget/cap exceeded | Stop new admission and terminally record the exact exhausted dimension; partial outputs are not complete. |
| Pause during side effect | Mark pause requested, stop new work, wait/reconcile current effect, then checkpoint paused. |
| Cancel with unknown external effect | Fence new work, retain effect-unknown state, and block unsafe retry, terminal success, and taskgraph candidate compilation. |
| Signal versus timer/cancel/terminal race | One owner-shard expected-version transaction wins; losing input is retained as rejected/late evidence and never delivered to a successor implicitly. |
| Required steering delivery unknown/expired | Keep the node/run terminal fence until an explicit disposition; never retry uncertain model-visible injection or waive on expiry. |
| History gap/corruption | Quarantine/recover from verified backup/event chain; never regenerate history from transcripts. |
| Replay snapshot/cache corrupt or missing | Discard derived artifact and replay canonical history; never repair history from it. |
| History hard cap reached | Admit only `continueAsNew` under its successor transaction or fail `HistoryLimitExceeded`; never truncate behind the same run ID. |
| Cache entry stale/incompatible/unauthorized | Execute normally or fail strict-cache request; never silently use it. |
| Remote node partition | Authority remains with the fenced owner shard; reconnect deduplicates by operation/call/effect IDs. |
| Taskgraph compiler loss | Return an explicit unsupported/loss report and no candidate plan. |
| SSE/client gap | Mark live view stale and reload authoritative snapshot; never infer completion, delivery, or cache state from missing deltas. |

## 18. Remote and cross-host execution

Plan 28 owns authority. A workflow definition/run belongs to one profile activity shard authority epoch at a time. Only that authority executes/replays the program and appends commands. Remote executors receive bounded `ExecutionUnitV1` activities and return addressed receipts; they do not own workflow history, evaluate JavaScript, mint call IDs, resolve cache hits, or run the orchestrator independently. Offline machines may capture provider activity but cannot advance a workflow without current authority/operation epoch.

Cross-host handoffs retain workflow/run/node/call IDs, engine/definition pins, scope/grants/budgets, context/anchor set, and cancellation/steering watermarks. Same Git repository clones correlate through plan-16 identity; local absolute paths never become workflow identity. All Hermes profiles, Codex, Claude, and Cursor share the same user-level TraceDecay workflow definitions according to existing profile rules.

Authority promotion requires the new owner to possess the exact source/schema/compiler/engine ABI or declare `IncompatibleRuntime` before accepting writes. It verifies the canonical history chain, operation epoch, outbox frontier, unresolved effect/signal/steering set, and last command tape position, then advances the authority epoch and fences the old owner. The old owner cannot publish buffered commands/results after reconnection. Engine compile caches and repository source files are never replicated authority; canonical protected source artifacts and histories follow the shared-Brain placement/backup protocol.

Remote dispatch is idempotent by execution unit plus activity-attempt/effect key. Reordered/duplicate replies insert-or-read one receipt. Partition after send follows the adapter's declared effect semantics and may remain `EffectUnknown`; failover cannot “try another host” until reconciliation proves safety. Route fallback is a new recorded attempt under the same command, not a rewritten history event. Coverage distinguishes authority unavailable, executor unavailable, source artifact unavailable, engine ABI unavailable, and hidden/denied evidence.

## 19. Observability, evaluation, and SLOs

Plan 26 owns dimensions and rollups. Required measurements include:

- definition/run/node counts and state distributions;
- admission/queue/engine compile/replay/activity/critical-path/terminal latency p50/p95/p99;
- replay event throughput, history bytes, checkpoint size, nondeterminism rate, resume success, and crash recovery time;
- concurrent/total agents, pipeline width/depth, backpressure, fairness delay, and executor utilization;
- per-node advisory target duration/progress age, queue/lock/provider/tool-call stage latency, context/prompt/output sizes, context-window rejection, observed stalls/redecomposition, explicit-cancellation acknowledgement, and straggler critical-path waste;
- requested/actual host/provider/model/effort/tool/skill route;
- input/output/token/cost/artifact totals with unknown denominators;
- history/fork/cross-run reuse rate, bytes/cost avoided, invalidation reasons, age, and quality outcomes;
- schema repair/retry/failure, effect-unknown, cancellation, pause, steering delivery, and taskgraph candidate-compilation outcomes;
- dashboard render/interaction latency and large-graph memory.

Every structured log/diagnostic/event includes `tracedecay_version`, component/build digest, workflow IR/schema version, definition/source/compiler/engine ABI, host adapter version where applicable, run/node/command correlation, and authority epoch. Raw source/prompt/result/signal/steering values and high-cardinality IDs do not become metric labels. Metrics distinguish command admission, dispatch attempt, observable model/tool effect, validated result, replay hit, and cache reuse; “agent count” cannot collapse those denominators.

Evaluation has four strata: deterministic runtime conformance; effect/recovery correctness; orchestration quality/efficiency; and product comprehension. The labeled corpus includes Find/Verify/Sweep/Rank, codebase audit, cross-repo plan synthesis, bounded migration, flaky-test rounds, research cross-checking, child workflows, signals, steering, task-child invocation, and adversarial cache/nondeterminism cases across real registered local projects. Compare single-agent, manual subagents, Claude same-session workflow evidence, and TraceDecay variants only on equivalent scope/model/tool/budget inputs. Measure terminal correctness/coverage, verified finding precision/recall, duplicate effort, wall time, tokens/cost, critical-path utilization, steering uptake, and human time-to-explain. Public engine or agent benchmarks are context, never product claims.

Promotion gates must be measured on a named reference machine and corpus. Initial targets:

- warm validation of a small workflow p95 below 100 ms excluding engine cold load;
- replay of 10,000 no-effect history commands p95 below 1 s and byte-identical across 1,000 repetitions;
- zero duplicate command admission, zero false terminal success, zero unsafe automatic retry after unknown effect, and zero unexplained duplicate observable effect in the full kill-point matrix;
- 100% detection at the first mismatching tape position for the versioned nondeterminism corpus and zero false mismatch on compatible histories;
- zero incorrect fork/cache reuse across the adversarial authorization/freshness/route/context/effect corpus; every reuse has a source receipt and exact UI/API explanation;
- canonical event-to-subscription visibility p95 at most two seconds after commit; reconnect either resumes exactly or shows stale/resync before controls re-enable;
- daemon/engine-worker restart with no unresolved external effect resumes a 10,000-command run within the recorded recovery target (initial p95 30 seconds); unresolved effects remain explicit rather than extending this denominator;
- run/event API and dashboard remain interactive at 1,000 live nodes and a 10,000-node retained run, 16 active agents, and bounded 10x history;
- pause/cancel/steering safe-boundary acknowledgement within the host capability SLO or explicit unsupported/deferred state;
- shared-scheduler load proves one max-width workflow cannot starve interactive/task/automation work beyond their configured fairness SLO;
- equivalent-quality 1/4/8/16-agent fixtures show useful critical-path speedup for parallelizable workflows, bounded p99/RSS/tokens/cost and duplicate effort, and no regression for intentionally serial workflows; a wider workflow that only adds waiting, context, or cost fails promotion;
- slow-but-connected provider-call, oversized tool output, and context-window rejection fixtures reach typed bounded-request outcomes; missing workflow heartbeat becomes visible without automatic termination, and explicit cancellation-loss reaches typed reconcile/block state without silently claiming completion;
- zero unexplained taskgraph nodes/edges in candidate-compilation fixtures.

Benchmarks compare engine candidates, cold/warm compile/evaluate, replay, fan-out scheduling, schema validation, history growth, API/SSE, and dashboard rendering. A public JavaScript-engine benchmark score is not TraceDecay evidence.

## 20. Deterministic, adversarial, and fault tests

Required fixture families:

1. **Source/compiler goldens:** JavaScript and TypeScript artifacts, `tdwf-js-wrapper-v1` meta/body split and exported async entrypoint, terminal top-level-return lowering, synthetic-wrapper/original-byte source maps and callsites, Claude-compatible globals, virtual SDK binding, exact compiler/SDK/schema manifest, daemon recompilation authority, local-component digest/ABI mismatch, proof SDK packages contain no compiler implementation, no arbitrary bundle/import, bytecode exclusion, and cross-platform byte-identical artifact digests.
2. **DSL goldens:** meta/args, nested phase, agent, parallel success/failure modes, keyed pipeline, child workflow, timer, signal/timer race, checkpoint, continue-as-new, log, return, schemas, diagnostics, source maps.
3. **Forbidden runtime:** filesystem, network, shell, env, process, module/package loader, dynamic import, WebAssembly/native module, wall clock, locale/timezone, random, `Promise`/custom thenable/microtask/timer globals, detached async, unbounded loop/promise/job/memory/stack, host reflection, `eval`/`Function`.
4. **Engine/placement:** exact Boa/rquickjs/upstream/features/allocator candidates, Test262 subset, limits, interrupts, job quiescence, leak slope, panic/abort/OOM/stack faults, daemon containment, helper crash/restart, three OS release artifacts, and 1,000 cross-engine replays.
5. **Replay/history:** identical command tape, out-of-order addressed results, deterministic parallel batches/output order, daemon restart at every command boundary, pending activity/timer/signal, log dedupe, snapshot discard/overlap, mismatch kind/order/key/input/schema/dependency, changed definition/compiler/engine ABI, version fork, history cap/continue-as-new, sealed page traversal under concurrent appends, cursor tamper/access drift/retention tombstone, terminal-seal stability, and byte-identical CLI/HTTP/SDK/MCP-resource/Studio pages.
6. **Effects:** kill before/after command admission/outbox/send/ack/result/terminal commit, idempotent/at-least-once/non-repeatable adapters, unknown provider/tool effect, safe retry attempt, cancellation, stale executor, duplicate/reordered delivery.
7. **Schemas/values:** dialect/vocabulary/meta-schema, internal refs, remote/dynamic refs rejected, invalid args/output, repair exhaustion, recursive/oversized/unsupported schema, regex/format drift, nonfinite/negative-zero/sparse/cyclic/custom values, duplicate object keys, canonical JSON parity.
8. **Capacity:** 0/1/16/max concurrency, 1,000 live and 10,000 retained nodes, depth/cardinality/child/signal/history caps, fairness with interactive/tasks/automation, rate limit, backpressure, budget exhaustion.
9. **Cache:** mandatory same-run history, explicit selected fork, cross-run disabled, exact hit, mutating-effect exclusion, stale watermark, changed route/model/tool/config/context, unknown effect, unauthorized/tombstoned source, explanation parity.
10. **Source discovery/names:** exact repository/profile roots, watcher create/change/remove/reorder, changed import generation, same-name cross-scope ambiguity, no CWD/nearest precedence, Git/worktree drift, idempotent import, export/write/reobserve loop, multi-machine candidate divergence.
11. **Steering/signals:** mid-turn required/advisory, unsupported host fallback, one Stop continuation, duplicate/stale acknowledgement, expiry, late terminal race, plain comment non-injection, signal schema/timer/cancel race, and proof signal bytes never become steering implicitly.
12. **Taskgraph:** static eligible graph, frozen executed dynamic manifest, unsupported loop/timer/signal, combined task/workflow cycle, complete node disposition/loss report, candidate-only output, plan-24 review/activation, edited-candidate divergence, task invokes workflow cancellation/fence/retry.
13. **Remote:** authority failover, partition, old-owner return, outbox frontier, duplicate receipt, incompatible engine/compiler/source artifact, remote executor crash, effect unknown, multi-clone scope.
14. **Surface/Studio parity:** CLI Markdown/JSON, HTTP/OpenAPI, compact MCP/resources, Rust/TS/Python SDK, plugin commands, SSE resume/gap, dashboard snapshot/controls all represent the same sealed run; large graph/timeline/table counts and accessibility remain equal.
15. **Find/Verify/Sweep/Rank:** reproduce the inspected Claude workflow shape and compare findings, phase/node graph, schema enforcement, durable resume, cost, transcript/code/PR links, and quality against equivalent manual/single-agent baselines.

Property tests generate legal/illegal state-event pairs; unspecified transitions reject with zero mutation. Fuzz source parsing, schemas, history decoding, call keys, and taskgraph compiler inputs. Golden histories remain versioned migration fixtures.

## 21. Implementation slices and dependency graph

```text
reconciliation gate
       |
       v
PR 38A  frozen contracts + conformance/placement evidence
   |
   +------------------+
   v                  v
PR 38B  domain/store  PR 38C  source compiler + selected engine adapter
   |                  |
   +---------+--------+
             v
          PR 38D  application replay + shared execution integration
             |
      +------+------+------+------+
      v      v      v      v      v
    38E    38F    38G*   38H    38I†
    API    CLI    Studio task-   steering/remote/
    SDK    MCP           graph   accounting
      \      \      \      /      /
       +------+------+-----+
              v
           PR 38J  qualification and accepted defaults
              |
              v
           PR 38K  import, cutover, deletion

* 38G depends on 38E's generated view/SSE contracts; it may develop against frozen fixtures after 38E's schema commit but cannot merge first.

† 38I may develop fixture-only adapters after 38D, but merge eligibility additionally requires the canonical host-safe-boundary contracts from PR 24F and PR 24P, remote placement/routing contracts from PR 24S, and accounting emitter/view contracts from PR 22F-LE and PR 30J. It consumes those owners; it does not recreate them.
```

### PR 38A — frozen contract and engine evidence

- Freeze `WorkflowSourceArtifactV1`, the command tape/history machine, call identity, JSON-value/schema profile, compiler/SDK ABI, `WorkflowProgramPort`, engine profile/placement manifest, and cross-plan ownership tests before implementation consumers merge.
- Build an isolated Boa/rquickjs-upstream-QuickJS/qualified-QuickJS-NG comparison; select engine and in-process versus supervised-worker placement only from measured determinism, containment, portability, latency, RSS, allocator, interrupt, and release-artifact evidence.
- No script execution reaches production effects.

### PR 38B — durable history and storage

- Add immutable definition/source-artifact versions, operation/run/node extensions, canonical command/history events, projections, blobs, indexes, idempotency, backup/restore, atomic outbox admission, and fault tests through plan 02.
- No new database/event journal/outbox/scheduler.

### PR 38C — authoring and root runtime

- Implement the root-private source-root observer/import compiler, JavaScript normalization, TypeScript transpilation, source-map/callsite pipeline, selected engine/placement adapter, forbidden-global realm, bounded job handling, bundled virtual SDK, and engine diagnostics.
- Add exact source/compiler/SDK/schema/engine/placement manifest, frozen wrapper lowering, one root-owned compiler release component/framed ABI, source-name ambiguity handling, no-CWD discovery tests, cross-platform artifacts, and ephemeral-cache invalidation. Generated packages contain declarations/adapters only; never persist engine bytecode or opaque continuations.

### PR 38D — application orchestration

- Wire definition/version/import/start/resume/fork/signal/control commands, command-tape replay, shared scheduler, generic operations, `ExecutionUnitV1`, model/tool/context routes, budgets, capacity, pause/resume/cancel, explicit fork, same-run history, cross-run cache decisions, and receipts.
- Prove tasks/automation/workflows share fairness and executor registrations.

### PR 38E — API and public SDKs

- Generate catalog/OpenAPI, generic-subscription SSE, sealed history-page HTTP/SDK binding, Rust/TS/Python clients, immutable source import/export, and operation/retrieval anchors; add upload containment, snapshot/delta/gap/reconnect, frozen-page cursor, split start/resume/fork, and parity tests.

### PR 38F — CLI, MCP, and host bundles

- Add generated CLI and compact three-tool MCP/resource profile; project stable definition/version bindings into Codex/Claude/Cursor/Hermes commands/skills without source forks, implicit activation, or MCP-only semantics.

### PR 38G — Workflow Studio

- Implement definition/source candidate views, compiled graph, live phase/DAG/timeline/command-tape inspectors, signal inbox, steering rail, node inspector, controls, cache explanation, generic Experiment compare/replay, and large-run/accessibility suites against 38E contracts.

### PR 38H — taskgraph candidate compilation

- Add pure eligibility/loss analysis, candidate compiler, PlanVersion review/activation handoff, provenance/divergence/cycle tests, and task-invokes-workflow path. The slice cannot activate tasks or expose `materialize`, `preview`, `apply`, or `rollback` aliases.

### PR 38I — cross-cutting intelligence

**Ordering:** fixture-only development may begin after PR 38D. Merge eligibility requires `PR 24F + PR 24P + PR 24S + PR 22F-LE + PR 30J`; PR 38I then joins PR 38E–38H as a prerequisite of PR 38J.

- Integrate structured steering/dispositions at host safe boundaries, distinct signals/comments/Plan-22 advisory suggestions, one-shot Stop/SubagentStop continuation policy, remote executor routing, observability/accounting, and graph/timeline/search projections.

### PR 38J — system qualification

- Run deterministic replay, engine-worker crash/OOM/interrupt kill matrix, 10,000-retained-node UI scale, many-host failover, route/effect/cache, schema/compiler drift, steering/signal races, cross-surface parity, and Find/Verify/Sweep/Rank corpora.
- Publish measured engine/placement/default-limit/SLO decisions with exact manifests and retrieval anchors; no configuration becomes default from external benchmarks alone.

### PR 38K — import, cutover, and deletion

- Inventory provider-native `.claude/workflows`, Agent SDK workflow observations, saved plugin commands, session-generated scripts, legacy application operation-workflow terminology, and ad-hoc automation/plugin orchestration. Captured runs remain observations; static Plan-09 operation workflows remain internal recipes; neither is silently promoted to a dynamic definition.
- Import only by explicit source candidate ID, generation, digest, scope, and requested alias. Never scan the current directory, choose a nearest file, auto-activate an observed change, or resolve a same-name collision by project/profile precedence.
- Shadow each legacy callable surface against the canonical definition/version, command tape, execution-unit, operation, event, and receipt contracts. Record semantic/output/cost/fault parity plus the exact accepted exception set before routing traffic.
- Cut over independently generated HTTP/SDK, CLI, compact MCP/resource, host-plugin, Studio, and taskgraph-candidate surfaces. A surface can roll back only within its retained compatibility window; run histories and immutable versions never roll back.
- Delete duplicate workflow status/event streams, engine/compiler paths, schedulers, retry/cache loops, executor envelopes, host-specific workflow sources, ambiguous generic workflow types, and old aliases only after zero live/saved/catalog/config references, retention expiry, migration receipts, and rollback-window closure are proven. Tests must reject deleted names and prove exported canonical source can be re-imported explicitly.

Every implementation candidate receives exact-SHA independent spec review, remediation/successor review if needed, canonical integration, and rollback/observation receipts. Candidate branch completion never activates a slice.

## 22. Required companion-plan reconciliation

Before PR 38A becomes executable, the canonical V2 plan branch must update:

| Plan | Required change |
|---|---|
| master plan | Add dynamic workflows to product outcome, architecture diagram, entity graph, dashboard, implementation phases, and PR 38 dependency order. |
| plan 00 | Register plan 32 as implementation authority, reading paths, dependency rules, slice inventory/source-set digest. |
| plan 01 | Reserve `WorkflowDefinitionId`/`WorkflowDefinitionVersionId`/`WorkflowRunId`/`WorkflowPhaseId`/`WorkflowNodeId`/`WorkflowCommandId`/reuse IDs exclusively for Plan-32 native dynamic workflows; provider-native records use `OrchestrationObservationV1` identities and static operation workflows use `OperationId`/`OperationStepId`. Replace or migrate ambiguous generic `WorkflowId`/`WorkflowStepId` aliases, then add refs, events, states, relations, and the `ExecutionUnitV1` distinction. |
| plan 02 | Rename provider-capture `workflow_runs` to `orchestration_observations`, reserve native `workflow_runs` for Plan 32, and add exact physical extension/projection schemas through existing activity/operation/event/outbox families plus migration/fault/backup gates. Provider observation IDs cannot migrate into native run IDs. |
| plan 07 | Add workflow lifecycle/steering safe-boundary hooks without script execution. |
| plan 08 | Register workflow use cases, schemas, engine/executor capabilities, MCP profile metadata, and generated docs. |
| plan 09 | Rename static cross-shard recipes to operation workflows; add dynamic replay/run/control/taskgraph-candidate use cases over the existing scheduler and generic operations without a second `WorkflowDefinition`. |
| plan 10 | Bind HTTP/SSE/upload/generated TS operations; forbid server paths and parallel event streams. |
| plan 11 | Add Workflow Studio/Run Graph/Compare/Replay/taskgraph-candidate routes and comprehension/performance gates. |
| plan 12 | Own engine dependency/adapter, daemon supervision, diagnostics, local cache/source containment, migration/cutover. |
| plan 13 | Replace provider-capture `WorkflowRunId` research facets with `OrchestrationObservationId`; native workflow runs remain separate typed anchor/relation targets. |
| plan 16 | Split provider `OrchestrationObservationId`, native `WorkflowDefinitionVersionId`/`WorkflowRunId`, and static `OperationId` scope identities. |
| plan 17 | Add public SDKs, `@tracedecay/workflow`, docs/examples/sandbox/conformance. |
| plan 19 | Add crate/dependency/entropy rules and duplicate orchestrator/scheduler deletion checks. |
| plan 20 | Register every workflow setting and four-axis state. |
| plan 21 | Add CLI/MCP/resource/presentation parity and progressive-disclosure limits. |
| plan 22 | Keep suggestions advisory; permit workflow evidence consumption without scheduling authority. |
| plan 24 | Define shared executor envelope, workflow↔task provenance, explicit candidate compilation and task-invokes-workflow, cycle/lease/steering rules. |
| plan 26 | Add workflow metrics/SLOs/accounting dimensions and unknown-denominator rules. |
| plan 27 | Project workflow commands/skills/hooks into each host from one source IR. |
| plan 28 | Add owner-shard workflow authority, remote executor, failover/reconnect, and coverage behavior. |

The reconciliation gate must prove no duplicate owner heading or PR ID, regenerate architecture views, update all source digests, and add Plan 32 to the machine-readable canonical slice DAG.

## 23. Definition of done

- [ ] `tracedecay-workflow` exists with the allowed dependency boundary and pure deterministic replay kernel.
- [ ] Engine selection is backed by a reproducible TraceDecay conformance/containment/portability/performance report.
- [ ] Scripts have no ambient I/O/nondeterminism; all effects route through cataloged application activities.
- [ ] Immutable definition versions and canonical history resume after daemon/host crashes without duplicate effects.
- [ ] Canonical source discovery/import/export has no CWD precedence, implicit activation, mutable-name execution, or durable engine bytecode.
- [ ] One root-owned compiler component produces authoritative artifacts; SDKs contain no compiler implementation, local validation is advisory, and wrapper lowering/source maps are ABI-golden tested.
- [ ] JavaScript/TypeScript authoring supports meta/args/phase/agent/parallel/pipeline/log/return with JSON-Schema outputs.
- [ ] CLI, HTTP/SSE, MCP, Rust/TS/Python SDKs, and host bundles share generated semantics and views.
- [ ] Sealed command-tape/history paging is one application read with byte-identical HTTP/SDK/CLI/MCP-resource/Studio traversal and no live-page drift.
- [ ] Workflow Studio renders and controls definitions/runs/nodes at required accessibility and scale gates without browser execution.
- [ ] History, fork, and cross-run reuse are distinct, pinned, explained, and adversarially tested.
- [ ] Structured steering works at safe boundaries; plain comments and Plan-22 suggestions never impersonate required steering.
- [ ] Ordinary workflows remain distinct from tasks; explicit compilation emits only reviewed candidate taskgraphs with provenance and loss reports.
- [ ] Tasks can invoke exact workflow versions without duplicate lease/scheduler/executor authority or cycles.
- [ ] Remote/many-host, fault, replay, accounting, privacy-reference, and cross-surface parity gates pass.
- [ ] Legacy/provider workflow sources, static operation workflows, and duplicate orchestration paths are classified, explicitly imported where legal, shadowed, and deleted only after accepted per-surface cutover receipts and zero-reference proof.

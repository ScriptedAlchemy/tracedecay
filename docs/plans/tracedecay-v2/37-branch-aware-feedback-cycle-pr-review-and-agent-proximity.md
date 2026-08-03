# TraceDecay V2 Branch-Aware Feedback Cycle, Read-Only PR Review Ingestion, and Agent Proximity

## Status and existing foundation

Completion and activity status is owned solely by
[the plan-set index](00-plan-set-index.md). This component plan defines
retained delivery requirements and does not infer milestone status from branch
artifacts.

PR9 provides the required canonical branch/commit snapshot and read-only
diff/hunk contract. PR11 requires Plan 09's transport-neutral one-shot feedback
request/result. PR12 requires post-edit diagnostics and impact through
LSP/MCP/CLI/HTTP, including the negotiated TraceDecay LSP context extension.
PR13 requires all four advisory pillars together through real host surfaces:

1. branch-aware post-edit diagnostics and impact;
2. CI-failure localization;
3. read-only ingestion and surfacing of existing GitHub review threads; and
4. concurrent-agent proximity.

Plan 09 remains the sole result/orchestration authority. Plans 05, 13, 21, 22,
27, 35, and 36 continue to own query evidence, durable anchors, rendering and
expansion, suggestions, host delivery, semantic/LSP providers, and Git
identity. This plan does not replace any accepted capability from those plans.
Plan 09's feedback cycle and canonical feedback-read application owner are the
required mechanism. Implementations visible on a branch are evidence of that
contract without establishing milestone status. Historical packet/type names,
exact source layouts, schema registries, and fixture matrices are evidence only
unless separately declared public or persisted API.

The PR12 feedback-read and TraceDecay LSP extension payloads have no
predecessor on `origin/master` or in a published package/release. Branch-local
V2 payloads and durable feedback findings, review snapshots, cursors, journals,
checkpoints, and receipts change in place. Persisted feedback accepts only its
exact final shape; any other database, store, spool, file, or projection returns
typed `ResetRequired` and requires explicit reset or recreation. No storage
reader, migration, backfill, dual write, or census path exists; branch history,
an experimental version tag, or a fixture alone is not release evidence.

## PR12 baseline reader and LSP context contract

PR12 must make canonical diagnostics, impact, affected-test, test-result,
feedback get/list, and exact-expansion reads callable; no placeholder or
advertised-unavailable handler satisfies that milestone. CLI, MCP, and HTTP
share the same reader semantics. Dashboard binding starts in PR14.

Plan 35 owns the versioned TraceDecay LSP context extension and carries it over
standard LSP/JSON-RPC framing after explicit experimental capability
negotiation. PR12 must project current diagnostics, impact, affected tests, and
test results in compact envelopes that retain exact authorized scope,
content/graph generation, producer state, coverage, omissions, and opaque
expansion handles. Handles reauthorize into Plan 21's canonical reads and are
neither durable evidence identity nor LSP-owned data. The extension accepts no
arbitrary method or payload forwarding.

Its typed provider contribution point admits only a callable cataloged
producer. PR13 registers GitHub review, CI localization, and proximity
contributions through that point without changing reader transport, framing,
or ownership; unavailable producers remain typed unavailable contributions.

**PR12 direct acceptance.** A real negotiated client receives diagnostics,
impact, affected-test, and test-result projections, expands an omitted item,
and proves scope/generation/coverage fidelity, cancellation, stale suppression,
authorization recheck, and rejection of unnegotiated versions, arbitrary
methods/payloads, and cross-scope handle reuse. CLI/MCP/HTTP calls return the
same canonical diagnostics/impact semantics, with no dashboard dependency.

## PR13 user outcome

After a saved edit or agent stop boundary, a user receives current
diagnostics, affected callers/files/tests, relevant existing PR review
findings, localized CI failures, and advisory overlap with concurrent agents.
The same evidence is available by explicit MCP/CLI request and, where
supported, through LSP or native diagnostics. Results are bounded and
expandable, never auto-applied, and never cause a GitHub write, CI rerun, task
mutation, or agent continuation.

Feedback never waits for repository or FastEmbed reindexing. A cycle freezes
the latest complete compatible generation already available for its exact
worktree and reports indexing/stale/partial coverage separately. Exact,
lexical, graph, Git, diagnostics, test, GitHub, CI, and proximity evidence
remain independently usable; semantic evidence is omitted until its complete
compatible vector generation atomically publishes.

## End-to-end production journeys

### One-shot edit and stop feedback

1. A saved-file hook, IDE save, explicit diagnostics call, or stop/pre-stop
   gate invokes one Plan 09 cycle bound to exact project, repository,
   worktree, branch, head/merge-base commits, generation or authorized overlay,
   changed files/ranges/symbols, session/agent/turn, policy/configuration, and
   budgets.
2. The daemon classifies new versus pre-existing diagnostics and composes
   semantic, graph-impact, affected-test, branch/diff, GitHub, CI, and
   proximity evidence. It does not join background parse, graph, lexical, or
   vector jobs. A newer saved-content frontier may schedule a later generation
   while this bounded cycle continues against the frozen ready generation.
3. Plan 22 may select one inert suggested next action from the same evidence.
4. The cycle terminates once with exactly one reason: clean,
   duplicate-no-op, blocked, incomplete coverage, stale/replan required,
   budget exceeded, cancellation, user stop, or daemon unavailable.
5. Plan 27/35/21 render the same result on the supported host surface.
   Silence is a normal result; adapter silence never invents a termination
   reason.

There is no repeating edit-fix loop. A later edit or explicit request starts a
new cycle. No cycle emits a host follow-up message, schedules another turn,
applies an edit, or performs a refactor.

### Read-only GitHub review refresh and surfacing

1. An authorized operation requests existing reviews, threads, comments, and
   replies for an exact repository and pull request.
2. Plan 27's adapter performs structurally read-only acquisition. REST permits
   only `GET`; GraphQL permits only the shipped static allowlisted `query`
   documents.
   Mutation operations, write-capable client methods, and write-capable or
   indeterminate credential scopes fail before network access.
3. Canonical capture preserves provider repository/PR/base/head/merge-base,
   review/thread/comment/reply/version, observed author class, body digest and
   retained evidence anchor, original/current position, safe URL, cursor,
   ETag, permission, rate limit, freshness, and coverage.
4. Plan 05/36 remap each item against the current immutable branch snapshot.
   The original observed thread is never rewritten; remap creates a derived
   anchored projection.
5. Current authorized findings receive semantic callers, implementations,
   affected tests, branch diagnostics, and CI context and then enter the same
   Plan 09 cycle result.

Each item retains an independent lifecycle:
`current | outdated | resolved | edited | deleted`.
Each refresh independently retains one provider outcome:
`complete | partial | unavailable | denied | rate_limited | stale | failed`.
The dimensions are never collapsed. Path or line similarity alone cannot make
an item current, and partial/stale/unavailable refresh cannot become clean
empty.

GitHub titles, descriptions, commit messages, review prose, severity claims,
author labels, and approval state are observed framing, not correctness or
trust authority. Frame-neutral code, graph, compiler, test, CI, policy, and
configuration evidence is evaluated independently.

TraceDecay never posts, updates, resolves, dismisses, reacts to, or replies to
a GitHub review item. There is no write grant, write receipt, autonomous mode,
supersession-as-write state, or deferred write path in any PR.

### CI-failure localization

1. An explicit request provides exact CI provider/repository, workflow/job/
   suite/run/attempt/check identity, evaluated head commit and branch, retained
   log/artifact anchor, excerpt digest, parser/version, event time, failure
   kind/file/line/test, confidence kind, permission, rate-limit, freshness,
   and coverage.
2. The daemon maps the reported failure to the exact branch generation,
   current symbol, callers, affected tests, and a targeted rerun hint.
3. The result reports stale, partial, denied, unavailable, or failed source
   state rather than fabricating localization.

CI remains authority for execution and pass/fail. TraceDecay never claims to
have run, verified, retried, or influenced CI; rerun hints are inert.
Confidence labels distinguish ordinal rank, heuristic score, calibrated
probability, and calibrated interval, while coverage remains separate.

### Concurrent-agent proximity

1. The daemon consumes existing authorized agent/session/worktree/branch
   observations and Plan 05/25 file, range, symbol, package, call, dependency,
   and test neighborhoods. It creates no second presence or graph model.
2. Exact same-file/range/symbol conflicts use the immediate tier.
3. Package/crate overlap, shared callers/dependencies/tests, incompatible
   branch/worktree state, and overlapping planned workspace changes use the
   configured threshold tier. The cycle pins Plan 20's effective
   `feedback.proximity.risk_threshold`, revision, and digest; adapters have no
   local default.
4. Every warning retains risk inputs, relation paths, observation anchors,
   freshness, expiry, coverage, inclusion/suppression reason, and authorized coarse
   address. Below-threshold is a successful silent outcome.
5. Delivery reveals only the fact and coarse authorized shape of overlap.
   Hidden actors, sessions, roots, counts, paths, branches, and content remain
   undisclosed.

Proximity is advisory. It creates no lock, task, assignment, lease, schedule,
workflow, peer message, or execution authority.

## Delivery on supported surfaces

The four pillars share one Plan 09 result across:

- hooks at edit/stop boundaries;
- the explicit MCP operation;
- the HTTP feedback read operations required in PR12;
- LSP Problems or Cursor native diagnostics where a current range exists;
- dashboard/Doctor consumption in PR14; and
- the CLI operation shared with MCP.

No IDE must be open for hook, MCP, CLI, or dashboard access. LSP is an
evidence source and editor projection, not the universal transport.

LSP projection uses the current remapped range, a producer-specific source,
stable finding code, an authorized credential-free source URL when safe, a
bounded data allowlist with finding/anchor/lifecycle/provider/coverage
identity, and bounded related locations. It never embeds thread bodies, reply
text, logs, source, diffs, task narrative, cursors, or full evidence.
Resolution, deletion, authorization loss, head/content/generation drift, and
supersession clear or republish monotonically. Severity never exceeds source
evidence.

Delivery timing, expiry, interruption reason, acknowledgement, inspection,
deferral, dismissal, verification, action, contradiction, and unknown remain
distinct. Display, click, acceptance, override, or later success never proves
correctness or adoption.

Explicit retrieval feedback is limited to helpful, stale, irrelevant,
contradictory, or unknown for the exact query/result/contribution and current
authorization/policy revision. Display, click, acknowledgement, expansion,
deferral, override, task completion, or comment resolution remains interaction
evidence only and never becomes a correctness label.

## PR13 implementation slices

### Concrete GitHub and CI defaults

- Use existing `ureq` transport and one shared set of typed Serde DTOs for
  GitHub review and CI responses. Keep exactly one GraphQL document as
  compile-time static audited query text with fixed response DTOs. This
  replaces provider-local HTTP/JSON wrappers and dynamic GraphQL parsing
  without changing exact
  repository/PR/run identity, overlap pagination, cursor publication,
  freshness, coverage, rate-limit, lifecycle, or read-only semantics.
- Keep `gh api` as a documented manual acquisition/troubleshooting fallback,
  not a daemon dependency or alternate product path. If a checked-in provider
  fixture exposes schema drift or a required field cannot be decoded, retain
  the last complete generation and return typed partial/stale/unavailable
  evidence until the DTO/query is updated.
- Do not add Octocrab, `backon`, or `graphql-parser`. Existing bounded retry
  remains explicit in the refresh owner; admit a replacement only if it
  deletes that owned mechanism without obscuring `Retry-After`, cancellation,
  quarantine, or publication rules.

### Complete the four-pillar cycle

- Connect real edit/stop events and explicit requests to the one Plan 09
  operation.
- Add GitHub refresh/remap, CI localization, and tiered proximity producers.
- Produce one reference-only canonical feedback evidence result carrying exact
  scope, generation, finding IDs, Plan 13 anchors, source state, freshness,
  coverage, watermark, counts, omissions, budgets, and cursor. Its concrete
  type and module placement follow the current Plan 09 result owner rather
  than reviving a deleted packet contract.
  It copies no source, review body, CI log, diagnostic payload, or private
  session content.
- Produce a reference-only proximity contribution with warning/risk/threshold
  provenance. Task linking and proximity fusion-rank influence remain disabled
  until their callable PR17 journeys ship. Expertise is never part of this
  retrieval/ranking path; PR17 may separately pass authorized anchored inputs
  to Plan 24's ephemeral expertise operation.

### Keep evidence lossless and projections bounded

- Every durable finding has a stable finding ID plus `RetrievalAnchorId`.
  Transport response handles never replace durable identity.
- Large threads, logs, semantic context, and proximity evidence return a safe
  snippet plus cursor/watermark, total/returned/omitted counts, applied budget,
  truncation reason, coverage, and authorized expansion.
- Authorization is rechecked on every page and expansion. Expired, deleted,
  redacted, missing, corrupt, denied, and retained states are typed and do not
  become silent empty results.
- LCM/session narrative may aid discovery but never replaces canonical
  GitHub, CI, diagnostic, Git, or branch evidence.

### Keep overlays session-local

Unsaved-overlay feedback is immediate, session-only, and visible only to the
authorized owner. It cannot enter durable state, telemetry, fixtures, exports,
remote transport, GitHub evidence, task relations, or workflow input. Durable
feedback requires exact saved-content or clean-generation identity.

## Replacement and deletion

- Remove duplicate owner matrices, repeated authority prose, exact source-file
  and schema inventories, standalone milestone gates, giant lifecycle ×
  provider fixture matrices, and placeholder benchmark packets.
- Remove reserved PR15/PR17 fields from PR13 schemas. Later callable
  operations add inputs to the current writer shape. An actually independently
  released public protocol may negotiate its documented revision at the
  transport boundary; persisted feedback never gains a prior-shape reader and
  returns `ResetRequired` when non-final.
- Remove adapter-local findings, proximity fanout/dedupe, suggestion streams,
  task linking, and evidence stores. The canonical owners above remain.
- Do not remove or reduce any pillar, surface, lifecycle/provider state,
  evidence/expansion behavior, safety boundary, or compatibility obligation.

## PR13 direct acceptance

- One real branch/PR scenario exercises a saved edit and stop boundary and
  simultaneously returns post-edit diagnostics/impact, a localized CI
  failure, an existing GitHub review finding, and a concurrent-agent proximity
  warning.
- Hook, MCP, CLI, Claude LSP, and Cursor native-diagnostics projections are
  semantically equivalent where capabilities overlap; unavailable surfaces
  are explicit and no IDE is required for non-LSP delivery.
- Duplicate triggers, daemon restart, branch/head/generation drift,
  cancellation, budget exhaustion, analyzer failure, and authorization loss
  return their exact typed state and never clean-by-silence.
- GitHub lifecycle and provider outcomes remain orthogonal across current,
  outdated, resolved, edited, deleted, complete, partial, unavailable, denied,
  rate-limited, stale, and failed cases. Rejected writes make zero network
  calls.
- GitHub/CI provider acceptance uses checked-in real native fixtures with
  recorded origin/version/digest, replayed through the real sanitizer and
  consuming path. Synthetic, lookalike, or invented protocol fields are
  non-binding. Those fixtures localize current supported input and preserve
  stale, partial, unavailable, and denied source state without exposing raw
  logs or claiming execution.
- Proximity fixtures cover immediate and above/below-threshold cases, pin the
  effective Plan 20 setting, expire/dedupe correctly, reveal no hidden peer,
  and create no lock or schedule.
- Saved-content and dirty-overlay cases prove exact anchor identity through
  durable sinks and no overlay persistence.
- Suggestions remain inert across every surface; no cycle edits source, writes
  GitHub, reruns CI, mutates Git, schedules work, or continues an agent.

## Later callable extensions

### PR14: dashboard, Doctor, and observability

Dashboard and Doctor call the same shipped feedback list/get/expand/status
operations. Plan 26 records system-quality metrics—coverage, relevance,
diversity, latency, omissions, denial, stale rate, revocation propagation, and
stack transitions—never worker-performance metrics. PR14 does not become first
availability of any PR13 pillar.

### PR15: multi-root feedback and stack signals

Plan 16 extends every pillar to independently authorized roots. Results retain
per-root coverage before merge and never reveal denied root identity or count.
GitHub/CI routing verifies canonical repository and immutable commit identity.
Overlaps with hidden roots use only an explicitly policy-safe coarse shape or
remain silent.

One daemon-local stack coordinator adds callable snapshot/expand behavior for:

- dependency-ready commits;
- actual and potential conflicts;
- upstream stack-tip and base drift;
- pull-request base/head/merge-base drift;
- CI evaluated-commit drift; and
- integration committed or needs-inspection receipts.

The coordinator consumes only Plan 16/36 authorized stack/worktree/Git
snapshots, read-only PR/CI observations, and existing graph/presence evidence.
Adapters never compare tips, schedule preflight, select recipients, persist
dedupe, or fan out locally.

For a new committed tip or stack revision, the coordinator may call Plan 36's
read-only preflight over visible declared edges with bounded concurrency,
in-flight joins, cancellation, and exact snapshot pinning. Fanout is
authorization-checked at enqueue, delivery, and expansion; batches are
deterministic, bounded, watermark-backed, and drop no state transitions.
Actual conflicts and authorization loss bypass debounce;
potential conflicts, readiness, and drift use centralized dedupe/debounce.
Restart restores durable clean-state watermarks; overlay state remains
memory-only.

Retain the accepted bounds and timing: at most four preflights per repository
and 16 per daemon; 64 recipients and 128 signals per fanout batch; 250 ms
debounce for dependency-ready and potential-conflict bursts; 1,000 ms for
stack/PR/CI drift; and a five-minute dedupe TTL. Actual conflict,
needs-inspection, authorization loss, and every
material state transition bypass debounce. Overflow drains in deterministic
batches instead of dropping transitions.

The optional preflight fanout has a Plan 20-versioned circuit breaker. Repeated
timeout, saturation, cancellation, or unavailable-native-state outcomes open
it for the exact repository/scope and suppress only new optional preflight
work; durable watermarks and material transitions continue, and delivery
reports degraded/partial coverage. A bounded half-open probe may close it
after current scope, authorization, stack revision, and native state are
revalidated. Adapters cannot reset or bypass it.

**PR15 direct acceptance.** Burst tests exceed each debounce window,
repository/daemon preflight limit, recipient/signal batch limit, and queue
capacity. They prove 250 ms and 1,000 ms classes coalesce independently;
actual conflict/revocation/needs-inspection transitions bypass debounce; no
more than four repository or 16 daemon preflights run; batches stay within
64 recipients/128 signals; overflow drains deterministically with every
material transition and truthful watermark/coverage; and restart preserves
clean-state ordering. Repeated injected failures open the scoped circuit
breaker without blocking non-preflight delivery, half-open permits only the
bounded probe, success closes it, and denied/stale scope never does.

A mechanical suggestion exists only for Plan 36
`MechanicalIntegrationEligible` preview and remains inert. Applying it
requires a separate exact Plan 36 approval and operation. Native/semantic
conflict or incomplete coverage always escalates for inspection. Plan 37 never
calls apply, auto-resolves conflict, reruns CI, writes GitHub, or continues an
agent.

The optional read-only GitHub Stacked PR adapter reports exactly
`Unavailable | PrivatePreviewDisabled | Enabled | Degraded`. Enabled snapshots
require a same-repository strictly linear stack with provider stack/position,
PR, base/head commit, final target, protection, CI, and merge-queue evidence.
Partial visibility or broken topology falls back to standard PR/Git evidence.
TraceDecay observes GitHub's atomic lower-layer merge and subsequent rebase/
retarget state but never invokes cascading rebase, push, submit, sync, modify,
force update, or provider mutation. An explicitly authorized external handoff
may ask the human/provider to perform the recognized `init`, `add`, `rebase`,
`push`, `submit`, `sync`, `modify`, `unstack`, `checkout`, or selected-layer
merge operation; ingestion then observes the new snapshot. The handoff is not
provider execution authority and never weakens Plan 36's prohibition on
automatic rebase or force-push.
Standard Git and other forges remain supported when the preview is absent.

### PR16: remote authority

Unsaved overlays and proximity computation remain on the node that owns the
live workspace. Durable feedback, GitHub, CI, stack, and delivery state passes
through Plan 28's fenced shard authority. Authority loss returns
partial/unavailable and cannot create a local writer, replicate overlay
content, or present stale cache as current.

### PR17: task evidence and workflow composition

Plan 24 may call one feedback-cycle evidence retriever rooted at exact
`TaskId` and immutable work-item version. A link affordance remains inert
until an explicit version-checked Plan 24 command creates a revisioned
supports/contradicts/risk/overlap/review-input relation. Head, generation,
task-version, authorization, retention, source deletion, anchor, or consent
change appends stale/superseded/revoked state; history is never rewritten and
ambiguous mapping never chooses a task.

The one proximity retriever preserves every source observation, relation path,
candidate/selected evidence ID, score kind, normalized contribution, coverage,
freshness, and inclusion/omission reason. Authorization precedes scoring;
dedupe never drops provenance. Source-diversity policy reports unmet minima or
family-share limits rather than inventing evidence. Proximity may annotate or,
after the retained reversible evidence gate, contribute at most 0.10 normalized
rank; it cannot alter severity, confidence, coverage, source lifecycle, task
identity, readiness, assignment, lease, attempt, effect, or receipt state.
Current/as-of/evolution/forensic reads preserve Plan 24's truthful complete,
partial, no-relevant-evidence, abstained, cancelled, timed-out, failed, denied,
stale, and unavailable outcomes.

Before rank influence, shadow retrieval must prove deterministic replay,
complete contribution provenance, and no wrong-project results on an
adjudicated multi-source corpus. When at least two source families are
eligible, at least two remain represented and no family may exceed 60% of the
top ten; diversity shortfall is explicit. The bounded 0.10 proximity
contribution requires non-regressing held-out nDCG@10, at least five percentage
points of proximity-positive recall@10 gain, under 1% stale selected evidence,
and no more than 10%/25 ms p95 latency regression. Failure disables rank
influence without removing base retrieval or PR13 proximity findings.

Plan 32 may compose the already callable read operation as an explicitly
admitted workflow step and return its normal evidence envelope. It cannot
create task links, infer consent, change rank policy, reactivate stale data, or
introduce a feedback-specific scheduler or GitHub effect.

Plan 37 may supply only authorized anchored expertise evidence input to Plan
24's separate default-off ephemeral operation/view. Eligible input is limited
to exact anchored authored or reviewed commits, causally resolved diagnostics,
independently accepted task outcomes, and anchored discussion contributions,
with attribution ambiguity, coverage, lifecycle, purpose, and consent
provenance intact. It never enters this plan's canonical feedback retrieval,
evidence envelopes, fusion/ranking, proximity score, task links, or completion
state.

Plan 24 owns invocation authorization, consent/revocation, actor-safe
projection, ephemeral retention, handle invalidation, and purge. Plan 37
immediately stops supplying revoked, expired, deleted, unauthorized,
ambiguous, incomplete, prohibited-purpose, or unavailable input and retains no
actor-indexed expertise view, score, leaderboard, export, workflow authority,
or employment signal. Focused tests prove revocation/purge removes eligible
input and cannot change feedback ranking or any canonical task result.

Task retrieval begins in shadow mode. Proximity rank influence, Plan 24's
expertise operation, cross-project cohorts, and stack fanout activate only
after their direct product behavior and rollback path work. Wrong-project
results, attempted GitHub writes, dropped stack transitions, prohibited
purpose, unexplained results, or semantic auto-resolution disable only the
affected extension without disabling base PR13 feedback or canonical
read-only evidence.

## Safety constraints retained

- GitHub is read-only ingress in every PR; no write path exists.
- The cycle is one-shot, advisory, and unable to edit, schedule, execute, rerun
  CI, mutate Git, or continue an agent.
- Dirty overlays are session-only and never durable, replicated, exported, or
  used for GitHub/task/workflow evidence.
- Canonical evidence is lossless, anchored, authorization-checked, and
  reversibly projected with truthful coverage and unavailable states.
- Proximity and expertise never become locks, task authority, trust scores,
  people rankings, or employment evidence.
- Later features ship only through callable owner operations with independent
  rollback; no future-only schema reservation is required.

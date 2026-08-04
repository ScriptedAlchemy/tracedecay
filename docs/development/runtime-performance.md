# Runtime performance harness

The runtime harness measures TraceDecay through the same process boundaries that
agent hosts use. It is a regression instrument, not an SLO gate. Raw samples
remain regression observations until a matching population reaches the
declared percentile threshold; artifacts must keep ineligible percentiles
unavailable.

Its scope is the final V2 runtime: both per-crate lanes and integrated
user-visible journeys. Scenario and artifact identities must not depend on
delivery sequencing. Three stable identities locate every measurement:

- **Crate identity** is the final Cargo package that owns the measured runtime
  boundary, such as the root `tracedecay` crate or an extracted V2 workspace
  crate.
- **Journey identity** is the durable runtime path, such as CLI tool execution,
  persistent MCP, hook handling, host activation, daemon contention, or daemon
  recovery.
- **Workload identity** is the exact operation and fixture input within that
  journey.

Renaming a branch, splitting delivery work, or integrating the same crate later
must not change those identities.

## Safety and prerequisites

Supply an already-built TraceDecay executable explicitly. The harness never
builds with Cargo and never resolves a `tracedecay` executable from `PATH`.
Single-binary commands require `--binary`; `paired` requires explicit
`--baseline` and `--treatment` executable paths. `compare` reads existing
artifacts and does not execute a binary.

Run the harness through:

```sh
scripts/run-runtime-performance.sh capture \
  --binary /absolute/path/to/tracedecay \
  --output /tmp/tracedecay-runtime/capture.json
```

The wrapper preserves every argument byte-for-byte, including spaces and glob
characters. It clears `TRACEDECAY_HOME`, `TRACEDECAY_PROFILE`, and
`TRACEDECAY_PROFILE_DIR` before invoking the standard-library Python runner.
The runner creates disposable homes and profiles beneath the selected output
root. It must not read or mutate the operator's profile, use a live semantic
model, download models, or start a second daemon against an operator profile.
The harness does not run Cargo benchmarks. Build or Cargo benchmarking is a
separate activity and is performed only when separately requested.

`prepare` only validates and copies deterministic fixtures. It must not launch
a daemon. Prepared data includes the checked-in project history and native
Codex, Claude, and Cursor provider layouts so session ingestion is exercised
without reading real host data.

## Commands

- `prepare --binary BIN --output DIR` creates a reusable, immutable fixture
  snapshot in an explicit location.
- `capture --binary BIN --output REPORT` records one variant and writes its raw
  samples as JSONL alongside the aggregate report.
- `paired --baseline A --treatment B --samples-per-variant N --output REPORT`
  runs isolated same-input ABBA rounds (`A, B, B, A`) and retains every raw
  sample. `N` is a positive even count per binary; four is the default and
  remains below percentile eligibility.
- `compare --baseline REPORT --treatment REPORT --output REPORT` checks schema,
  fixture, machine, correctness, and paired latency evidence without executing
  either binary.
- `smoke --binary BIN --output REPORT` runs the smallest contract-complete
  capture for development and CI wiring.
- `incidents --output REPORT` writes the machine-readable final incident
  catalog. Pending product routes remain explicitly unavailable.
- `incident --binary BIN --workload missing-daemon-after-shell --samples N
  --output REPORT` invokes the committed `hook-cursor-after-shell` product
  command against an intentionally absent disposable socket, retaining typed
  unavailable samples and process-tree cleanup evidence. It also measures a
  same-binary `--version` startup control, a direct product-command invocation,
  and the process-owning lifecycle-wrapper invocation. It reports direct hook
  wall time, the non-negative direct-minus-startup residual, and lifecycle
  wrapper overhead separately. The residual reconciles process-launch cost but
  is not claimed as authoritative internal handler timing.
- `incident --binary BIN --workload diagnostic-dedup-batch-rate --events N
  --samples N --output REPORT` starts one disposable owned daemon per sample,
  floods the production `lsp bridge --stdio` route with bounded identical-save
  events, and records observed diagnostic publications, deduplication, queue
  depth, and complete process cleanup. `--authority-test` instead requires the
  explicitly supplied prebuilt `diagnostic_publication_stress` test executable;
  its committed authority fixes `--events` at 10,000 and proves one retained
  publication under backpressure without claiming production-route latency.

All output paths are explicit. Existing output must fail safely rather than be
overwritten or receive another capture. Runner-generated capture identifiers
must remain unique even for back-to-back invocations whose wall clock
timestamps are equal. A host may emit the same capture identifier more than
once for one operation; those occurrences remain separate raw samples with
unique sample identifiers. Conflicting capture identifiers for one operation
are a hard failure.

## Measurement contract

Artifacts use a strict, versioned schema. Raw samples are canonical UTF-8 JSONL
with one complete sample per line and a trailing newline; aggregate reports
carry the raw JSONL digest. Malformed JSON, unknown schema versions, missing
fields, non-finite numbers, digest mismatches, and count inconsistencies are
errors.

Every sample identifies its capture, run, crate, journey, workload, variant,
machine fingerprint, ABBA round and position, surface, and runtime state. The
four surfaces are:

- **CLI**: executable start through complete stdout/stderr drain and exit.
- **MCP**: JSON-RPC request write through matched response receipt.
- **Hook**: host event submission through the complete hook result.
- **Host**: host launch or activation through the user-visible operation.

The harness records these measurements without substituting zero or success for
missing evidence:

- End-to-end wall time from immediately before boundary invocation through
  complete response and stream drain.
- Handler middle-slice time when the transport exposes authoritative handler
  timing. It is nullable when unavailable and is never inferred by subtracting
  unrelated phases.
- Process count for the measured operation, including persistent-process reuse.
- UTF-8 request bytes, response bytes, and their combined payload size.
- Availability as a typed state: available, unavailable, or unsupported.
- Timeout phase, distinguishing activation/readiness, request/handler,
  concurrent stream drain, and shutdown.
- Activation and restart state, including whether activation was already warm,
  newly performed, or required a restart.
- Daemon survival evidence after each success, error, or timeout. Survival is
  verified from process/status evidence, not assumed from a wrapper exit code.
- Linux daemon CPU time, peak RSS, PSS, process disk-read/write bytes, SQLite
  WAL bytes, and integer write-amplification parts per million when `/proc`
  exposes authoritative counters. Unsupported memory-peak and profiler
  overhead counters remain null.
- Typed queue depth/enqueue/shed/cancel/retry, generation, diagnostic
  generated/deduplicated/batch, indexing no-op/coalesced, and
  renderer/consumer event counters. A journey without an authoritative
  production counter remains unavailable rather than recording zero.

An unavailable measurement remains unavailable. An unsupported surface remains
unsupported. Neither may be rendered as a successful zero-duration or
zero-result sample.

## Final V2 lanes and scenarios

The scenario catalog covers cold, first, warm, repeat, and persistent MCP
states and explicitly includes no-op, contention, and recovery journeys:

- **Cold** uses a fresh disposable profile and includes required startup or
  activation.
- **First** is the first operation after readiness and captures one-time lazy
  work.
- **Warm** repeats the operation after caches and indexes are ready.
- **Repeat** detects drift and accidental per-call process or activation work.
- **No-op** proves that an already-satisfied activation, registration, or
  synchronization request performs no unnecessary process or write work.
- **Contention** exercises concurrent callers competing for the same daemon,
  profile lease, MCP process, or bounded resource.
- **Recovery** measures the final V2 path from a typed unavailable, dirty, or
  interrupted state back to readiness without corrupting foreign state.
- **Persistent MCP** sends multiple and concurrent requests through one
  initialized MCP process and correlates out-of-order responses by request ID.

Workloads cover exact lookup, lexical search, graph traversal, query/context,
session and LCM retrieval, provider ingestion, and payload stress. CLI and MCP
include serial and concurrent throughput cases. Hook and host cases measure
their full external boundary rather than reporting only an internal handler
timer.

The final incident catalog additionally covers missing-daemon after-shell
failure and descendant reaping, sustained edit/commit indexing coalescence,
foreground work under maintenance, diagnostic deduplication/batching, daemon
steady-state CPU/memory/WAL/I/O/queue/generation, and renderer-consumer event
counts. The missing-daemon after-shell and diagnostic-flood drivers are
available through committed product commands. Diagnostic-flood sends a bounded
set of identical saves through `lsp bridge --stdio` and counts observed
diagnostic publications after the flood. The remaining entries stay `n=1`,
unavailable, and non-gating until their production routes are mounted. The
paired executable driver currently records the integrated CLI exact-query
lane; MCP, dashboard, storage, maintenance, and renderer-consumer entries
remain truthfully unavailable until their real driver and authoritative
evidence are observable.

Correctness is part of every performance sample. Stable expected-result digests
ignore explicitly volatile timing metadata while preserving each workload's
declared ordering semantics.

Per-crate lanes and integrated journeys use the same raw-sample shape. An
integrated sample may identify multiple participating crates in its metadata,
but its primary crate, journey, and workload identities remain stable and
comparable across captures.

## Runtime-only JUnit normalization

Runtime-only JUnit cases use the stable crate, journey, and workload identities,
then normalize results by **platform**, **shard**, **storage mode**,
**concurrency**, and **cold/warm** state. Two samples match only when all of
those dimensions, the fixture digest, and the comparable-machine identity
match. The harness never pools a cold sample with a warm sample, one concurrency
level with another, or results from different storage modes, shards, or
platforms.

JUnit retention preserves pass/fail diagnostics and links to raw artifacts. It
is not percentile history and must not be treated as the source population for
latency statistics. Raw JSONL is the measurement authority. A p95 requires at
least **40 matching samples**. A p99 is unavailable below **100 matching samples**.
Missing sample counts remain unavailable; the renderer must not
manufacture a percentile from retained JUnit cases or from a smaller,
non-matching population.

## Adversarial fake-host coverage

The fake host is hermetic and uses only disposable state. It must exercise:

- A warming daemon that changes from starting to ready. Capture waits within a
  bounded readiness phase, avoids a duplicate restart, and records that the
  original daemon survived.
- An unresponsive daemon. Capture terminates at the declared timeout phase,
  drains or closes child streams concurrently, and preserves truthful daemon
  survival evidence.
- Dashboard responses with malformed bodies, HTTP 204, and HTTP 404. Each is a
  typed unavailable/error result, never a successful empty dashboard.
- A child that writes enough data to both stdout and stderr to fill ordinary
  pipe buffers, followed by a hang. Both streams are drained concurrently and
  the timeout kills only the disposable process group.
- Repeated identical capture identifiers from one host operation. Each
  occurrence is retained as a separate raw sample; conflicting identifiers are
  rejected. Back-to-back runner captures still receive unique identifiers, and
  output collisions fail before artifacts are changed.

These cases are black-box: they invoke the runner and fake executable through
real subprocess, JSON-RPC, hook, and HTTP boundaries. Tests must remain bounded
and must never connect to a live profile or download semantic data.

Readiness polling, bounded deadlines, concurrent stdout/stderr drain, process
group ownership, and reaping remain in the harness. Product code is not changed
to make a benchmark pass, and harness cleanup must not reap a daemon or process
group it does not own.

## Remote integrated journeys

A remote final-V2 journey becomes eligible only after its committed production
route is mounted in the executable under test and is reachable through the real
CLI, MCP, hook, or host boundary. A catalog entry, schema declaration, mock-only
handler, or passing contract test is not route evidence. The hermetic benchmark
may substitute loopback fixtures for an external service, but it still invokes
the mounted production route.

Contract-only unwired success is a correctness failure. Capture and comparison
must reject it instead of recording an available zero, empty result, or
successful latency sample. Until route mounting is proven, availability is
truthfully unavailable or unsupported and the journey contributes no latency
distribution.

## `n=1` versus distribution policy

Paired comparisons use ABBA ordering and paired log ratios with a seeded
bootstrap confidence interval. Every ABBA position remains present in the raw
samples; aggregates never replace that evidence. A machine-fingerprint mismatch
makes the result descriptive only.

One observation is `n=1`. It may reproduce a regression or preserve a historical
fact, but it must not be presented as a percentile, distribution, baseline, or
gate. Distribution summaries require repeated raw samples with the same stable
crate, journey, workload, fixture, and comparable-machine identities. Only then
may p50, p95, p99, variance, or confidence evidence be described as a
distribution.

## Comparison and gate policy

Correctness regressions are hard failures: malformed artifacts, digest
mismatches, unexpected errors, increased timeout/error rates, missing required
surfaces, false availability, leaked processes, overwritten captures, or a
daemon that should have survived but did not. Latency budgets and proposed SLOs
are advisory. They must not affect the process exit status until the advisory
baseline policy has been met with repeated representative captures, comparable
machine and fixture identity, reviewed variance/confidence evidence, and an
explicit decision to promote a budget.

Relative comparison margins are not accepted from measured output. They live
in the independently versioned and hashed
`benchmarks/runtime/policies/journey-margins-v1.json` artifact, separately for
CLI, MCP, query, dashboard, host, storage, daemon steady-state, and foreground
under maintenance. Policy loading rejects modified journey identities,
eligibility counts, or margins. Paired reports hash both acceptance and journey
policy artifacts into receipts.

Current shutdown observations are historical `n=1` regression samples:

- One trace lasted **89 seconds total**, with abort beginning at **+81
  seconds** from trace start.
- One trace lasted **57 seconds total**, with abort beginning at **+52
  seconds** from trace start.

The abort values are offsets from trace start, not total durations and not time
remaining after abort. These four observations are evidence for targeted
regression checks only; they are not percentiles, baselines, SLOs, or gates.

# Provider-observation pipeline benchmark

> **Historical evidence only.** Preserve the original inputs, measurements,
> and provenance in this directory without upgrading generated inputs into
> native evidence. Current requirements come only from the
> `docs/plans/tracedecay-v2/` hierarchy; exact commands, counts, snapshots,
> receipts, attestations, PR packets, and gate fields below are not rebuild
> instructions. Validate current provider behavior directly.

The directory keeps its PR5 name for artifact provenance. Workload schema 4 is
the current executable multi-provider Linux benchmark contract. The only
checked-in result predates that contract and is retained as descriptive
historical evidence, not current acceptance authority.

The versioned [workload](workload-v1.json) runs production scan/parse,
normalization, sanitizer, authoritative commit, projection/V1 fold, bounded
replay, and exact repeat-ingest no-op paths for claude, codex, cursor, hermes,
kiro, cline, roo-code, and kilo. Each provider baseline machine-asserts parse,
normalize, sanitize, commit, replay, duplicate-noop, projection, backlog,
fairness, and peak-resource checks. The measured loop executes one provider
turn per round in catalog order, records every turn, and validates an
eight-provider maximum turn distance. Hermes uses a bounded native capture.
The other seven provider inputs are generated adapter-behavior records and
confer no native provider or schema acceptance. Unique identities and
secret-shaped canaries are injected into those bounded inputs and the canaries
must be absent from durable observations. The historical result embeds a
versioned `provider-observation-performance-result-v1` containing per-provider
raw parse/commit/replay/pipeline/no-op samples, p50/p95/p99 distributions,
throughput, CPU, process write I/O, database growth, peak RSS, bounded replay
backlog, and observation-count deltas. The pipeline is the real production
parse/normalize/sanitize/commit/project/replay path. The separate parse
distribution measures benchmark-input decoding over the exact bounded input;
it is not native-schema evidence. Commit measures the full production adapter
through authoritative commit and projection, and replay measures the bounded
authoritative-store query.
Scopes are embedded beside each distribution so the commit sample is not
misrepresented as excluding the adapter's parse and normalization work.

The provider catalog remains schema v1. Its measurement block is an additive
optional field for wire compatibility, while executable validation requires
that block for every supported provider and rejects pending paths. This avoids
an unnecessary incompatible schema-v2 claim. A normal test deserializes the
complete manifest with unknown fields denied, executes every production adapter
once, and proves each repeat is a durable no-op.

The retained historical measurement includes the nested
`provider_observation_performance` result and `hook_telemetry_readiness`
diagnostic. [evidence-index.json](evidence-index.json) has no
`current_acceptance`; it indexes
[result-2026-07-26-dc17dd73.json](result-2026-07-26-dc17dd73.json) only as
`historical_stale`.

The measurement embeds hook telemetry readiness as
`hook-telemetry-baseline-readiness-v1`, not as a measured baseline. It reads the
redacted fixtures under `tests/fixtures/host_events` directly, records each
fixture path and SHA-256 identity, and computes compact canonical request-byte
samples. It consumes `crate::hooks::host_hook_telemetry_contract` and
`crate::hooks::measure_host_event_payload_bytes`; it defines no second runtime
telemetry schema. Current production telemetry exposes hook wall time, daemon
RTT, host-event/IPC byte counts, timeout state, and disposition per invocation
on instrumented hosts; Hermes coverage remains partial. Checked-in
distributions and aggregate timeout/disposition counts remain explicitly
unavailable rather than being represented as zero.

The historical result records real SHA-256 identities for its workload members
and inputs plus the host target, Rust/Cargo versions, kernel, and hardware.
Those source/content digests remain useful provenance but do not upgrade
generated records into native captures. Do not recreate the former
clean-checkout archive, tracked-source snapshot, compiler-input attestation, or
evidence-only commit workflow. Run ordinary Cargo against the working checkout
and report the source revision and dirty state truthfully.

Linux `/proc` is the explicit measurement platform contract. Preflight requires
all measured interfaces, a successful write of `5` to
`/proc/self/clear_refs`, and a nonzero `getconf CLK_TCK`, before warmup begins.
CPU, process-write, storage-growth, and peak-RSS provider fields therefore
contain measurements only for successful Linux evidence runs. Unsupported
platforms reject at preflight; the harness never substitutes numeric zero for
an unavailable counter. Zero is reserved for an available counter that
actually measured no work, including the strict repeat-ingest no-op assertion.
The module still compiles on non-Linux targets; an attempted measurement there
rejects the unsupported platform at preflight. CPU identity accepts common x86,
ARM, POWER, and other Linux
`/proc/cpuinfo` labels.

Every replayed authoritative payload is checked for canary removal and a
payload-bound sanitization receipt, and every folded V1 message is checked for
exact identity, role, text, and canary absence. These assertions and V1 point
reads run after each measured phase, so correctness verification is not charged
to latency, CPU, I/O, or storage-growth measurements. The run also requires
zero legacy transcript writes: the observation projector is the only V1
message writer, and compatibility transcript counters report those projector
outputs.
The timed no-op retry replays after the durable end cursor and must return zero
new observations; a full replay verifies unchanged cardinality afterward.

The historical [measurement result](result-2026-07-26-dc17dd73.json) records
source revision `dc17dd731e97f9262e570afd9e7ec1602d8af99e` with 3 warmups and
30 independent measured repetitions of 64 records (30 × 64 = 1,920 records).
The raw artifact records the Linux kernel, CPU, memory, Rust/Cargo toolchains,
every repetition, and the nearest-rank/sample-standard-deviation method.

- Pipeline batch latency: p50 2,341,276,568 ns; p95 2,527,159,267 ns; p99
  2,708,492,865 ns.
- Pipeline throughput: 27.11224430907219 records/s.
- Timed pipeline CPU: 69,110 ms; peak RSS: 153,276 KiB.
- Timed process write I/O: 1,551,220,736 bytes; SQLite database/WAL/SHM growth:
  1,122,556,640 bytes across the 30 independent databases.
- Exact no-op retry plus bounded replay: p50 1,185,536 ns; p95 1,504,741 ns;
  p99 1,558,623 ns; 40 ms CPU total; zero process write bytes, database growth,
  observation-count delta, and coordinator work counters.
- Round-robin fairness: maximum provider turn distance 8.

It is stale because generated provider adapter inputs were mislabeled as native
normalization fixtures and has been superseded by workload schema 4. Its JSON
carries `"evidence_status": "historical_stale"` and is not current product
evidence.

```console
scripts/run-pr5-observation-benchmark.sh
```

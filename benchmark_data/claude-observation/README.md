# Provider-observation pipeline benchmark

> **Historical evidence only.** Preserve the authentic provider fixtures,
> measurements, and provenance in this directory. Current requirements come
> only from the `docs/plans/tracedecay-v2/` hierarchy; exact commands, counts,
> snapshots, receipts, attestations, PR packets, and gate fields below are not
> rebuild instructions. Validate current provider behavior directly.

Archival result artifacts keep the workload id they were sealed under for
provenance. Workload schema 3 is the historical multi-provider Linux
benchmark; the checked-in results are descriptive measurements, not PR
acceptance authority.

The versioned [workload](workload-v1.json) runs production scan/parse,
normalization, sanitizer, authoritative commit, projection/V1 fold, bounded
replay, and exact repeat-ingest no-op paths for claude, codex, cursor, hermes,
kiro, cline, roo-code, and kilo. Each provider baseline machine-asserts parse,
normalize, sanitize, commit, replay, duplicate-noop, projection, backlog,
fairness, and peak-resource checks. The measured loop executes one provider
turn per round in catalog order, records every turn, and validates an
eight-provider maximum turn distance. Each provider input is a bounded copy of
the checked-in native fixtures named in the workload; unique identities and
secret-shaped canaries are injected into those native shapes and the canaries
must be absent from durable observations. The historical result embeds a versioned
`provider-observation-performance-result-v1` containing per-provider raw
parse/commit/replay/pipeline/no-op samples, p50/p95/p99 distributions,
throughput, CPU, process write I/O, database growth, peak RSS, bounded replay backlog, and
observation-count deltas. The pipeline is the real production
parse/normalize/sanitize/commit/project/replay path. The separate parse
distribution measures native provider-format decoding over the exact bounded
input, commit measures the full production adapter through authoritative commit
and projection, and replay measures the bounded authoritative-store query.
Scopes are embedded beside each distribution so the commit sample is not
misrepresented as excluding the adapter's parse and normalization work.

The provider catalog is schema v1. Executable validation requires its
measurement block for every supported provider and rejects pending paths. A
normal test deserializes the complete manifest with unknown fields denied,
executes every production adapter once, and proves each repeat is a durable
no-op.

The measurement includes the nested `provider_observation_performance` result
and `hook_telemetry_readiness` diagnostic. [evidence-index.json](evidence-index.json)
names the single `current_acceptance` artifact, or `null` while no acceptance
run is published.

The measurement embeds hook telemetry readiness as
`hook-telemetry-baseline-readiness-v1`, not as a measured baseline. It reads the
redacted fixtures under `tests/fixtures/host_events` directly, records each
fixture path and SHA-256 identity, and computes compact canonical request-byte
samples. It consumes `crate::hooks::host_hook_telemetry_contract` and
`crate::hooks::measure_host_event_payload_bytes`; it defines no second runtime
telemetry schema. Current production telemetry exposes hook wall time, daemon
RTT, host-event/IPC byte counts, timeout state, and disposition per invocation
on instrumented hosts; Hermes coverage remains partial. Checked-in distributions
and aggregate timeout/disposition counts remain explicitly unavailable rather
than being represented as zero.

The result schema records real SHA-256 identities for workload members and
native fixtures plus the host target, Rust/Cargo versions, kernel, and hardware.
Those source/content digests remain useful provenance. Do not recreate the
former clean-checkout archive, tracked-source snapshot, compiler-input
attestation, or evidence-only commit workflow. The runner uses Bazel `--config=release` in the current clean checkout and
records the source revision before and after measurement, the Bazel and Rust
versions, and the measured executable identity. It validates new output before
publishing the result and index, without archive or backup copies.

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
exact identity, role, text, and canary absence. These assertions and V1 point reads run after
each measured phase, so correctness verification is not charged to latency,
CPU, I/O, or storage-growth measurements. The run also requires zero legacy
transcript writes: the observation projector is the only V1 message writer,
and compatibility transcript counters report those projector outputs.
The timed no-op retry replays after the durable end cursor and must return zero
new observations; a full replay verifies unchanged cardinality afterward.

No acceptance result is published: `current_acceptance` is `null` until
`scripts/run-claude-observation-benchmark.sh` seals one on a Linux evidence
run. A published result records the source revision, 3 warmups and 30
independent measured repetitions of 64 records, the Linux kernel, CPU, memory,
Rust/Cargo toolchains, every repetition, and the nearest-rank/sample-standard-
deviation method.

```console
scripts/run-claude-observation-benchmark.sh
```

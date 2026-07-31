# PR20: End-to-End Performance Optimization

## Status and authority

PR20 follows V2 convergence. It optimizes demonstrated bottlenecks in shipped
product journeys and retains only changes that improve the same journey on the
same host without changing semantics, safety, or recovery.

`00-plan-set-index.md` owns acceptance. Historical benchmark packets,
scorecards, crate-breakup phases, build-lane choreography, and gate labels are
not requirements. Plan 33 does not create a benchmark service, performance
protocol, execution ledger, leaderboard, or parallel observability authority.

## User outcome

Users experience materially faster or less resource-intensive:

- edit-to-diagnostic, impact, CI/review, and proximity feedback;
- dashboard investigation, health, settings, and legal remediation;
- authorized multi-root query and explicit Git operations;
- remote capture, synchronization, query, backup, restore, and failover;
- task/work updates, admitted provider execution, cancellation, and resume;
- public SDK operations; and
- startup, package installation, migration, recovery, and common developer
  feedback loops.

An inconclusive or noisy comparison reports `pending`; it never justifies
shipping a speculative optimization.

## Direct same-host comparison

For each candidate:

1. Reproduce one real supported user journey through its production entry point,
   daemon/application path, durable state or computation, and observable result.
2. Use existing bounded, redacted observability to identify the practical
   bottleneck. Synthetic microbenchmarks may diagnose a mechanism but cannot
   select or accept the product change.
3. Run baseline and candidate on the same host with the same source revision,
   workload, corpus, configuration, feature set, cache preparation, arrival
   model, timeout/retry policy, and correctness oracle.
4. Record raw samples and the observable latency, throughput, CPU, memory, I/O,
   disk, network, write-amplification, and provider-cost evidence relevant to
   that journey.
5. Retain the candidate only when the practical user result improves and direct
   tests preserve behavior under normal operation and the failure/recovery
   conditions the changed mechanism can encounter.
6. Publish a concise `pass`, `fail`, or `pending` result. Missing, censored,
   partial, survivor-biased, or unsupported measurements remain explicit.

Real source and content digests may identify inputs. No clean-checkout snapshot,
committed evidence packet, attestation, or fixed test inventory is required.

## Eligible optimization areas

- **Database and synchronization:** measured statements, transactions, locks,
  WAL/checkpoint work, batching, no-op suppression, fair progress, and bounded
  admission.
- **Projection, indexing, and caches:** changed-evidence recomputation,
  generation reuse under complete identity, bounded publication and eviction,
  and removal of repeated startup work.
- **Retrieval and graph operations:** batching and pruning that preserve exact
  tiers, ordering, cursors, coverage, explanations, fallbacks, and exhaustive
  affected sets.
- **Daemon, LSP, and provider execution:** bounded queues, coalescing,
  cancellation, overlay isolation, exact provider/process identity, progress,
  terminal outcomes, reconnect, and restart.
- **Git operations:** faster status, diff, preview, and explicit apply while
  preserving `HunkRef` freshness, native Git compare-and-swap, atomic runtime
  receipts, and refusal of autonomous history or ref mutation.
- **Build and developer feedback:** portable dependency, feature, target, and
  build-script changes retained only when the same common edit/check journey
  improves on the same host and normal contributor, CI, release, and package
  behavior remains valid.

Package count, source movement, schema conformance, declaration shape, and a
faster synthetic substitute are not product performance evidence.

## Invariants

- One fenced daemon remains the sole mutable authority for each shard.
- Authorization, project/worktree isolation, stable errors, exact identity,
  ordering, cursors, coverage, legal actions, durable effects, paging,
  streaming, cancellation, retry, reconnect, and one terminal outcome do not
  change.
- Cache, process, analyzer, connection, or generation sharing requires complete
  store, scope, authorization, configuration, provider/protocol, model, and
  overlay identity as applicable.
- Batching never weakens atomic cursor, projection, migration, Git, workflow, or
  product-runtime receipt commits.
- Partial, denied, stale, timed-out, failed, cancelled, and unavailable states
  never become successful zero or complete output.
- Production telemetry stays bounded and redacted. It records no credentials,
  prompts, private source, provider payloads, argv/stdin, or secret-bearing
  errors.
- Runtime rollout uses the owning configuration authority and can return to the
  exact prior compatible profile. In-flight effects remain under their owning
  fencing and recovery rules.

## Direct acceptance

- Every retained change begins with an observed bottleneck in a shipped journey
  and a reproducible same-host baseline/candidate comparison.
- The user-visible result improves materially under the same workload and
  correctness oracle; unsupported statistics or incomplete resource coverage
  produce `pending`.
- Direct journey tests preserve semantic equivalence and exercise the relevant
  cancellation, overload, restart, recovery, cache-loss, provider-failure,
  migration, or storage interruption behavior.
- Default-feature product behavior passes ordinary Linux, macOS, and Windows CI.
  Developer performance comparisons may remain Linux-only.
- Semantic divergence, authority or scope violation, hidden fallback,
  duplicate/unknown unsafe effect, secret disclosure, ordering drift, or
  recovery failure rejects or rolls back the candidate.

## Cleanup

Remove rejected candidates, candidate-only flags, temporary profiling hooks,
placeholder baselines, and one-off measurement code that is neither production
instrumentation nor a reproducible local comparison. Keep the ordinary
operational measurements needed to diagnose product health and explain a
truthful failed or pending outcome.

## Not in PR20

- New product semantics or benchmark-only APIs.
- A telemetry database, benchmark daemon, leaderboard, acceptance packet, or
  performance dashboard required for product use.
- Machine-specific target paths, wrappers, lane allocation, or cache policy as
  roadmap mechanisms.
- Optimization chosen by transferred thresholds, point estimates, publication
  pressure, or framework setup.

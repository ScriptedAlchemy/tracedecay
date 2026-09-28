# Hosted warm-worker trial

A maintainer dispatches **TraceDecay CI folding experiment** on `master` with
`pull_requests: 123,124`. Selecting the pair explicitly admits both heads into
one trusted execution domain. Use two distinct, ready, same-repository PRs whose
code and build scripts the operator trusts. The trial adds the non-required
`Warm CI trial / core-contracts` check to each admitted head; existing required
CI remains authoritative.

```sh
gh workflow run ci-folding.yml --ref master -f pull_requests=123,124
```

The planner resolves immutable heads and rejects dependency, toolchain, Cargo
selection, nextest-policy or setup-action differences from the controller. The
controller comes from the dispatched `master` commit. The worker rechecks each
PR's open/ready/current-head state before work, and a fresh reporting job checks
it again before reporting. Changed, closed or draft heads receive a cancelled
check on the old SHA; the trial never advances to a replacement head. A batch
that has skipped a closed PR does not resume when it reopens; dispatch a new
batch. Moving `master` alone does not invalidate this
**head-only** experiment: it does not test GitHub's synthetic merge commit.

One standard `ubuntu-24.04-arm` worker installs the shared dependency graph and
runs the existing `core-contracts` Linux group, preserving all 23 package
selections and features. A `cargo test --no-run --message-format=json` pass with
the exact same selection records compiler freshness and compile time, followed
by the existing `scripts/linux-test-partitions.py run-linux-group` runner and
its nextest/JUnit policy. Every test runs again; no test verdict is reused.

Measurement order is:

1. A with an empty target: this seeds the shared compiler tree.
2. Checkout B once, move A's target aside, and run B with an empty target at the
   **same absolute path**.
3. Delete the cold target, restore A's target, and run B without checking it out
   again. Unchanged source mtimes therefore stay unchanged.
4. Append a failing unit test to `tracedecay-private-fs`, require the named test
   to fail, restore the original source, and require that small crate to pass.

The reported independent total is `A seed + B cold`; the warm total is
`A seed + B warm`. A's measurement is shared by both totals. These timings
exclude runner admission and dependency installation, and do not imply measured
production queue savings. Cargo artifact counts distinguish fresh from compiled
units, with workspace artifacts counted separately from dependencies. Logs,
compile/run timings and JUnit are uploaded; B's cold and warm test identities
and counts must agree. The injected-source controls are separate from PR results.
The job-private Hauler daemon is stopped after each snapshot; only Cargo's
compiler tree is carried as build output across snapshots. Each snapshot has
fresh HOME, temporary and XDG runtime directories, with installed Rust/Cargo
tool homes pinned. Test-profile and nextest output directories are cleared, and
the actual checkout HEAD and tracked source must still match the admitted SHA
after testing. The finite batch consumes one runner and has no persistent service or
self-hosted runner.

The execution job has read-only repository/PR permissions, no supplied secrets,
no persisted checkout credentials, and native `cache-mode: none`; it cannot
publish the warmed target into durable Actions caches. A fresh reporter runs
only the pinned controller and uses planner outputs to choose the exact SHAs
that may receive checks. It never executes artifact content. This separation
protects check-writing credentials; it **does not sanitize hostile PR code or
make its results authentic**. Both snapshots can affect the worker, installed
tools, vendored dependencies, compiler outputs and measurement files. The trial
is appropriate only for explicitly trusted code and is not a fork-PR gate.

Cheap local checks:

```sh
python3 -B evals/ci-folding/warm.py --help
actionlint -ignore 'unexpected key "cache-mode" for "workflow" section' .github/workflows/ci-folding.yml
```

The narrow actionlint exception is for older versions without GitHub's
[current native cache-mode syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#cache-mode).
Hosted acceptance, the two real PR heads, and the source failure/restoration
proof are required before calling this trial verified. The older
`warm-result.json` and `warm-repeat-result.json` are historical framing-only
ancestor probes, not evidence for this cross-PR implementation.

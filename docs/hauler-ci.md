# Hauler CI

The Hauler workflow drains eligible open PRs through eight hosted lanes: six
Linux test lanes, compile checks on ARM, and the shipped CLI/dashboard on x86.
The reviewed `.github/hauler-ci.json` owns commands and compatibility inputs.
Each lane reuses compatible build outputs across admitted snapshots and posts
each PR's result as soon as its work finishes. Each Linux lane runs one test
group, so independent groups can return results in parallel. Reuse happens
across PRs within that group. This trades additional worker starts and duplicate
cold compilation for a shorter serial path; hosted measurements must establish
the effect on complete PR latency and runner minutes.

## Pilot and activation

Automatic admission is off unless repository variable `HAULER_CI_ENABLED` is
`true`. Run **Hauler CI** manually from `master`, selecting PR numbers and a
bounded lane lifetime (at most 300 minutes). The Action validates these inputs.
An explicit manual PR selection bypasses automatic routing receipts for the
pilot; it can duplicate native work. Use a small snapshot limit. Its artifacts
contain controller timing metadata and bounded XML/JSON test reports. Raw worker logs are excluded; report text is PR-produced, not sanitized.

Compare complete PR verdict latency and total runner minutes with ordinary CI.
With the variable unset, ordinary CI remains active. Setting it to `true` routes
ready same-repository PRs from the trusted recipe authors to Hauler only after
its queued lane checks are visible. The Action validates the entire trusted
recipe before ordinary CI skips equivalent heavy jobs. Cheap gates remain;
forks, unlisted authors, invalid policies, and routing timeouts keep native CI.
Push and manual full-CI runs remain native.

A trusted `pull_request_target` job publishes pending checks without executing
PR code or waiting for the worker pool. The read-only routing Action runs before
PR checkout and records its decision in scope-job step metadata. Automated
workers require that matching delegation receipt; a late enqueue cannot take
work already routed to native CI. All three modes share a policy identity tied
to the immutable Action version, normalized recipe, and trusted image inputs.
CI completion wakes the pool; the ten-minute schedule recovers missed wakeups.
Each lane has its own concurrency group, leaving enqueue free to run promptly. Unset the variable
to restore native admission for subsequent runs. Already-created workflows
retain their existing jobs.

## Snapshot and trust contract

Hauler requires successful repository gates, a same-repository PR, and an author
listed in `trustedAuthors`. Forks and authors outside that list keep ordinary
PR CI. Author admission is an explicit trust decision: with `sharedBuilds`, a
PR can modify compiler outputs subsequently reused by another admitted PR.
Compatible inputs do not make hostile cache writes safe.

The controller pins each admitted head, base, and merge commit. A newer PR head,
closure, or draft transition supersedes its work. A later change to `master`
alone does not cancel it. Checks identify the tested snapshot; they do not
certify a newer merge base. Completed checks for the same head and configuration
are not rerun merely because the base moved, and a new head never inherits a
completed test verdict. Update the PR head when fresh base integration evidence
is needed.

Only the trusted host controller receives the short-lived job token, scoped to
reading contents/PRs/Actions metadata and writing checks. Routing receives only
read permissions; it cannot publish checks. Each snapshot runs in a restricted
container with no host token, Docker socket, or host environment mounts; the
container and its descendants are removed before another snapshot starts.
Controller code, recipe, and image setup come from the reviewed default branch,
and the Cargo Hauler Action is pinned to an immutable commit. Checkout does not
persist credentials. Container isolation shares the runner's kernel; it does
not make admitted PRs mutually trustworthy.

Hosted runners and their warm state end with the job. The lane lifetime and
snapshot limit bound spending, not a guaranteed queue latency. Inspect the
reported timings before increasing either limit.

## Measure warm reuse

Keep two compatible PR heads open until both finish in the same lane job.
Compare `durationSeconds` and per-task timings in its `summary.json`; the first
snapshot includes cold preparation, while the next can reuse build outputs.
Report all lanes, failures, and infrastructure errors. A merged or superseded
second PR provides no warm sample. Historical `queueSeconds` includes time
before admission was enabled, so use newly opened PRs to measure rollout queue
latency. Compare total runner minutes as well as each PR's time to results.

## Verify automatic ownership

For a new eligible PR head, check that **Hauler CI / enqueue** creates eight
pending Hauler checks, then inspect the ordinary CI scope job. Its successful
`Hauler route / delegated / <policy>` step is the handoff receipt. Equivalent
native heavy jobs should be skipped while repository gates still run. The
workers update those same pending checks with the pinned snapshot and results.
A successful controller workflow does not mean every PR passed; the individual
Hauler checks contain the test verdicts.

## Hosted reuse trial, 29 September 2026

Two documentation-only PR snapshots used identical Rust inputs. The first
snapshot was cold in each worker; the second reused that worker's build state.
Both trials tested the same pinned head/base/merge commits. The eight-lane
trial also included the Git-subtree timestamp fix, so its improvement cannot
be attributed to parallelism alone.

| Measurement | Four-worker trial | Eight-worker trial |
| --- | --- | --- |
| First PR, full results | 77m 10s after preliminary gates | 49m 17s after dispatch |
| Second PR, full results | 111m 08s after preliminary gates | 55m 38s after dispatch |
| Heavy worker allocation, both PRs | 310m 59s | 272m 38s |
| Heavy worker starts, both PRs | 4 | 8 |

The first trial used automatic admission. The second used manual dispatch and
includes pending-job interference and recovery in its wall time. The timing
origins differ; this is not a controlled percentage improvement in PR latency.
Heavy runner allocation fell 12.3% with comparable job-start/end boundaries,
but excludes enqueue, controller-only and native CI jobs. From 03:24:00 through
04:14:30 UTC, 33 additional Hauler jobs consumed about 23 runner-minutes;
that is an upper bound on avoidable overhead, not proof every job was empty.

Each snapshot executed the same 13,762 unique Linux testcases with zero skips.
The second trial reported 17 test failures per snapshot; all seven check-lane
tasks and both product tasks passed. No infrastructure error was reported.
These results demonstrate warm reuse for unchanged build inputs, not the cost
of recompiling a representative Rust change or a guarantee for a larger queue.

The first trial evidence is in Actions runs
[36508095645](https://github.com/ScriptedAlchemy/tracedecay/actions/runs/36508095645)
and [36508250975](https://github.com/ScriptedAlchemy/tracedecay/actions/runs/36508250975).
The second is in
[36517035115](https://github.com/ScriptedAlchemy/tracedecay/actions/runs/36517035115)
and [36517214575](https://github.com/ScriptedAlchemy/tracedecay/actions/runs/36517214575).
Some redundant pending jobs were cancelled; the eight workers supplying the
reported snapshots completed successfully. Worker success means the controller
reported results, not that the product tests passed.

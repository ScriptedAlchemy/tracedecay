# Hauler CI

The Hauler workflow drains eligible open PRs through four hosted lanes: two
Linux test lanes, compile checks on ARM, and the shipped CLI/dashboard on x86.
The reviewed `.github/hauler-ci.json` owns commands and compatibility inputs.
Each lane reuses compatible build outputs across admitted snapshots and posts
each PR's result as soon as its work finishes.

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

## Verify automatic ownership

For a new eligible PR head, check that **Hauler CI / enqueue** creates four
pending Hauler checks, then inspect the ordinary CI scope job. Its successful
`Hauler route / delegated / <policy>` step is the handoff receipt. Equivalent
native heavy jobs should be skipped while repository gates still run. The
workers update those same pending checks with the pinned snapshot and results.
A successful controller workflow does not mean every PR passed; the individual
Hauler checks contain the test verdicts.

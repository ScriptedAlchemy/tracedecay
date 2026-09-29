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
Use a small snapshot limit for the pilot. Its sanitized artifacts contain timing
and verdict evidence; raw test logs and arbitrary PR-produced files are not
uploaded.

Compare complete PR verdict latency and total runner minutes with ordinary CI.
The pilot does not replace existing CI. Before enabling automatic admission,
route admitted PRs away from duplicate heavy CI jobs while retaining repository
gates and the ordinary path for unsupported PRs. Then set `HAULER_CI_ENABLED` to
`true`. CI completion wakes the pool; the ten-minute schedule recovers missed
wakeups. A single workflow concurrency group bounds the active pool.

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
reading contents/PRs and writing checks. Each snapshot runs in a restricted
container with no host token, Docker socket, or host environment mounts; the
container and its descendants are removed before another snapshot starts.
Controller code, recipe, and image setup come from the reviewed default branch,
and the Cargo Hauler Action is pinned to an immutable commit. Checkout does not
persist credentials. Container isolation shares the runner's kernel; it does
not make admitted PRs mutually trustworthy.

Hosted runners and their warm state end with the job. The lane lifetime and
snapshot limit bound spending, not a guaranteed queue latency. Inspect the
reported timings before increasing either limit.

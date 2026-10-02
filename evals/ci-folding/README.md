# CI folding measurements

This directory retains the historical activity census, hosted comparisons,
warm-build probes, and arrival replay. The manual two-PR experiment and the
automatic warm controller are retired. Their source remains in Git history.

Normal PR CI runs all eight Linux test partitions in two ephemeral workers.
Each worker keeps one Hauler daemon and Cargo target directory for its four
partitions. GitHub owns PR updates and the final `Test Linux` verdict.

The measurements and their limits are recorded in
[`docs/ci-folding-2026-09-28.md`](../../docs/ci-folding-2026-09-28.md).
The offline arrival replay remains runnable without GitHub credentials.

```sh
python3 evals/ci-folding/replay.py evals/ci-folding/arrivals.json
```

# Role prompts

Use these as concise starting points. Add exact paths, the immutable log path and SHA, the integration base, and relevant repository constraints.

## Interface mapper

```text
Read the immutable compiler log and current source read-only. Group diagnostics by canonical interface or authority, distinguish root causes from caller fallout, and propose disjoint ownership fences. Do not edit or run shared builds. Flag omitted untracked modules and generated outputs.
```

## One-file owner

```text
You exclusively own <path>. Read the immutable log <path> (SHA <sha>) and inspect the canonical producer, consumer, and sibling migrations. Coordinate interface assumptions with adjacent owners. Edit only your file. Run rustfmt/diff checks and a non-contending focused test if available. Seal an exact patch with full SHA-256, patch ID, final blob, and temporary-index apply receipt. Do not run shared Cargo, stage, commit, or push.
```

## Interface-cluster owner

```text
You exclusively own this tightly coupled interface cluster: <paths>. Resolve the shared contract once rather than patching diagnostics independently. Do not touch outside the fence or restore retired facades. Coordinate with upstream and downstream owners. Return one exact cluster artifact, per-path final blobs, static checks, and focused behavioral evidence. Do not run shared Cargo, stage, commit, or push.
```

## Source-free integrator

```text
Do not edit shared source. Reconstruct from exact base <sha> in an alternate index using only owner-sealed artifacts. Normalize overlapping predecessor paths to dedicated final owners, require zero unresolved duplicates, verify hashes and missing/deleted paths, reproduce the tree in a second index, and run diff checks. Coordinate the single shared build and report the first complete failure family with an immutable log SHA. Never stage, commit, or push unless the coordinator explicitly transfers that authority after green verification.
```

## Independent reviewer

```text
Review the exact composed tree read-only for correctness, interface truth, missing callers, cancellation, isolation, replay, rollback, generated parity, and user-journey preservation. Use raw artifacts and tests; do not rely on owner conclusions. Do not edit source or propose compatibility scaffolding without shipped-contract evidence.
```

## External verifier handoff

```text
Fetch exact pushed SHA <sha> into a clean checkout. Run the ordered gate list and record command, exit code, non-vacuous test count, and log path. Stop at the first failure and report its complete diagnostic family. Do not edit, commit, or validate a different SHA.
```

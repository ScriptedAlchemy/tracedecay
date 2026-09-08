---
name: interface-cluster-development
description: "Coordinate parallel agents on a shared interface migration when related caller changes require disjoint file ownership."
---

# Interface Cluster Development

Partition work around canonical interfaces and assign one final writer per
path. Use this for a migration that benefits from multiple agents, not an
isolated compiler fix.

## Establish ownership

Inspect active agents, repository instructions, the branch/base, dirty and
staged work, and active builds. Preserve unrelated work. Coordinate with any
existing owner before redirecting overlapping work. Choose an integration base
and one coordinator for shared Cargo or contract-generation runs.

Trace errors to their canonical type, trait, schema, or lifecycle authority.
Group tightly coupled changes; stop downstream edits made unnecessary by an
upstream fix. Include untracked modules, reexports, and generated outputs in
ownership. Do not restore retired facades or duplicate DTOs to quiet callers.

Keep a temporary path-to-owner map, including dependencies. Each writer reads
the same relevant diagnostic evidence, inspects producers and callers, rereads
owned files before editing, and coordinates assumptions across boundaries.
Writers return exact changed paths, changes, and focused verification; they
stop editing after handoff until reactivated. Use
[role-prompts.md](references/role-prompts.md) when dispatching roles.

## Integrate and verify

Integrate in dependency order using the repository's supported worktree or
shared-checkout flow. Review owned diffs; do not broad-stage peer work. Resolve
overlap with the owners rather than silently choosing newer bytes. For detached
patch handoffs, verify the base, patch identity, and paths before applying.
Do not require duplicate indexes, manifests, or hash receipts for ordinary
shared-file work.

Run the smallest check spanning the migrated interface once competing builds
have finished. Group any new diagnostics by shared authority and assign a
coherent follow-up instead of one agent per error. Do not weaken invariants,
fabricate success, or patch generated contracts by hand.

Verify the affected production journey with non-vacuous focused tests. For
Rust wire-schema changes, regenerate contracts, check parity, and test relevant
dashboard/SDK consumers. Cover failure, cancellation, stale identity, isolation,
replay, and rollback where the change affects them. Use independent read-only
review when the migration's scope or risk warrants it.

Checkpoint coherent completed work as repository instructions require, staging
only owned paths. Push only within the user's authorization. Report the exact
revision, changed interfaces, verification, and remaining risks. Remove only
owned temporary worktrees through the repository's cleanup procedure.

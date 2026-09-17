---
name: interface-cluster-development
description: "Coordinate parallel agents on a shared interface migration when related caller changes require disjoint file ownership."
---

# Interface Cluster Development

Partition work around canonical interfaces and assign one final writer per
path. Use this for a migration that benefits from multiple agents, not an
isolated compiler fix.

## Establish ownership

Trace failures to the canonical type, trait, schema, or lifecycle authority,
then group tightly coupled changes and stop work made unnecessary by an upstream
fix. Include untracked modules, reexports, and generated outputs in ownership;
do not restore retired facades or duplicate DTOs to quiet callers.

Choose the integration base and one final writer per path. Coordinate overlap
with active owners and keep shared builds or contract generation under one
coordinator. Use [role-prompts.md](references/role-prompts.md) when dispatching;
it contains the path, evidence, and handoff fields each role needs.

## Integrate and verify

Integrate in dependency order through the repository's supported worktree or
shared-checkout flow. Review owned diffs, resolve overlap with the owners, and
verify the base, patch identity, and paths of detached handoffs. Do not require
duplicate indexes, manifests, or hash receipts for ordinary shared-file work.

Run the smallest check spanning the migrated interface once competing builds
have finished. Group any new diagnostics by shared authority and assign a
coherent follow-up instead of one agent per error. Do not weaken invariants,
fabricate success, or patch generated contracts by hand.

Verify the affected production journey with non-vacuous focused tests. For wire
schema changes, regenerate contracts and test relevant consumers. Cover failure,
cancellation, identity, isolation, replay, or rollback when the interface affects
them. Use independent read-only review when the scope or risk warrants it.

Complete the migration and relevant verification before handoff. Stage only
owned coherent paths, push only within the user's authorization, and remove only
owned temporary worktrees through the repository cleanup procedure. Report the
revision, changed interfaces, verification, and remaining risks.

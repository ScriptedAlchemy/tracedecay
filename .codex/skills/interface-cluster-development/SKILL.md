---
name: interface-cluster-development
description: Coordinate conflict-free parallel subagent development around canonical interfaces. Use for compiler-error migrations, API or DTO cutovers, cross-crate signature changes, generated-contract drift, or large dirty shared worktrees where several agents must edit related code without overlapping ownership.
---

# Interface Cluster Development

Partition work by canonical interface, not by raw error count. Give every path one final writer, keep integration source-free, and let each clean compiler pass define the next bounded queue.

## Establish the floor

1. Read repository instructions and inspect the branch, exact HEAD, upstream, dirty state, staged state, worktrees, and active build processes.
2. Inspect active agents before assigning ownership. Freeze or redirect stale writers that overlap the new partition.
3. Choose one exact integration base. Preserve unrelated dirty work and never broad-stage it.
4. Appoint one build coordinator. In TraceDecay, only that coordinator starts shared Cargo or contract-generation runs.
5. Capture one immutable compiler or test log and its SHA-256. All owners diagnose the same evidence until the next integration pass.

## Build the interface map

Trace each diagnostic to the canonical authority before assigning edits.

- Group errors that share a type, trait, request control, generated schema, lifecycle boundary, or producer-consumer contract.
- Prefer an upstream authority fix when it truthfully resolves multiple callers.
- Do not restore a removed facade merely because several callers still import it. Migrate callers to the canonical boundary.
- Treat omitted untracked modules, reexports, generated outputs, and module mounts as interface members, not isolated compiler accidents.
- Stop downstream writers when an upstream change makes their proposed edits unnecessary.

Use the smallest coherent ownership unit:

- One-file owner for an isolated caller or fixture.
- Small cluster owner for tightly coupled files that must change atomically.
- Generated-output owner for one canonical regeneration pass and its consumers.
- Source-free integrator for composition, duplicate detection, clean builds, and packaging. The integrator does not edit production source.

Maintain an ephemeral owner table during the run:

```text
interface | final owner | exact paths | evidence log | status | dependencies
```

One path must have exactly one final owner. Ordered predecessor patches do not count as final ownership; normalize overlaps into a dedicated final-owner overlay.

## Dispatch owners

Read [role-prompts.md](references/role-prompts.md) when creating or redirecting agents.

Every writer must:

1. Own an explicit path fence and touch nothing outside it.
2. Read the immutable log and inspect the canonical producer, consumer, and sibling migrations.
3. Message adjacent owners when an interface assumption crosses their fence.
4. Re-read the owned files immediately before editing and before sealing.
5. Prefer current canonical APIs over compatibility aliases or local DTO copies.
6. Run file-scoped formatting and diff checks. Run focused tests only when they do not contend with the coordinator-owned build target.
7. Return the final blob identity, exact patch path and SHA-256, patch ID, path list, and verification receipt.
8. Stop writing after handoff unless explicitly reactivated for the same path.

Do not ask every agent to run Cargo. Do not let reviewers mutate source. Do not let the integrator infer missing bytes from a dirty worktree without an owner receipt.

## Integrate without source edits

The source-free integrator must:

1. Create an alternate index or clean temporary checkout from the exact base.
2. Apply reviewed artifacts in dependency order.
3. Exclude overlapping paths from predecessor artifacts and apply their dedicated final-owner overlays last.
4. Verify declared hashes against owner-frozen bytes.
5. Fail on duplicate final owners, missing untracked modules, undeclared deletions, generated drift, or path-manifest mismatch.
6. Reproduce the result in a second index and require byte-identical trees.
7. Run diff checks before invoking the build coordinator.

Temporary manifests and hashes are coordination evidence only. Do not turn them into product acceptance gates or committed repository bureaucracy.

## Run the bounded compiler loop

1. Confirm no equivalent Cargo, rustc, codegen, or dashboard build is active.
2. Run the smallest compile or contract gate that crosses the migrated interface.
3. Report the first complete diagnostic family and the log SHA immediately.
4. Map that family to canonical interfaces, update ownership, and dispatch only the new bounded work.
5. Recompose from the exact base; do not patch the temporary checkout by hand.
6. Repeat until the interface gate is green.

When a pass exposes dozens of errors in one crate, stop per-error dispatch. Assign a coherent interface cluster owner to migrate the crate boundary once.

## Verify the product journey

After compilation is green:

- Run non-vacuous focused tests for the changed authority and its callers.
- Regenerate Rust-derived dashboard contracts with `contracts:generate`; never hand-edit generated contracts.
- Run `contracts:check`, dashboard typecheck, focused Vitest, and the relevant SDK codegen check.
- Test failure, cancellation, stale identity, isolation, replay, and rollback where applicable.
- Preserve the real user journey across CLI, MCP, HTTP, SDK, and dashboard surfaces that remain in scope.
- Request an independent read-only review of the final composed tree.

## Commit and hand off

1. Apply the exact verified composition to the real index, never `git add -A` in a shared dirty worktree.
2. Prove the staged tree, staged path set, and reviewed composition are identical.
3. Commit a coherent conventional slice and push the intended integration branch.
4. Give Ubuntu and Hawk the exact pushed SHA and ordered gates; do not let them validate different commits.
5. Remove only explicitly inventoried temporary worktrees, targets, and patches after the pushed SHA is fetched and the artifacts are no longer needed.

## Escalate instead of guessing

Pause and re-partition when:

- two writers touch the same path;
- a file changes while its owner is sealing it;
- a proposed caller fix actually belongs at a shared authority;
- a generated schema and handwritten decoder disagree;
- a clean composition omits a mounted untracked file;
- focused tests pass but the clean interface gate fails;
- a fix would fabricate success, weaken an invariant, or reintroduce a retired compatibility facade.

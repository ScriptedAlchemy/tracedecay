# TraceDecay V2 Cross-Project and Worktree Scope

## Status / Role

Status: active product plan.

Role: PR15 makes repository, project, checkout, worktree, ref, and global activity scope
consistent across query, CLI, MCP, HTTP, LSP, and UI consumers.

## Outcome

An explicit target always reaches the intended authorized project or code snapshot.
Project facts and sessions remain project-wide across branches and worktrees; only code
graphs select branch/worktree snapshots. Cross-project results load exactly without CWD
choreography or storage knowledge.

## Owns

- Shared scope resolution and federated query semantics.
- Canonical repository, checkout, worktree, ref, snapshot, and project-set relationships.
- Explicit-target, ambiguity, partial-coverage, freshness, and distributed-cursor rules.
- External worktree discovery, safe visibility, cleanup eligibility, and daemon cleanup.

## Does not own

- Project fact/session storage, user-profile storage, code indexing, ranking, transport
  route catalogs, UI components, task graphs, plan executors, or agent schedulers.
- Worktree creation, provisioning, branch deletion, repository mutation, or task-driven
  authority expansion.
- Git index, hunk, and commit evidence or explicit index transactions; those are owned by
  [Plan 36](36-git-aware-change-context-and-index-transactions.md) and consume this plan's
  resolved repository/worktree identity.
- Provider `project_key`, process CWD, host profile, path hash, branch database, or store
  filename as public identity.

## Required behavior

1. One application resolver accepts canonical IDs and bounded locators for repository,
   project, path, checkout, worktree, ref, commit, pull request, session, and saved project
   set. Every surface consumes the same resolved result and typed errors.
2. If the caller names a target, resolution succeeds, returns disambiguation candidates,
   or fails. It never substitutes the active checkout, CWD, first workspace, host home,
   cached project, newest store, or an empty store.
3. `current` is allowed only when no explicit target exists, and every response states the
   resolved project plus code snapshot when code is involved.
4. Project facts, project sessions, messages, and LCM are stored and queried project-wide.
   All branches and worktrees share that authority. Account-wide sessions use the
   user/profile store. No worktree-local fallback store is created.
5. Code queries resolve the requested branch/worktree/ref to an immutable indexed
   snapshot. Dirty, untracked, stale, base-only, missing, or rebuilding coverage is shown;
   the result never implies live working-copy coverage it does not have.
6. Multi-token project lookup matches tokens, aliases, credential-free remotes, paths,
   and verified repository relationships independently. Failure of one combined string
   is not proof that projects are unregistered.
7. Cross-project execution prunes unavailable shards, bounds concurrency and cost, and
   returns searched, stale, unavailable, denied, redacted, and truncated coverage.
   Partial success cannot be rendered as complete success.
8. Stable session, message, entity, and retrieval anchors route to their owner globally
   within the authorized profile. Exact load never requires changing CWD or supplying a
   store-local project key.
9. Project moves and worktree deletion preserve canonical project/session/fact identity.
   Code snapshot and local path aliases retain their time-qualified provenance.
10. TraceDecay discovers externally created worktrees from Git common-directory/admin
    records and observed work locations. It displays repository, path, branch, head,
    dirty state, holders, related sessions/PRs, provenance, confidence, and ambiguity.
11. TraceDecay never creates a worktree. Discovery or association never grants cleanup.
    No product tool, workflow, or automation creates Git branches or worktrees
    or deletes branches, moves refs, or rewrites history; PR15 owns scope resolution and
    safe worktree cleanup only.
12. Cleanup begins with a read-only daemon inspection. Dirty/untracked files, active
    holders, unpushed or unmerged commits, open or uncertain PRs, shared references,
    ambiguous identity, stale evidence, or missing authorization block cleanup.
13. A cleanup request pins the inspected worktree identity and evidence version. The daemon
    re-resolves Git identity and blockers immediately before removing only that worktree
    registration/root. Branch deletion is not part of cleanup.
14. Crash or uncertain cleanup outcome enters reconciliation and remains visible. Missing
    path alone never proves success or authorizes deletion.
15. Related-project suggestions are explicit and bounded. A query, hint, model, task title,
    or agent cannot silently expand one repository into all projects.
16. The [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) gateway
    resolves every LSP workspace folder through this same application resolver.
    Before PR15 it supports only PR12's explicitly resolved single-project
    admission; after PR15, nested roots and multi-root workspaces bind each
    document, analyzer session, code generation, and diagnostic to its exact
    owning folder.
17. An unresolved, ambiguous, denied, stale, or unsupported LSP folder remains
    unavailable with explicit coverage. The gateway never substitutes CWD, the
    first workspace folder, the active checkout, or another folder's analyzer
    or graph generation.
18. [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)'s
    branch-aware feedback cycle and concurrent-agent proximity resolve every
    repository/worktree/branch target through this same application resolver
    before PR15 single-root and after PR15 multi-root scope; neither creates a
    private scope resolver.

## Acceptance

- PR15 tests same-name repositories, moved paths, symlinks, linked worktrees, detached and
  dirty heads, missing indexes, stale/locked/corrupt shards, duplicate legacy routes, and
  unauthorized neighbors.
- A frozen Rspack/Rsbuild/React Router-style fixture resolves token-wise, queries multiple
  repositories, preserves source class, and exact-loads every returned session/entity.
- Project facts and sessions are identical from two worktrees while their code queries
  select different declared snapshots.
- CLI, MCP, HTTP, LSP, and UI conformance returns the same resolution, ambiguity candidates,
  coverage, cursor binding, and errors.
- LSP fixtures cover same-name repositories, nested roots, linked worktrees,
  symlink escapes, ambiguous folders, denied neighbors, and partial multi-root
  coverage without cross-folder document or diagnostic state.
- Worktree discovery is idempotent; safe cleanup blocks every unsafe case, revalidates at
  mutation time, preserves branches, and reconciles crash outcomes.
- No public or internal PR15 operation creates a worktree or opens a worktree-local fact,
  session, or LCM database.

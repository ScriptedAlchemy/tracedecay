# Arena task: whole-shape design sketch for TraceDecay storage

You are one candidate runner in an architect-skill arena. Before anything else, read in full:

1. The architect skill: /home/zack/.claude/plugins/cache/pstack-claude/pstack/0.9.35/skills/architect/SKILL.md
2. The runner prompt (your discipline): /home/zack/.claude/plugins/cache/pstack-claude/pstack/0.9.35/skills/architect/references/runner-prompt.md
3. The rationale template (your output shape): /home/zack/.claude/plugins/cache/pstack-claude/pstack/0.9.35/skills/architect/references/rationale-template.md
4. The red flags you will be screened against: /home/zack/.claude/plugins/cache/pstack-claude/pstack/0.9.35/skills/architect/references/design-red-flags.md
5. The grounding: every file in /tmp/claude-1000/-fast-projects-tracedecay/c9698727-3461-47d1-82ba-d60f5d96485a/scratchpad/storage-arch/ground/ (start with explanation.md, then the explorer files as needed) and /tmp/claude-1000/-fast-projects-tracedecay/c9698727-3461-47d1-82ba-d60f5d96485a/scratchpad/storage-arch/why/synthesis.md, plus /fast/tmp/arch-native-exec/*.md.

Repository (read-only for you): /fast/projects/tracedecay/.claude/worktrees/agent-a9a062786c04623b1 at origin/master e5a8ede6ea. Verify claims against source when a design decision depends on them. NEVER open crates/tracedecay/src/daemon/project_server_lifecycle.rs. Do not run builds. Do not touch ~/.tracedecay or the live daemon.

## The task

Redesign TraceDecay's databases and storage layer from first principles, as a whole. Produce one candidate design package: a usage sketch (caller's view first), a type sketch in Rust (traits, structs, enums, signatures with `todo!("not implemented")` bodies and `// TODO` pseudocode for tricky logic), a module map (which crates exist after the redesign, which are deleted or merged), and the one-page rationale per the template.

Your design must answer each of these explicitly, each in its own short subsection of Shape:

1. Engine. SQLite (rusqlite) is the recorded 2026-10-12 decision (Turso/libSQL abandoned; one engine). Grafeo currently backs the code graph. If you keep, drop, or replace either, argue it against the evidence.
2. Unit of storage and sharing across worktrees of one repository. What is one file? What is one database? How do 30+ agent worktrees of one repository map onto storage? Where do profile-wide data (registry, observations, transcripts, LCM payloads) live?
2a. Partitioning (required; Zack asked "So maybe we need many databases not just 1 sessions db?" after learning one sessions.db carries the observation log, LCM rows, Work/Workflow, git-transaction and native-integration journals, and the projection queue). State how your design splits storage into files/databases along four axes and justify each split or merge: failure domain (corruption containment), write cadence (hot append-only evidence vs small transactional journals vs rebuildable derived read models vs code graph and memory), retention policy per class, and sharing scope (profile vs repository vs worktree). Give a table: data class, file, cadence, retention, scope, rebuildable or authoritative. Constraint: more files must not mean more threads. Today each open store spawns 1 writer plus 3 reader threads (30 projects is about 244 threads at rest), so any split must sit on one shared, bounded executor for the whole daemon. The deleted ADR-003 (2026-07-12) planned separate catalog.db/activity.db/project.db and rejected "one giant database" because it "couples privacy, backup, and failure domains"; the V2 plans later folded sessions into the project-wide store with no recorded reason. A candidate that keeps one file per project must argue against that ADR with evidence.
3. Durability and WAL/checkpoint ownership. Who checkpoints, with what mode, and how is the PASSIVE-under-overlapping-readers ratchet (PR #4097: 229 MB WAL, 245k retry wakes) impossible by construction?
4. Execution class. How do reads and writes run so that no caller ever parks a native async worker (today 2,683 native reader waits per mcp_suite run through StoreRuntimeClientLease::dispatch_read, issue #4085; sync Work/Workflow ports park writers)? Name the thread/actor model and the async/sync boundary.
5. Corruption containment. One bad row or file must not freeze unrelated work (today a "database disk image is malformed" error froze all transcript ingest for 6 days because the drain is fail-closed; the drain lane found the cause was a corrupt in-heap SQLite TEMP cache (#4094), a process-lifetime fault, not on-disk corruption). State the blast radius of a corrupt row, a corrupt file, a process-lifetime connection/heap fault like #4094, and a corrupt graph generation, and how each is detected and recovered without operator action.
6. Retention and bounded growth as first-class invariants with observable sizes (today the profile store reached 153 GB with growth unmeasured, #4099). Which component owns each byte budget, how is size observed, and what happens at the budget? Answer specifically each growth source in /tmp/claude-1000/-fast-projects-tracedecay/c9698727-3461-47d1-82ba-d60f5d96485a/scratchpad/storage-arch/growth-sources-4099.md, at least the top three: per-worktree project stores that never collect (orphan classifier keeps a deleted linked worktree Live while the primary .git exists; unregistered proj_* dirs have no collector; any memory_* row blocks collection), the SQLite hard-pressure drain that retries PASSIVE every 100 ms forever with no reader revocation or escalation, and Grafeo WALs with no periodic checkpoint; then LCM payloads, observation projection, generation staging, hook_analytics.jsonl, response-handles.
7. Schema evolution. How does a schema change ship, and what is the upgrade path for a beta install (the accepted reset is `tracedecay wipe --stale` plus `tracedecay init`; say whether your design needs more).
8. Public port surface. List every public trait/type the rest of the workspace sees. Depth: hide the engine; no transport or wire types on the surface; count the methods and compare with today's census.
9. Lifecycle: init, wipe, projects forget, doctor, maintenance. What each does in your design in one line apiece. Also answer: on 2026-10-16 the operator's profile was deleted while the daemon ran, and the daemon kept writing to about 112 GB of unlinked files (sessions.db 63 GB, user-sessions.db 46 GB, global.db 2.6 GB) with nothing noticing (explorer 1, Headline). Today local `wipe` deletes `projects/<id>/` but leaves the `code_projects` registry row, and the next open recreates the store.
10. Cutover plan. A sequence of independently verifiable PRs against master, riskiest unknown first, that converges on the target with no long-lived compatibility layer (outcome-oriented execution: migrate callers then delete legacy APIs in the same wave). For each PR: what it proves and how (Bazel targets: clippy, rustfmt, unit tests of touched crates, //crates/tracedecay:daemon_suite, :mcp_suite, :session_suite, :unit_test).

Also run a short Alternatives section that names the other whole shapes you rejected and why, judged on interface depth.

## Fixed inputs (decided; do not reopen)

1. Folding sessions into the project-wide store (2026-07-15) has no recorded reason; treat it as drift. ADR-003's separate-failure-domain argument stands unless you refute it with evidence.
2. Storage classes. Durable memory (fact store, holographic memory, user preferences, project decisions) must survive indefinitely with full durability (synchronous=FULL or equivalent for that class) and is never subject to size-based eviction. No backup copies are ever made. Everything else is either rebuildable (code graph, derived read models, summaries) or evidence with retention (observations, raw messages, payloads).
3. Project identity is the repository, not the git directory. The operator host has about 14 full clones of one repository plus 32 linked worktrees; a full clone has its own git dir and today gets its own store. Say how clones of one repository share a store (remote URL, root commit, explicit identity file, or other) while code indexes stay scoped per worktree.
4. The projection drain's "never skip ahead" rule was designed with a dead-letter path that was never built. Your design must include that path.
5. Durability (synchronous mode) is decided per storage class, not globally (today the writer runs NORMAL where the July design said FULL).

## Your assigned direction

{DIRECTION}

Argue this direction at full strength; don't hedge toward a safe middle. If the evidence makes your direction untenable on one axis, say so plainly and adapt only that axis, naming the adaptation. Differences between candidates are the signal the orchestrator uses.

## Output

Write the package to {OUTDIR}/design.md (rationale plus usage plus module map) and {OUTDIR}/sketch.rs (the type sketch; one file, may be long). Keep design.md to roughly 400 lines and sketch.rs to what the shape needs. Your final reply: at most 15 lines naming your shape, its three load-bearing decisions, and its biggest risk.

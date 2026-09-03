---
name: tracedecay-dev-sweep
description: "TraceDecay Dev: Use for a full introspection-and-improvement pass over TraceDecay itself — triggers every dev skill in dependency order (diagnostics gate, evidence sweep across usage/logs/automation/hints/code-health, synthesis, fixes, verification). Use when asked to 'improve tracedecay from its own data', run a mass introspection, or when several dev skills would otherwise be invoked ad hoc."
---

# TraceDecay Dev: Full Introspection & Improvement Sweep

One entry point that sequences all repo-local dev skills plus the relevant
bundled/profile skills. Each phase INVOKES the named skill — do not restate or
improvise its internals here; this skill only owns ordering, parallelism, and
the hand-offs between phases. Repo-local dev instructions
(`.claude/skills` / `.codex/skills`), not plugin-distributed.

Announce: "Running tracedecay-dev-sweep: <phases planned>."

## Why this order

Evidence before fixes; cheap gates before expensive mining; independent
evidence lanes in parallel; one synthesis chooses the smallest durable fixes;
implementation last, verified, and recorded. A broken build invalidates every
downstream signal, so it gates first.

## Phase 0 — Build gate (blocking, cheap)

Invoke `interpreting-tracedecay-diagnostics`. If diagnostics show build/type
errors, fix those first (its workflow) and re-gate. Do not mine usage data on
a broken tree.

## Phase 1 — Evidence sweep (independent lanes; parallelize via subagents or a workflow)

Run all five lanes; each returns evidence with counts and cited
sessions/run-ids, never conclusions without citations:

1. **Adoption & telemetry** — invoke `introspecting-tracedecay-usage`
   (its five audit lanes + helper script). Headline per-tool counts, error
   rates, fact-store adoption.
2. **Transcript mining** — invoke `self-improving-from-usage-logs`. Real
   failure modes, misfires, and workarounds from session logs and agent
   transcripts.
3. **Automation loops** — invoke `inspecting-automation-cycles`. Skipped
   runs, curator/reflector/skill-writer output quality, automatic validation
   and activation, terminal records, and advertised run artifacts.
4. **Hook hints** — invoke `evaluating-hook-hints` (quality, verbosity,
   dedupe, token efficiency, real transcript replays). For profile-managed
   depth, view `agent-hook-hint-quality-review` and
   `agent-hook-latency-profiling` via `tracedecay_skill_view`.
5. **Code health** — invoke `tracedecay:check-health` (and
   `tracedecay:audit-safety` when ship-risk matters). Worst offenders become
   candidate fix targets, and `tracedecay_simplify_scan` output is triaged by
   the lead, not applied blind.

Cap each lane's scope before launching (project set, time window) so lanes
finish comparably; report dead ends honestly — an empty lane is a finding.

## Phase 2 — Synthesis (lead judgment, single context)

Merge lane evidence into failure classes (discovery, fallback, diagnostics
interpretation, automation validation, memory curation, code structure — per
`introspecting-tracedecay-usage`'s interpretation rules). For each class pick
the SMALLEST durable fix: bundled-skill text < eval/test < code change <
managed-skill action. Rank by evidence strength × blast radius. Drop anything
supported by a single uncited anecdote.

## Phase 3 — Implement (bounded, isolated)

- Code/eval/doc fixes: follow `isolated-worktree-task-flow` (view via
  `tracedecay_skill_view`) — fresh worktree per independent fix set, exact
  scoped edits for subagents, lead reviews diffs.
- New or updated automation/profile skills: invoke
  `writing-agent-managed-skills` for automatic writer validation and activation,
  followed by read-only run and adoption review.
- Repo-local dev-skill updates: edit `.claude/skills/` and `.codex/skills/`
  together — the copies must not drift (host-specific `agents/` overlays are
  the only allowed asymmetry).

## Phase 4 — Verify & record

- Narrowest verification per fix, then the affected suites once
  (`tracedecay:test-changes` / `tracedecay_run_affected_tests`).
- Record durable learnings with `tracedecay_fact_store` (calibrated trust,
  cited source); update MEMORY-style indexes only through the skills above.
- Deliverable: per-lane headline counts, the failure classes, each applied
  fix with its verification result, anything deferred with a reason, and
  `tracedecay_metrics:` savings where tools reported them.

## Stop conditions

Stop and report instead of pushing on when: the Phase 0 gate cannot be
cleared, a lane's tooling is unavailable even via the `tracedecay` CLI
fallback, or a proposed fix requires deleting user memory/facts without an
exact user instruction naming the destructive intent and target.

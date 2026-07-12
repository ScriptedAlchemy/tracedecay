---
name: executing-tracedecay-v2-plan
description: Parse and execute the linked TraceDecay V2 redesign plans without skipping prerequisites or duplicating completed work. Use when selecting the next V2 PR/task, turning the redesign into Kanban/worktree assignments, auditing completion, resuming after compaction, or reconciling plan dependencies with live Git, review, test, and task-graph evidence.
---

# Execute the TraceDecay V2 plan

Treat plan text as the intended dependency specification and the current activated canonical task graph as dispatch authority. Treat immutable Git/review/test receipts as completion authority. A mismatch between plan and graph blocks dispatch until an explicit versioned reconciliation; neither side is silently rewritten. Never infer completion from a checked box, task status, branch name, or worker prose alone.

## Build the inventory once

Run from the repository root:

```bash
python3 .codex/skills/executing-tracedecay-v2-plan/scripts/plan_inventory.py
```

Use `--json` for machine processing and `--id 'PR 4E'` for one slice. The script is read-only. It locates PR/task headings, source lines, ordering statements, referenced PR IDs, acceptance-checkbox counts, and declared commit subjects. It does not decide that prose references are gating edges.

Read these authorities in order:

1. `docs/plans/tracedecay-v2/00-plan-set-index.md` for plan ownership and cross-plan order.
2. `docs/plans/2026-07-09-tracedecay-brain-rewrite.md` for the integrated phase/PR sequence.
3. The numbered owner plan for full acceptance and files.
4. Any companion plan named by `Ordering`, `after`, `depends`, `blocked by`, or the index.

Do not make every worker reread the full plan set. The orchestrator parses once and gives each worker exact source sections, complete acceptance, constraints, and retrieval anchors.

## Determine completion

For each candidate slice, collect all of:

- exact implementation commit reachable from the intended branch;
- clean, correct worktree/branch binding;
- required independent review verdict over that exact candidate;
- named tests/checks and their receipts;
- remediation and successor-review state for every negative verdict;
- integration commit when downstream work requires integrated output;
- current master/open-PR changes that supersede plan assumptions.

Classify `not_started | active | changes_requested | implemented_unreviewed | approved_unintegrated | integrated | superseded | blocked_unknown`. A task marked `done` with `CHANGES_REQUESTED` is terminal review evidence, not approved implementation.

## Select the next work

1. Exclude `integrated` and `superseded` slices.
2. Exclude any slice with a prerequisite outside `integrated` or an explicitly accepted same-stack parent state.
3. Exclude ambiguous scope, dirty/shared writer worktrees, stale candidate SHAs, and missing review evidence.
4. Prefer the smallest reviewable slice on the critical path.
5. Create implementation, independent review, remediation, successor review, and integration gates as distinct work items.
6. Attach parents at creation time. Publish multi-edge graph grooming atomically when V2 supports it; on Hermes, block dispatch first, add replacement parents before removing old parents, and recheck for stale claims after every mutation.
7. Use stable idempotency keys derived from plan ID + slice + role + candidate generation.

Never call a slice eligible because its parent title/status looks complete. Resolve canonical IDs and inspect results.

## Worker packet

Include:

- plan file + exact section/line and PR ID;
- objective, bounded files, required skills, workspace/branch, effect ceiling;
- every prerequisite ID and accepted input commit;
- full acceptance checklist and exact commands;
- requested lifecycle owner and acting runtime/model;
- prohibition on self-approval, merge, push, or unrelated edits as applicable;
- required handoff: candidate SHA, diff scope, tests, risks, retrieval anchors.

Use native Claude Code/Codex CLI acting lanes as separate attempt participants when requested; do not disguise them as Hermes provider profiles.

## Review and advance

After every coherent checkpoint, independently inspect the actual diff, branch, plan authority, and board graph before promoting downstream work. A negative review completes as evidence and creates one idempotent remediation + successor review. Integration depends on the latest approved successor review, not merely on remediation completion.

Before handoff, report:

```text
Selected: <canonical plan slice>
Why eligible: <all prerequisite receipts>
Blocked alternatives: <exact missing evidence>
Worker packet: <task IDs/worktree/acceptance>
Next gate: <independent review or integration>
```

Stop on ambiguous authority rather than inventing an edge or completion state.

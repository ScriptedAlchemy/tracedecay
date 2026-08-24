---
name: managing-work
description: 'Use when creating, proposing, admitting, running, placing, or inspecting TraceDecay Work tasks, including attempts, graph mutation, and adjudication. Do not use for Workflow definitions or runs.'
---

# Managing Work

Work is the daemon-owned task/attempt surface. Every operation below is
projected from the same executable registry as HTTP and CLI. Use exact
identities the previous step returned; do not invent proposal, attempt,
placement, or run ids from chat text.

Announce: "Using tracedecay:managing-work to <propose/run/inspect>."

## Propose and admit

1. Draft → `tracedecay_work_generate_proposal`. Compare alternatives with
   `tracedecay_work_compare_proposal` before choosing.
2. Materialize → `tracedecay_work_create`, then
   `tracedecay_work_review_proposal` / `tracedecay_work_accept_proposal`.
3. Admit execution → `tracedecay_work_admit_execution` only after accept.

## Attempts

1. Start → `tracedecay_work_start_attempt`. Synthesis admission is
   `tracedecay_work_synthesize`.
2. Inspect → `tracedecay_work_attempt_status`, `tracedecay_work_list_attempts`,
   `tracedecay_work_execution_history`.
3. Recover → `tracedecay_work_cancel_attempt`, `tracedecay_work_resume_attempts`,
   `tracedecay_work_retry_attempt`.

## Evidence and views

Read-only after an attempt exists: `tracedecay_work_hydrate_artifacts`,
`tracedecay_work_retrieve_evidence`, `tracedecay_work_views`,
`tracedecay_work_experience`.

## Graph mutation and topology

Preview with `tracedecay_work_prepare_graph_mutation`, then apply
`tracedecay_work_mutate_graph`. Shape and metrics are
`tracedecay_work_topology` and `tracedecay_work_topology_metrics`.

## Adjudication

Preview duplicates with `tracedecay_work_prepare_duplicate_adjudication`, then
`tracedecay_work_adjudicate_duplicate` or `tracedecay_work_adjudicate_leak`.

## Run and placement control

- Run: `tracedecay_work_pause_run`, `tracedecay_work_resume_run`,
  `tracedecay_work_run_control`.
- Placement: `tracedecay_work_placement_preflight` →
  `tracedecay_work_admit_placement` → `tracedecay_work_placement_status` →
  `tracedecay_work_release_placement`.

## Guardrails

- Definition/run lifecycle for named Workflows →
  `tracedecay:managing-workflows`.
- Linked-worktree cleanup after placement → `tracedecay:reviewing-changes`.
- Multi-root Work queries use a saved scope set, then
  `tracedecay_multi_root_execute` — see `tracedecay:using-tracedecay`.

## If tools are deferred or MCP fails

- Deferred: one ToolSearch call —
  `select:tracedecay_work_generate_proposal,tracedecay_work_create,tracedecay_work_start_attempt,tracedecay_work_attempt_status,tracedecay_work_views`.
- MCP error: `tracedecay tool <name>` (see `tracedecay:using-the-cli`).

## Deliverable

Do not end without the Work identity used (proposal/attempt/placement/run),
the last operation and its typed outcome, and any follow-up identity the
next step must consume. Report any `tracedecay_metrics:` line.

---
name: managing-workflows
description: 'Use when registering, validating, activating, or retiring Workflow definitions, issuing or redeeming handoffs, or starting, pausing, resuming, or canceling a Workflow run.'
---

# Managing Workflows

Workflow is the daemon-owned definition and run surface. Every operation
below is projected from the same executable registry as HTTP and CLI. Use
exact definition and run identities the previous step returned; do not
reconstruct them from a label or path.

Announce: "Using tracedecay:managing-workflows to <define/handoff/run>."

This is not `tracedecay_workflows` (read-only session recovery of `wf_*`
runs). That read stays in `tracedecay:managing-session-context`.

## Definitions

1. Validate before write → `tracedecay_workflow_validate_definition`.
2. Register → `tracedecay_workflow_register_definition`, then
   `tracedecay_workflow_activate_definition`.
3. Leave the roster → `tracedecay_workflow_retire_definition` or
   `tracedecay_workflow_reject_definition`.
4. Inspect → `tracedecay_workflow_get_definition`,
   `tracedecay_workflow_list_definitions`,
   `tracedecay_workflow_definition_history`,
   `tracedecay_workflow_diff_definition`.

## Handoffs

Issue a grant with `tracedecay_workflow_handoff_issue`. Redeem only that
grant with `tracedecay_workflow_handoff_redeem`. Do not invent grant
identities.

## Runs

Start with `tracedecay_workflow_start_run`. Control an existing run with
`tracedecay_workflow_pause_run`, `tracedecay_workflow_resume_run`, or
`tracedecay_workflow_cancel_run`. Inspect with `tracedecay_workflow_get_run`.

## Guardrails

- Work task/attempt/placement operations → `tracedecay:managing-work`.
- Raw transcript recovery of a `wf_*` run →
  `tracedecay:managing-session-context`.
- Mutations require the exact definition or run identity from a prior read
  or register; a display name is not enough.

## If tools are deferred or MCP fails

- Deferred: one ToolSearch call —
  `select:tracedecay_workflow_list_definitions,tracedecay_workflow_validate_definition,tracedecay_workflow_register_definition,tracedecay_workflow_start_run,tracedecay_workflow_get_run`.
- MCP error: `tracedecay tool <name>` (see `tracedecay:using-the-cli`).

## Deliverable

Do not end without the definition or run identity, the last operation and
its typed outcome, and whether a handoff grant remains unredeemed. Report
any `tracedecay_metrics:` line.

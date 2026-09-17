---
name: managing-workflows
description: 'Register, validate, activate, or retire TraceDecay Workflow definitions; issue handoffs or control an existing Workflow run.'
---

# Managing Workflows

Validate a definition before registering it
(`tracedecay_workflow_register_definition`) and activating it
(`tracedecay_workflow_activate_definition`); rejection and
retirement (`tracedecay_workflow_reject_definition`,
`tracedecay_workflow_retire_definition`) are typed lifecycle transitions, not
deletions. Mutations consume the exact definition or run identity returned by a
read or registration; a name, label, or filesystem path is not authority.
Handoff redemption (`tracedecay_workflow_handoff_redeem`) consumes the grant
`tracedecay_workflow_handoff_issue` returned, never an invented grant identity.

Starting a run (`tracedecay_workflow_start_run`) differs from controlling an
existing run (`tracedecay_workflow_pause_run`, `tracedecay_workflow_resume_run`,
`tracedecay_workflow_cancel_run`). Preserve typed state when pausing, resuming,
or canceling, and inspect terminal effects before retrying an interrupted
request. Use live operation schemas for available arguments.

`tracedecay_workflows` retrieves historical `wf_*` session runs; it does not
control Workflow definitions or execution. Use `managing-session-context` for
that history and `managing-work` for task/attempt/placement operations.

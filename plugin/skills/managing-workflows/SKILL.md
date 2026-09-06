---
name: managing-workflows
description: 'Register, validate, activate, or retire TraceDecay Workflow definitions; issue handoffs or control an existing Workflow run.'
---

# Managing Workflows

Validate a definition before registering and activating it. Mutations consume
the exact definition or run identity returned by a read or registration; a name,
label, or filesystem path is not authority. Handoff redemption consumes the
issued grant, never an invented grant identity.

Starting a run differs from controlling an existing run. Preserve typed state
when pausing, resuming, or canceling, and inspect terminal effects before retrying
an interrupted request. Use live operation schemas for available arguments.

`tracedecay_workflows` retrieves historical `wf_*` session runs; it does not
control Workflow definitions or execution. Use `managing-session-context` for
that history and `managing-work` for task/attempt/placement operations.

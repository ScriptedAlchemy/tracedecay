---
name: managing-workflows
description: Use when the user asks to register, validate, activate, or retire a TraceDecay Workflow, issue a handoff, or control a Workflow run. Load this skill before the first tool call.
metadata:
  opencode/slash: "true"
---

# Managing Workflows

Follow these steps in order. Report a step you skip as `skip: <reason>`. Every
mutation consumes the exact definition, run, or grant identity a read or a prior
mutation returned; a name, label, or filesystem path is not authority. Use live
operation schemas for arguments.

1. Validate the definition (`tracedecay_workflow_validate_definition`). skip:
   the request controls an existing run; go to step 4.
2. Register it (`tracedecay_workflow_register_definition`) and keep the
   definition identity it returns.
3. Activate it (`tracedecay_workflow_activate_definition`). Rejection and
   retirement (`tracedecay_workflow_reject_definition`,
   `tracedecay_workflow_retire_definition`) are typed lifecycle transitions,
   not deletions.
4. Start a run from an Active version (`tracedecay_workflow_start_run`), or
   control an existing run (`tracedecay_workflow_pause_run`,
   `tracedecay_workflow_resume_run`, `tracedecay_workflow_cancel_run`). Inspect
   terminal effects before retrying an interrupted request.
5. Issue a handoff (`tracedecay_workflow_handoff_issue`) only for a step of an
   Active version and a run started from it, and redeem
   (`tracedecay_workflow_handoff_redeem`) only the grant it returned. skip: no
   handoff was requested.
6. Act on a refusal by its type. `stale` carries the requested and current
   `expected_revision` or `expected_sequence` in `detail`: refresh and resend.
   `conflict` is an illegal lifecycle edge or a run from an inactive version.
   `reauthorize` needs a new handoff from its issuer.

`tracedecay_workflows` reads historical `wf_*` session runs and controls
nothing; that history belongs to `managing-session-context`, and task, attempt,
and placement operations belong to `managing-work`.

**Report:** the definition or run identity, the lifecycle state it reached, and
any typed refusal with its detail. Do not resend a `stale` or `conflict` request
unchanged.

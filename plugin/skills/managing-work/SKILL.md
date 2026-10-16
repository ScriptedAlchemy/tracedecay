---
name: managing-work
description: Use when the user asks to create or control TraceDecay Work tasks, attempts, graph mutations, placements, or adjudication. Load this skill before the first tool call.
metadata:
  opencode/slash: "true"
---

# Managing Work

Follow these steps in order. Report a step you skip as `skip: <reason>`.
[Operations](references/operations.md) covers each manual Work operation.

1. For bounded parallel implementation with a reviewed synthesis, call
   `tracedecay_work_advance_job` with one request that names the tasks, each
   task's placement, and the synthesis. skip: the user asked to control one
   existing task, attempt, placement, or run; go to step 4.
2. Call it again with the same request until it settles: receipts, a wait, a
   typed refusal, or the completed attempts.
3. On a typed refusal or a failed attempt, read its state before you retry,
   resume, or cancel anything.
4. For a step the job leaves to you (pause, resume, retry, adjudication), call
   the operation with the exact identity the preceding operation returned.
5. Read the final state of every task, attempt, and run you touched.

Named Workflow lifecycle belongs to `managing-workflows`; session recovery is
read-only history, not Work execution.

**Report:** each task, attempt, placement, or run identity with its state and
the receipt or typed refusal that set it. Do not retry a refused step without
reading its state.

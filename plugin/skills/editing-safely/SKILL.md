---
name: editing-safely
description: Use when the user asks to rename, move, extract, replace, or insert code across symbols or files. Skip when the edit stays inside one function body and keeps its signature. Load this skill before the first tool call.
metadata:
  opencode/slash: "true"
---

# Editing safely

Follow these steps in order. Report a step you skip as `skip: <reason>`.
[Operations](references/operations.md) lists each mutation's preview, apply,
rollback, and reconcile rules.

1. Resolve the exact symbol. Refuse an ambiguous name rather than choosing a
   same-named declaration.
2. Inspect what the edit touches: callers for a signature change, constructor
   and write sites for a field change. Graph results can miss public consumers,
   macros, generated code, and string-keyed dispatch; inspect those boundaries
   when relevant.
3. Preview the mutation (`tracedecay_rename_preview` for a rename). A preview is
   evidence, not an applied edit. skip: the operation has no preview and its
   anchor is unique.
4. Apply with the identity and expected state the preview or read returned.
   When the expected state no longer matches, stop and preserve the peer's
   change.
5. If an operation was interrupted, reconcile its committed effects before
   retrying. skip: every operation completed.
6. Verify the changed behavior with the native build or tests; use
   `assessing-impact` to select them.

**Report:** what changed, the operation identity of each applied mutation, the
references inspected, and the verification that ran. Do not report a preview as
an applied edit.

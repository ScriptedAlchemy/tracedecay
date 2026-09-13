---
description: Find and safely remove dead code, unused imports, and duplication with TraceDecay and compiler diagnostics.
argument-hint: "[path]"
---

# Clean dead code

Clean the whole repository, or `$ARGUMENTS` when it names a directory. Use dead
code, redundancy, unmounted-file, and compiler or language-server evidence to
find candidates. Confirm callers, references, runtime loaders, generated paths,
and external consumers as applicable before removing code; public symbols need
evidence beyond an empty indexed caller set.

Apply the smallest anchored edits, then run the native build or typecheck and
the relevant behavioral tests. Retained diagnostics do not recompile edits.
Report what was removed or consolidated and the verification that exercised it.

---
name: tracedecay-clean-dead-code
description: Find and safely remove dead code, unused imports, and duplication with TraceDecay and compiler diagnostics.
---

# /tracedecay-clean-dead-code

Use `tracedecay:reviewing-changes` to identify candidates, then `tracedecay:editing-safely` for removals.

Clean the whole repository, or `$ARGUMENTS` when it names a directory. Confirm
callers, references, runtime loaders, generated paths, and external consumers as
applicable before removal; public symbols need more than an empty indexed caller
set. Respect Cursor approval and run mode for edits and verification, then report
what changed and the checks that exercised it.

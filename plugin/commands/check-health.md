---
description: Assess code health and prioritize concrete structural problems in a repository or directory.
argument-hint: "[path]"
---

# Check health

Assess the whole repository, or `$ARGUMENTS` when it names a directory. Follow
the bundled `code-health` skill. Start with detailed health evidence and drill
only into weak dimensions or concerns the user named. Inspect implicated code
before turning a score into a finding.

This command is read-only. Report the meaningful dimensions, concrete ranked
offenders, coverage limits, and the fixes with the best expected value.

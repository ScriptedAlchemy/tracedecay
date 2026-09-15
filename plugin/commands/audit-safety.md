---
description: Audit a repository or directory for concrete ship-blocking code risks.
argument-hint: "[path]"
---

# Audit safety

Audit the whole repository, or `$ARGUMENTS` when it names a directory. This is
read-only. Follow the bundled `reviewing-changes` safety-audit guidance and use
only the scans the requested scope warrants: unsafe patterns, unfinished work,
dead or unmounted code, diagnostics, and structural test risk.

Confirm reachability and a concrete failure mode before reporting a finding.
Test panics and the mere presence of an unsafe block are not defects. Report
prioritized findings with their file, enclosing symbol, evidence, and practical
follow-up; do not fix them in this command.

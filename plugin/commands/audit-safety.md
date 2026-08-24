---
description: Audit the repo or a directory for ship-blocking risk, panic sites, risk markers, dead code, and untested high-risk symbols.
argument-hint: "[path]"
---

# Audit safety

Run a read-only ship-readiness sweep over the whole repo, or `$ARGUMENTS` if a directory was given. Report findings; do not fix them here.

1. Panic & unsafe sites → `tracedecay_unsafe_patterns` (use `kinds` to narrow to `unwrap`/`unsafe`, `exclude_tests: true` for production-only, `path` to scope). Each hit carries file, line, kind, enclosing symbol, `in_test`.
2. Unfinished work → `tracedecay_todos` (`kinds: ["FIXME","HACK","XXX","UNIMPLEMENTED"]`).
3. Unreachable code → `tracedecay_dead_code` (`include_public: true` for workspace-internal audits), `tracedecay_unused_imports`, and `tracedecay_unmounted_files` (files no build root reaches — nothing ever loads them, so the toolchain never reviewed their contents).
4. Risky and untested → `tracedecay_test_risk`: high-complexity, high-fan-in symbols with weak coverage.
5. Rank: production panic/unsafe in hot paths first (cross-check fan-in with `tracedecay_callers`), then UNIMPLEMENTED/HACK markers, then untested high-risk symbols, then dead code and imports.

`unwrap`/`panic!` inside tests is normal — respect `exclude_tests`/`in_test` before flagging. An `unsafe { }` block is a review-attention site, not automatically a finding.

Output: findings grouped Critical / Warning / Note with file + enclosing symbol, and a prioritized follow-up list. If any result includes a `tracedecay_metrics:` line, report the savings.

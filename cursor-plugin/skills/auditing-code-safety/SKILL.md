---
name: auditing-code-safety
description: Sweep the repo or a directory for ship-blocking risk — panic sites (unwrap/expect/panic/todo/unimplemented and unsafe blocks), unfinished-work markers, unreachable code, unused imports, and risky untested symbols. Use for "audit for unsafe code", "find panic sites", "is this ready to ship", a pre-release safety check, or a security-flavored code sweep.
---

# Auditing code safety

A focused ship-readiness sweep. For the full quality scorecard (complexity, duplication, docs) use `tokensave:code-health-report`; for reviewing one diff use `tokensave:reviewing-a-diff`.

## Workflow

1. **Panic & unsafe sites → `tokensave_unsafe_patterns`** (`kinds?` to narrow e.g. to `unwrap`/`unsafe`, `exclude_tests: true` for production-only, `path?`): each hit carries file, line, kind, enclosing symbol, and an `in_test` flag.
2. **Unfinished work → `tokensave_todos`** (`kinds: ["FIXME","HACK","XXX","UNIMPLEMENTED"]` for the risk-relevant subset): markers with their enclosing symbol.
3. **Unreachable code → `tokensave_dead_code`** (`include_public: true` for workspace-internal audits) and **`tokensave_unused_imports`**: dead paths hide stale assumptions and untested branches.
4. **Risky and untested → `tokensave_test_risk`** (`path?`, `limit?`): high-complexity, high-fan-in symbols with weak coverage — where a latent bug hurts most.
5. **Rank the findings:** production panic/unsafe sites in hot paths first (cross-check fan-in with `tokensave_callers` on the worst offenders), then UNIMPLEMENTED/HACK markers, then untested high-risk symbols, then dead code and imports.

## Guardrails

- Entirely read-only and parallel-safe; this skill reports, it does not fix. Hand fixes to `tokensave:atomic-code-edits` / `tokensave:cleaning-up-dead-code`, verification to `tokensave:running-impacted-tests`.
- `unwrap`/`panic!` inside tests is normal — respect `exclude_tests` / `in_test` before flagging. An `unsafe { }` block is not automatically a bug; report it as a review-attention site, not a finding to "fix".

## Output

- Findings grouped **Critical** (production panic/unsafe in hot paths) / **Warning** (risk markers, untested high-risk symbols) / **Note** (dead code, unused imports), each with file + enclosing symbol, plus a prioritized follow-up list.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.

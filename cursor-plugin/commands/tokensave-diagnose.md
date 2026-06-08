---
name: tokensave-diagnose
description: Run or parse build/type-checker errors, map them to the enclosing symbols, then fix.
---

# /tokensave-diagnose

Apply the `tokensave:fixing-build-and-type-errors` skill.

- **Args:** if `$ARGUMENTS` contains pasted `cargo`/`clippy` output, route it to `tokensave_diagnose`; otherwise run `tokensave_diagnostics` (scope from `$ARGUMENTS` if given).
- Follow that skill's guardrails: prefer pasted output when available; `tokensave_diagnostics` runs the toolchain, so respect Cursor approval/run-mode.

Output: grouped diagnostics with enclosing symbols + callers, the applied fix, and a clean re-check.

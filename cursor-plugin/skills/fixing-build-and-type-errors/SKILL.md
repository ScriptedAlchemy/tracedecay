---
name: fixing-build-and-type-errors
description: Use when diagnosing or fixing compiler/type-checker errors, cargo/clippy output, tsc/pyright failures, mapped diagnostics, or build failures that need graph-anchored context.
---

# Fixing build & type errors

Use this when build or type diagnostics are relevant to the task. Prefer pasted output when available; respect Cursor approval/run-mode before running fresh toolchain checks.

## Workflow

1. **Already have raw output? → `tracedecay_diagnose`** (`cargo_output` required, `severity?`: `error`|`warning`|`all`, `include_callers?`, `max_diagnostics?`): paste full `cargo check`/`clippy`/`rustc` stderr; each diagnostic maps to the smallest containing node with up to 5 callers pre-attached. No toolchain run — cheap and safe.
2. **Need fresh diagnostics → `tracedecay_diagnostics`** (`scope`: `workspace` (default) | `package` (needs `name`) | `file` (needs `path`)): structured errors/warnings, each mapped to the enclosing graph node. Forces target dir `.tracedecay/target/`; the **first** run on a fresh tree can take minutes, later calls are sub-second.
3. **Understand the failing code:** resolve/inspect with the `tracedecay:searching-for-code` ladder; widen blast radius with `tracedecay_impact` if a fix is risky.
4. **Apply the fix → `tracedecay:atomic-code-edits`** (or your normal edit tools).
5. **Re-check** with the cheapest applicable diagnostic path, then verify behavior via `tracedecay:running-impacted-tests`.

## Guardrails

- `tracedecay_diagnostics` runs `cargo`/`tsc`/`pyright` and is the only heavyweight call here; `tracedecay_diagnose` only parses text you provide — prefer it when you already captured the output.
- `tracedecay_diagnostics` is multi-language (cargo/tsc/pyright); `tracedecay_diagnose` is Rust/cargo-specific.

## Output

- The grouped diagnostics with enclosing symbols + callers, the applied fix, and a clean re-check.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.

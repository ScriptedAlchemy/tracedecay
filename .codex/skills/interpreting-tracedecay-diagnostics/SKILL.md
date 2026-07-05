---
name: interpreting-tracedecay-diagnostics
description: Use when interpreting TraceDecay compiler diagnostics, `tracedecay tool diagnose`, `tracedecay tool diagnostics`, mapped errors, affected symbols, or build/type failure output.
---

# Interpreting TraceDecay Diagnostics

TraceDecay diagnostics turn raw compiler output into mapped symbols, callers,
and likely test scope. Use them before eyeballing cargo, clippy, tsc, or
pyright output.

## Workflow

1. If raw stderr already exists, pass it to
   `tracedecay tool diagnose --args '{"cargo_output":"..."}'`.
2. If no stderr exists, run `tracedecay tool diagnostics` instead of a broad
   shell build first.
3. Read diagnostics by group:
   - parser count: did TraceDecay recognize the compiler output?
   - mapped node: which function, impl, type, or file owns the failure?
   - callers/dependents: what may break after the fix?
   - affected tests: what narrow verification should run?
4. If parser count is zero, rerun the native command only long enough to
   capture exact stderr, then diagnose that text. Do not manually scan pages
   of compiler output.
5. If output is truncated and includes a handle, retrieve or narrow before
   rerunning broad diagnostics.

## Interpretation

- Unmapped diagnostics usually mean parse coverage or file mapping is missing,
  not that the error is unimportant.
- Many errors in one file often share one signature, enum variant, or feature
  gate root cause. Fix the first mapped owner, then rerun diagnostics.
- For Rust enum pattern errors, prefer `..` in matches when future fields are
  intentionally ignored.
- A mapped test target is a recommendation. Run it, then broaden only if the
  changed symbol is shared or public.

## Deliverable

Report symptom, mapped owner, root cause, patch, and verification command. If
diagnostics could not parse the output, include the exact command that produced
the stderr sample.

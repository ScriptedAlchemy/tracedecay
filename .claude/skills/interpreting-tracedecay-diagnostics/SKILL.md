---
name: interpreting-tracedecay-diagnostics
description: "Interpret TraceDecay mapped compiler diagnostics to locate a build or type failure and its affected callers."
---

# Interpreting TraceDecay Diagnostics

Use mapped diagnostics when symbol ownership or affected callers would help
resolve a compiler failure. If existing compiler output already identifies a
local error, work from it without launching another diagnostic pass.

1. Map existing stderr with
   `tracedecay tool diagnose --args '{"cargo_output":"..."}'`.
   `tracedecay tool diagnostics` reads retained diagnostic evidence; it does
   not run the compiler. For fresh evidence, run the narrow native compiler
   command appropriate to the failure, then map that output if useful.
2. Read recognized count, mapped owner, callers/dependents, and affected tests.
   Many diagnostics can share one signature, enum, or feature root cause;
   inspect that authority before patching callers individually.
3. Unmapped or unrecognized output remains valid evidence. Read the raw stderr
   and use the native tool when mapping is unavailable; do not repeatedly build
   merely to obtain a mapped result.
4. If output is truncated with a retrieval handle, retrieve or narrow it before
   repeating an expensive diagnostic run.

The legacy `scripts/diagnose-summary.sh` can hide malformed output as success.
Use the direct tool output above until that helper validates typed responses
and preserves failures; do not treat its empty summary as a clean build.

Mapped tests are suggestions. For a fix, run the narrow relevant check and
broaden when changed behavior or remaining uncertainty warrants it. Report the
root cause, patch or recommendation, and verification; distinguish parser gaps
from compiler failures.

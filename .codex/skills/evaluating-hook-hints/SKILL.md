---
name: evaluating-hook-hints
description: "Evaluate TraceDecay hook hint relevance, repetition, and host-visible output when auditing or changing hint behavior."
---

# Evaluating Hook Hints

Inspect the hint classifier, dedupe state, affected host adapters, and relevant
transcript evidence before changing behavior. Current sources live under
`crates/tracedecay-agent-hosts/src/hooks/`; locate the current modules rather
than relying on historical root `src/hooks/` paths.

For transcript regressions, render the selected session with
`scripts/render-codex-hook-inputs.py /path/to/rollout.jsonl --all --limit 5`.
Choose the user's relevant sessions; do not scan a fixed operator home by default.

Judge relevance, appropriate silence, compactness, and dedupe after a category
has already been shown. Prefer tightening the responsible classifier or dedupe
behavior over broad static instructions. Normal prompts should not acquire
noisy hook-completion wrappers; no hint is a valid outcome.

For behavior changes, use real and synthetic cases that exercise the changed
failure and neighboring cases that should remain silent. Read
[hint-eval-signals.md](references/hint-eval-signals.md) to choose classifier
evals versus adapter tests. A review-only pass does not require running builds.

Run relevant existing evals in the owning package:
`cargo test -p tracedecay-agent-hosts --lib hooks::tool_hints::evals -- --nocapture`.
For an adapter change, run its focused tests separately; Cargo accepts one name
filter per invocation. Confirm non-zero test counts. Audit all supported hosts
when shared hook behavior changes; test affected behavior without repeating
identical substrate cases across every host.

Preserve contextual hint budgets and the current bounded repetition policy;
do not weaken limits to hide excessive hints. Report observed behavior,
changes or findings, and the relevant verification.

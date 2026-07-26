---
name: evaluating-hook-hints
description: "TraceDecay Dev: Use when auditing, testing, debugging, or improving TraceDecay hook hint behavior, especially Codex/Claude/Cursor hint injection, verbosity, repetition, dedupe, token efficiency, real transcript examples, synthetic scenarios, or `src/hooks/tool_hints*` changes."
---

# TraceDecay Dev: Evaluating Hook Hints

Use this skill for TraceDecay repo development only. These are repo-local dev instructions (`.claude/skills` / `.codex/skills`), not plugin-distributed skills.

## Workflow

1. Start with TraceDecay context for `src/hooks/tool_hints.rs`, `src/hooks/tool_hints/classifiers.rs`, the `src/hooks/tool_hints/evals/` module (`mod.rs` plus `host_cases.rs`), host adapters in `src/hooks/{codex,claude,cursor}.rs`, and recent transcript evidence.
2. Render model-visible hook input when judging verbosity:
   `scripts/render-codex-hook-inputs.py --glob '/home/zack/.codex/sessions/**/*.jsonl' --all --limit 5 --max-chars 800`.
3. Run deterministic evals before behavior edits:
   `cargo test --lib hooks::tool_hints::evals -- --nocapture`.
4. Score hints on four axes: relevance, silence when no hint is needed, compactness, and rotation/dedupe after the model has seen a category.
5. Prefer tightening classifiers, budgets, or dedupe over adding broad static instructions. Use bundled skills and tool discovery as the durable guidance layer.
6. After edits, run focused host tests for the touched adapter:
   `cargo test --lib hooks::tool_hints:: hooks::codex::tests::` for Codex prompt hints, or the matching Claude/Cursor hook tests.

## Acceptance Rules

- Hints should be contextual and short; avoid generic TraceDecay availability boilerplate.
- Real-world transcript cases and synthetic cases should both be represented in `src/hooks/tool_hints/evals/` (`host_cases.rs` holds the per-host scenarios). The module is `#[cfg(test)]`, so it only compiles under a test profile.
- A normal user prompt should not be wrapped into a noisy `UserPromptSubmit hook (completed)` model-visible user message.
- No hint is a valid outcome when the request does not need TraceDecay guidance.
- Repeated native behavior should get at most one initial hint plus one later escalation per category, bounded by the session budget.

## References

Read `references/hint-eval-signals.md` when deciding whether a scenario belongs in evals or in an adapter-specific integration test.

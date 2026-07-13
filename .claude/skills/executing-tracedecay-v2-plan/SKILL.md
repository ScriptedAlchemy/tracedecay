---
name: executing-tracedecay-v2-plan
description: Discover and use the canonical TraceDecay V2 plan-execution skill from Claude Code without maintaining a second implementation.
---

# Execute the TraceDecay V2 plan

This is a Claude Code discovery wrapper only. The canonical skill, instructions,
schemas, scripts, fixtures, and tests are owned exclusively by:

`<repo-root>/.codex/skills/executing-tracedecay-v2-plan/`

Before selecting, validating, dispatching, resuming, reviewing, or reconciling any
V2 plan work, resolve `<repo-root>` with `git rev-parse --show-toplevel` and read
the canonical `.codex/skills/executing-tracedecay-v2-plan/SKILL.md` completely.
Follow it as the sole procedural authority. If it is missing, unreadable, or
incompatible, stop with a blocker; do not reconstruct instructions from this
wrapper, cached text, plan prose, or another checkout.

Claude and Codex entrypoints must resolve the same shared state path for canonical
`plan_execution.py`. That one
`tracedecay.v2.execution-state/v1` document contains the activated graph, pinned
`tracedecay.v2.completion-ledger/v1`, tracking state, dispatch packets, and
review/test/integration/steering receipts. For bootstrap manifest resolution,
both hosts use the canonical precedence unchanged: explicit argument,
`TRACEDECAY_V2_EXECUTION_MANIFEST`, then exactly one repo-local source: either
`<repo-root>/.tracedecay/v2-execution-manifest.json` or the atomically switched
`<repo-root>/.tracedecay/v2-execution-active.json` generation. Coexistence is ambiguous
and fails closed. Never infer or maintain a
host-specific manifest, ledger, state, receipt, cache, output, or “current” path
under `.claude`, `.codex`, `$CLAUDE_HOME`, `$CODEX_HOME`, or another host store.
The canonical bootstrap command stages manifest/state together and atomically
switches `<repo-root>/.tracedecay/v2-execution-active.json`. State resolution is
explicit `--graph`, `TRACEDECAY_V2_EXECUTION_STATE`, then exactly one repo-local source:
the legacy direct `<repo-root>/.tracedecay/v2-execution-state.json` or that active
generation. Coexistence is ambiguous and fails closed.
If the two entrypoints do not resolve the same state input, stop rather than
merge, mirror, or choose newer host-local state. Missing state blocks dispatch
only; read-only inventory, audit, and plan review may continue without claiming
that a slice is ready.

Until activation, plan authoring, finalization, order auditing, and review use
the canonical inventory and cited plan sections without treating the expected
missing operational state as a plan-review failure.

Run only the canonical scripts, for example:

```bash
python3 .codex/skills/executing-tracedecay-v2-plan/scripts/plan_inventory.py
python3 .codex/skills/executing-tracedecay-v2-plan/scripts/compile_plan_authority.py \
  --root "$(git rev-parse --show-toplevel)" \
  --canonical-ref refs/heads/codex/tracedecay-total-redesign-plan \
  --manifest-output docs/plans/tracedecay-v2/execution-authority.json \
  --state-output .tracedecay/v2-execution-state.candidate.json
python3 .codex/skills/executing-tracedecay-v2-plan/scripts/compile_plan_authority.py \
  --root "$(git rev-parse --show-toplevel)" \
  --canonical-ref refs/heads/codex/tracedecay-total-redesign-plan --check
python3 .codex/skills/executing-tracedecay-v2-plan/scripts/bootstrap_execution.py \
  --manifest docs/plans/tracedecay-v2/execution-authority.json \
  --state-export .tracedecay/v2-execution-state.candidate.json \
  --root "$(git rev-parse --show-toplevel)" \
  --canonical-ref refs/heads/codex/tracedecay-total-redesign-plan
python3 .codex/skills/executing-tracedecay-v2-plan/scripts/plan_execution.py \
  --root "$(git rev-parse --show-toplevel)" \
  --canonical-ref refs/heads/codex/tracedecay-total-redesign-plan --next-ready
python3 -m unittest discover \
  -s .codex/skills/executing-tracedecay-v2-plan/scripts -p 'test_*.py'
```

Never create or invoke `.claude/skills/executing-tracedecay-v2-plan/scripts`,
fixtures, tests, copied modules, generated mirrors, or fallback implementations.
Updates land once under `.codex`; this wrapper may change only when discovery or
canonical-path instructions change.

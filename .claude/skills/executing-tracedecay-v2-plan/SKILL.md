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

Claude and Codex entrypoints must resolve and pass the same explicit `--graph`
path to the canonical `plan_execution.py`. That one
`tracedecay.v2.execution-state/v1` document contains the activated graph, pinned
`tracedecay.v2.completion-ledger/v1`, tracking state, dispatch packets, and
review/test/integration/steering receipts. For bootstrap manifest resolution,
both hosts use the canonical precedence unchanged: explicit argument,
`TRACEDECAY_V2_EXECUTION_MANIFEST`, then
`<repo-root>/.tracedecay/v2-execution-manifest.json`. Never infer or maintain a
host-specific manifest, ledger, state, receipt, cache, output, or “current” path
under `.claude`, `.codex`, `$CLAUDE_HOME`, `$CODEX_HOME`, or another host store.
If the two entrypoints are not given the same explicit state input, stop rather
than merge, mirror, or choose newer host-local state.

Run only the canonical scripts, for example:

```bash
python3 .codex/skills/executing-tracedecay-v2-plan/scripts/plan_inventory.py
python3 .codex/skills/executing-tracedecay-v2-plan/scripts/plan_execution.py \
  --graph /explicit/shared/v2-execution-state.json \
  --root "$(git rev-parse --show-toplevel)" \
  --canonical-ref refs/heads/master --next-ready
python3 -m unittest discover \
  -s .codex/skills/executing-tracedecay-v2-plan/scripts -p 'test_*.py'
```

Never create or invoke `.claude/skills/executing-tracedecay-v2-plan/scripts`,
fixtures, tests, copied modules, generated mirrors, or fallback implementations.
Updates land once under `.codex`; this wrapper may change only when discovery or
canonical-path instructions change.

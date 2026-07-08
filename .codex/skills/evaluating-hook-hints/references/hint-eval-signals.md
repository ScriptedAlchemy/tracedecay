# Hint Eval Signals

Use deterministic evals for pure classifier/dedupe behavior:

- prompt text maps to the expected `HintCategory`
- shell/tool event maps to the expected `HintCategory`
- no hint should be emitted for generic or already-covered situations
- emitted hint text must stay compact and avoid static availability boilerplate
- repeated categories rotate out through `ToolHintDedupe`

Use adapter tests when behavior depends on host-specific hook shape:

- Codex `UserPromptSubmit` developer/user message construction
- Claude/Cursor hook output schema
- persisted hint analytics rows
- workspace classification: initialized project, unindexed git workspace, generic non-code chat

Use raw transcript rendering when reviewing regressions:

```sh
scripts/render-codex-hook-inputs.py /path/to/rollout.jsonl --all --limit 5
```

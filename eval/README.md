# Behavioral Memory-Hygiene Evals

Scenario-driven evals for tracedecay's holographic memory: they seed a throwaway
fixture project, drive the real `tracedecay` write/curation paths (and optionally
a real agent), then assert on **end-state** with plain SQL against the fixture's
`.tracedecay/tracedecay.db`.

Full documentation: [`docs/memory-evals.md`](../docs/memory-evals.md).

## Layers

| Layer | Driver | Cost | Where it runs |
| --- | --- | --- | --- |
| Deterministic | scripted tool-call sequences (no LLM) | free | `cargo nextest run -E 'binary(=memory_suite)'` (part of normal CI) |
| Real-model | Hermes (or `cursor-agent`) driving the generated tracedecay plugin | model credits | `eval/run_real_model.py`, cost-gated, never in CI |

The real-model layer is gated behind **both** `--agent-turn` and
`--i-understand-model-cost` (pattern adopted from mnemon). Without both flags it
records a blocked report and exits.

```bash
# deterministic layer (the eval lives in the memory_suite test binary)
cargo nextest run -E 'binary(=memory_suite)'

# real-model layer (consumes model credits/quota)
python3 eval/run_real_model.py --scenario memory-no-pollution \
    --agent-turn --i-understand-model-cost
```

The real-model runner creates a unique temporary user home for each run and
installs the normal user-level Hermes integration there. It never selects a
named Hermes profile or uses `HERMES_HOME` as TraceDecay storage.

Run reports land under `eval/runs/<timestamp>/` (gitignored).

## Scenarios

Declared in [`scenarios/*.json`](scenarios/); one file per scenario with setup
facts/files, scripted deterministic sequences (well-behaved + violation), the
real-model prompts, and SQL/curate-report assertions. Stable scenarios hard-fail
when a bad write is accepted and neither the write path nor curator dry-run
neutralizes the bad state.

## Attribution

The scenario taxonomy and several prompts are adapted from the
[mnemon](https://github.com/mnemon-dev/mnemon) harness eval suite
(`harness/loops/eval/`, commit `41a9612`), licensed under Apache-2.0.

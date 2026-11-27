# Behavioral memory-hygiene evals

These holographic memory evaluations seed a disposable project and run the
`tracedecay` memory write and curation paths. Some runs also use a real agent.
Each evaluation checks the final state with SQL against the fixture's
`.tracedecay/tracedecay.db`.

Full documentation: [`docs/memory-evals.md`](../../docs/memory-evals.md).

## Layers

| Layer | Driver | Cost | Where it runs |
| --- | --- | --- | --- |
| Deterministic | scripted tool-call sequences (no LLM) | free | `bazel test //crates/tracedecay:memory_suite --test_output=errors` |
| Real-model | Hermes or Cursor Composer (`cursor-agent`) driving the generated tracedecay plugin | model credits | `evals/memory/run_real_model.py`, cost-gated, never in CI |

The real-model runner requires `--agent-turn` and `--i-understand-model-cost`,
following mnemon's cost check. If either flag is missing, the runner records a
blocked report and exits.

```bash
# deterministic layer (the eval lives in the memory_suite test binary)
bazel test //crates/tracedecay:memory_suite --test_output=errors

# real-model layer (consumes model credits/quota)
python3 evals/memory/run_real_model.py --scenario memory-no-pollution \
    --agent-turn --i-understand-model-cost
```

The real-model runner creates a unique temporary user home for each run and
installs the normal user-level Hermes integration there. It never selects a
named Hermes profile or uses `HERMES_HOME` as TraceDecay storage.
Each scenario starts an isolated `tracedecay daemon run` with a private socket
and waits for it before `init`. The runner reports setup, daemon, agent, and
cancellation failures as errors. It attempts cleanup on every exit path and
reports retained paths and cleanup errors if it cannot stop its own work or
delete its files.

Run reports land under `evals/memory/runs/<timestamp>/` (gitignored).

## Scenarios

Each file in [`scenarios/*.json`](scenarios/) defines setup facts and files,
deterministic sequences for expected behavior and violations, real-model
prompts, and assertions against SQL results and curation reports. A stable
scenario fails if the write path accepts a bad write and neither the write path
nor the curator dry-run neutralizes it.

## Attribution

The scenario taxonomy and several prompts are adapted from the
[mnemon](https://github.com/mnemon-dev/mnemon) harness eval suite
(`harness/loops/eval/`, commit `41a9612`), licensed under Apache-2.0.

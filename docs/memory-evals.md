# Behavioral Memory-Hygiene Evals

tracedecay's holographic memory is only useful if agents keep it clean: no run
noise, no secrets, no duplicate preferences, and recall that actually works
across sessions. The eval suite under [`eval/`](../eval/) tests those
*behaviors* end-to-end instead of unit-testing internals.

The scenario taxonomy, the cost-gating pattern, and several prompts are adapted
from the [mnemon](https://github.com/mnemon-dev/mnemon) harness eval suite
(`harness/loops/eval/`, commit `41a9612`), licensed under Apache-2.0. See the
repository `NOTICE` file.

## Harness design

Every scenario follows the same shape:

1. **Fixture** — a throwaway project directory is created, `tracedecay init`
   builds a real `.tracedecay/` store, and the scenario's setup block seeds
   facts (through the real `fact_store` write path, so HRR vectors and FTS
   stay consistent) plus optional workspace files. Trust scores,
   retrieval counts, and source labels are then pinned with SQL.
2. **Drive** — either a scripted tool-call sequence (deterministic layer) or a
   real agent prompted over the generated tracedecay integration (real-model
   layer) exercises the memory write/recall/curation paths.
3. **Assert** — end-state is checked with plain SQL against the fixture's
   `.tracedecay/tracedecay.db` (plus structured checks of the
   `tracedecay memory curate` dry-run report for curation scenarios).
4. **Cleanup** — the fixture directory is deleted; nothing touches the host
   project's stores.

Scenario declarations live in [`eval/scenarios/*.json`](../eval/scenarios/)
and are shared by both layers, so prompts, setup, and assertions can never
drift apart.

### Deterministic layer (no LLM, runs in CI)

`tests/memory_eval_test.rs` replays scripted tool-call sequences through the
real `tracedecay` binary — the same code path MCP tool calls hit — and runs in
the normal `cargo test --workspace` suite (so it is part of the existing CI
test job on Linux/macOS/Windows; CI never calls a model).

Each scenario runs up to two phases:

- **Well-behaved phase** — the tool sequence a hygienic agent would issue must
  leave a compliant end-state (all assertions pass).
- **Violation phase** — a misbehaving sequence (storing the secret, adding the
  duplicate, skipping recall) is replayed against a fresh fixture. Depending
  on the scenario's `expectation`:
  - `detect` — at least one assertion must fail, proving the assertion set
    can actually catch a misbehaving agent (instrument self-check).
  - `defend-or-detect` — either the write path or deterministic curator
    refuses/neutralizes the bad state (all assertions pass ⇒ defended), or the
    assertion set catches the violation. Stable-contract scenarios fail on
    "accepted + bad end-state" regressions.

### Real-model layer (cost-gated, never in CI)

`eval/run_real_model.py` drives a real agent through the same scenarios:

- **Hermes** (default): the runner writes a dedicated Hermes profile
  (`~/.hermes/profiles/tracedecay-eval`), runs
  `tracedecay install --agent hermes --profile tracedecay-eval
  --project-root <fixture>` so the generated plugin pins every memory tool to
  the fixture project, and then sends each scenario prompt through
  `uv run python cli.py -q …` with `HERMES_HOME` pointed at the profile.
- **cursor-agent** (experimental): `tracedecay install --agent cursor --local`
  inside the fixture, then `cursor-agent -p` with the fixture as cwd.

Adopting mnemon's cost gate, nothing model-shaped runs unless **both**
`--agent-turn` and `--i-understand-model-cost` are passed; otherwise a
`blocked` report is recorded and the runner exits with code 2:

```bash
# blocked (no flags): records eval/runs/<ts>/report.json with status=blocked
python3 eval/run_real_model.py --scenario memory-no-pollution

# real run (consumes model credits/quota)
python3 eval/run_real_model.py --scenario memory-no-pollution \
    --agent-turn --i-understand-model-cost --model gpt-5.4-mini
```

Reports and per-prompt agent transcripts land under `eval/runs/<timestamp>/`
(gitignored). Reports include per-assertion outcomes and best-effort token
usage extracted from the agent output; the raw transcript is always saved so
usage claims can be audited.

## Scenario taxonomy

| Scenario | Contract | What it guards |
| --- | --- | --- |
| `memory-no-pollution` | stable | Single-turn throwaway tokens never become facts; durable decisions still can. |
| `memory-secret-rejection` | stable | Credential-like values are rejected by the write path before they reach durable memory. |
| `memory-skip-local` | stable | Content already visible in workspace files is neither stored nor recall-churned. |
| `memory-supersede-without-dup` | stable | Preference pivots update the existing fact; naive duplicate adds must be flagged by curation dry-run for deletion of the older superseded fact. |
| `memory-multiturn-continuity` | stable | Facts stored in one session are recalled (with a real retrieval hit) in the next. |
| `memory-curation-conservatism` | stable | `tracedecay memory curate` never proposes deleting high-trust, high-access facts absent strong similarity, while genuine near-dups collapse — in dry-run and under `--apply`. |

## Adding a scenario

1. Drop a new `eval/scenarios/<id>.json` (copy an existing one; keep
   `schema_version: 1`).
2. Wire a `#[test]` for it in `tests/memory_eval_test.rs` — the
   `every_scenario_file_is_wired` test fails until you do.
3. If it has a `real_model` block it is automatically runnable through
   `eval/run_real_model.py`.

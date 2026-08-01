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

`tests/memory_suite/memory_eval_test.rs` replays scripted tool-call sequences through the
real `tracedecay` binary — the same code path MCP tool calls hit — and runs in
the normal `cargo nextest run --workspace --all-features --no-fail-fast` suite (so it is part
of the existing CI test job on Linux/macOS/Windows; CI never calls a model).

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

- **Hermes** (default): the runner creates an isolated temporary user home,
  runs `tracedecay install --agent hermes` there, and sends each scenario
  prompt through Hermes with the fixture as the working directory. The plugin
  and TraceDecay store use that isolated user profile. Hermes host files may
  live under its own home, but no Hermes profile or `HERMES_HOME` value routes
  TraceDecay storage or project identity.
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
| `memory-feedback-trust` | stable | `fact_feedback` (helpful) raises `trust_score` above the seed and appends a `memory_feedback_events` audit row. |
| `memory-ranking-retrieval-reinforcement` | stable | A frequently-retrieved fact out-ranks an equal-trust, never-retrieved rival — the `combined_score` usage boost, through the real search tool. |
| `memory-ranking-feedback-promotes` | stable | Rating one fact `helpful` and an equally-relevant rival `unhelpful` flips their order in real search results (the full feedback → trust → rank loop). |

## Adding a scenario

1. Drop a new `eval/scenarios/<id>.json` (copy an existing one; keep
   `schema_version: 1`).
2. Wire a `#[test]` for it in `tests/memory_suite/memory_eval_test.rs` — the
   `every_scenario_file_is_wired` test fails until you do.
3. If it has a `real_model` block it is automatically runnable through
   `eval/run_real_model.py`.

## Triggering & adoption scorecard

The layers above test whether the memory *engine* behaves. They do **not** test
whether a real model *chooses* to use memory unprompted — the behavior that
actually determines whether durable memory helps a user. That is the
**fact-store adoption scorecard**, built on the hermetic harness in
[`eval/hermetic/`](../eval/hermetic/).

### How it works

`eval/hermetic/run.sh` builds the dev binary, stages it at a non-cargo path,
installs the plugin into a throwaway `CLAUDE_CONFIG_DIR`/`CODEX_HOME`/
`TRACEDECAY_DATA_DIR`, indexes a project, then drives a **real** agent
(`claude -p` or `codex exec --json`) at each corpus prompt and records the
transcript. `score.py` classifies which tracedecay MCP tools and CLI commands
each session used. The adoption corpus is
[`eval/hermetic/corpora/fact-store-adoption.jsonl`](../eval/hermetic/corpora/fact-store-adoption.jsonl);
`eval/hermetic/scorecard.py` rolls a run's `results.jsonl` into an adoption %
per bucket.

```bash
ENV=$(eval/hermetic/run.sh setup --agent codex --debug)   # subscription; no API
eval/hermetic/run.sh index --env-dir "$ENV" --project <repo>
eval/hermetic/run.sh run   --agent codex --env-dir "$ENV" \
  --corpus eval/hermetic/corpora/fact-store-adoption.jsonl
python3 eval/hermetic/scorecard.py "$ENV"/results/results.jsonl \
  --corpus eval/hermetic/corpora/fact-store-adoption.jsonl
eval/hermetic/run.sh teardown --env-dir "$ENV"
```

Cost note: `codex` uses the local subscription (no per-token API); `claude`
(`--model sonnet|opus`) bills the Anthropic API. `run.sh` supports
`--agent claude` and `--agent codex` only (no codex app-server mode).

### Scoring rules (why they matter)

- **Prompts are ambiguous, never leading.** They are ordinary user turns —
  state a preference, ask a question a stored fact answers, give a correction
  that makes a fact stale — with **no** mention of memory/tools/rating. A prompt
  that says "remember this" or "rate that fact" measures *compliance*, not
  natural adoption, and is disallowed.
- **Path-agnostic.** tracedecay is reachable via the MCP tools **or** the
  `tracedecay tool …` CLI; a scenario counts as triggered on **either** path,
  or on its `verify_cmd` store-state change (fact added / `helpful_count` /
  `unhelpful_count` / `fact_count`). Codex empirically reaches for the CLI, so
  an MCP-only pass badly undercounts it.
- **Adoption ≠ precision.** "Use memory when you should" (write/recall/feedback)
  is scored separately from restraint ("don't store ephemeral state",
  `factstore-precision`). Correct abstention is **not** counted as adoption, so
  it can't inflate the headline.

### Findings (2026-07-06, codex CLI)

- **Baseline: 40% overall adoption**, and **proactive-store 0%** — codex used
  *zero* memory tools when simply told a durable preference. Recall 50%,
  feedback 50%, precision 100%.
- **Root cause:** codex's install shipped only the plugin bundle
  (model-invoked skills) with **no always-on instruction** — the personal
  digest is bundle-excluded, and install had stopped writing a profile-level
  `AGENTS.md`. The model never *considered* memory. Sharpening the `fact_store`
  tool description did **not** help (the model never reached it).
- **Fix:** `install --agent codex` writes the shared prompt rules plus a
  proactive-memory paragraph to the **global** `~/.codex/AGENTS.md`
  (`crates/tracedecay-agent-hosts/src/agents/codex.rs::install_codex_global_prompt_rules`).
- **Result (n=1): proactive-store 0% → 100%, overall adoption 40% → 60%.**

**The lever for adoption is the always-on, profile-level instruction the model
sees every turn — not tool metadata.** The same behavioral memory rule belongs
in every host's always-on surface (`CLAUDE.md`, `AGENTS.md`,
`STANDARD_PARAGRAPHS`). Treat single-run numbers as directional; use
`run … --reps N` (scorecard aggregates rows across reps) for a confident
figure.

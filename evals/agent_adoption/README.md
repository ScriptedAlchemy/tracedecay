# Agent-adoption evals

The first **agent-in-the-loop** eval tier for TraceDecay. Where
`src/hooks/tool_hints/evals/` grade what the *classifier* would decide offline,
this harness launches **real headless Claude Code and Codex agents** against a
small indexed fixture project and grades what the agents **actually do** — which
tool they reach for first, whether they fall back to raw `grep`/`glob`/`cat`
before any tracedecay tool, whether they hit the right answer within a tool
budget, and whether they ever rate a memory fact they relied on.

It exists because measured baselines show the gap this tier is meant to track:
agents default to native grep/read until manually steered; fact feedback almost
never happens; and tool-driven session recovery has failed end-to-end in the
wild. Those are behaviors, not classifier outputs, so only a live harness can
score them.

## Layout

```
evals/agent_adoption/
  scenarios/*.json     # 14 active + 1 deferred labeled scenarios
  fixture/             # the orders_fixture crate (copied to a temp dir per run)
  fixture_broken/      # orders.rs with a planted type error (diagnostics scenario)
  run.sh               # runner: build+index fixtures, seed facts, drive agents, grade
  grade.py             # deterministic grader + dual-host transcript normalizer
  README.md
```

## How to run

Live agent runs cost tokens, so they are gated behind `TRACEDECAY_AGENT_EVALS=1`.
Nothing here is wired into `cargo test` or CI.

```sh
# Free dry run: builds + indexes the fixtures, seeds facts, prints the exact
# scenario x host commands, launches no agent.
evals/agent_adoption/run.sh

# Full live run, Claude only (cheapest default model = haiku):
TRACEDECAY_AGENT_EVALS=1 HOSTS=claude evals/agent_adoption/run.sh

# Smoke: three representative scenarios, one host:
TRACEDECAY_AGENT_EVALS=1 HOSTS=claude \
  SCENARIOS="explore_reserve_stock recall_discount_decision feedback_currency" \
  evals/agent_adoption/run.sh

# Both hosts, a stronger model, longer budget:
TRACEDECAY_AGENT_EVALS=1 HOSTS="claude codex" \
  CLAUDE_MODEL=sonnet SCENARIO_TIMEOUT=360 evals/agent_adoption/run.sh
```

Outputs land in a throwaway work dir (`$TMPDIR/agent-evals.XXXX/run/`):
`scoreboard.json`, `report.md`, per-scenario `*.stdout.jsonl` transcripts, and
`*.meta.json`. Set `EVAL_OUT=<dir>` to also copy the scoreboard + report
somewhere durable. Transcripts and the scoreboard contain machine-absolute temp
paths, so do **not** commit run artifacts.

### Env knobs

| var | default | meaning |
|-----|---------|---------|
| `TRACEDECAY_AGENT_EVALS` | unset | must be `1` to launch agents (else dry run) |
| `HOSTS` | `claude` | `claude`, `codex`, or `claude codex` |
| `SCENARIOS` | all active | space-separated scenario ids to run |
| `EVAL_INCLUDE_DEFERRED` | `0` | also run `status:"deferred"` scenarios |
| `CLAUDE_MODEL` | `haiku` | `claude --model` alias |
| `CODEX_MODEL` | codex default | `codex exec -m` |
| `SCENARIO_TIMEOUT` | `240` | per scenario wall-clock seconds |
| `TRACEDECAY_BIN` | from `PATH` | tracedecay binary |
| `EVAL_OUT` | unset | dir to copy scoreboard + report into |

### Cost expectations

One scenario x host is a single short headless agent turn (a few tool calls).
14 active scenarios x 1 host ≈ 14 short agent sessions. Keep smoke runs to 2-3
scenarios. `haiku` is the cheapest model that still does tool use; use a real
coding model (`sonnet`/`opus`, or the Codex default) when you actually want a
representative adoption baseline rather than a harness smoke test. `--max-turns`
does not exist in the installed Claude Code (2.1.x), so the only hard guardrail
is `SCENARIO_TIMEOUT`; the tool budget is scored, not enforced.

## Store isolation & auth

The runner points `TRACEDECAY_DATA_DIR` at a throwaway dir and sets
`TRACEDECAY_ENABLE_GLOBAL_DB=0`, so the fixture graph and seeded facts never
touch your real tracedecay store, and `tracedecay init` stays fast (indexing the
multi-thousand-node global DB is what makes a naive `init` hang). `HOME` is left
alone, so Claude OAuth and `~/.codex` auth keep working; the `tracedecay serve`
MCP process the agents spawn inherits `TRACEDECAY_DATA_DIR` and therefore sees
the same fixture store the runner seeded.

## Known confound: ambient steering

Agents run inside the fixture temp dir but still load your **user-global**
`~/.claude/CLAUDE.md` (Claude) / `~/.codex` config. If that global config
contains a strong "always use tracedecay, never grep" mandate, it will inflate
adoption numbers — the harness then measures *steered* behavior, not cold-start
default behavior. To measure a true cold-start baseline, run against a profile
whose global memory does **not** pre-steer tool choice. The tracedecay plugin's
own hint hooks are part of the product and are intentionally left in.

## Scenario schema

Each `scenarios/<id>.json`:

```jsonc
{
  "id": "explore_reserve_stock",
  "category": "code_exploration",
  "hosts": ["claude", "codex"],       // which hosts this applies to
  "fixture": "main",                  // "main" or "broken"
  "status": "active",                 // or "deferred"
  "prompt": "How does stock reservation work in this crate?",
  "expected_tools": {
    "required_first": [ ... ],        // first meaningful tool must be one of these to pass
    "acceptable":     [ ... ],        // not first-choice but fine later
    "forbidden_first":["Grep","Glob"],// raw search/read tools
    "forbidden_bash": ["grep","rg ","find ","cat "]  // Bash substrings that count as raw search
  },
  "ground_truth": ["reserve_stock", "check_availability"], // fragments expected in the answer
  "max_tool_calls": 6,                // efficiency budget
  "seeded_fact": "currency_fact_id",  // (optional) key in seeded_facts.json
  "grade_feedback": true              // (optional) also score fact_feedback behavior
}
```

### Scoring (per scenario, 0..1)

Weighted, applicable-subscore-normalized:

| subscore | weight | pass condition |
|----------|--------|----------------|
| `first_tool_choice` | 0.30 | first meaningful tool ∈ `required_first` (tracedecay_grep counts; raw Grep/Glob/cat do not) |
| `not_forbidden_first` | 0.25 | no forbidden raw-search tool before the first tracedecay tool |
| `outcome` | 0.25 | fraction of `ground_truth` fragments present in the final answer |
| `efficiency` | 0.10 | meaningful tool-call count ≤ `max_tool_calls` |
| `feedback` | 0.30 | (feedback scenarios) `tracedecay_fact_feedback` called `helpful` on the seeded fact |

Aggregated per host into `scoreboard.json` (mean score plus each rate) and a
compact `report.md`.

### Transcript normalizer

`grade.py` auto-detects and normalizes both formats to one event model
(ordered tool calls + final answer):

* **Claude** `--output-format stream-json`: `assistant` events carry
  `tool_use` items; the `result` event carries the final answer.
* **Codex** `codex exec --json`: `msg.type` of `mcp_tool_call_begin` /
  `exec_command_begin` / `function_call` become tool calls; `agent_message`
  is the final answer.

Host-specific MCP tool names collapse to canonical form, e.g.
`mcp__plugin_tracedecay_tracedecay__tracedecay_context` → `tracedecay_context`.

> The Claude path is validated by a live smoke run. The Codex path is
> implemented to the documented `codex exec --json` event schema but should be
> confirmed against a live Codex transcript before trusting Codex aggregates
> (run one `codex` scenario and eyeball `report.md`).

## Adding a scenario

1. Drop a new `scenarios/<id>.json` following the schema above.
2. If it needs new fixture symbols, add them to `fixture/` (kept intentionally
   small and unambiguously named). Re-run the dry run to confirm indexing.
3. If it depends on a seeded fact, add the seed in `run.sh` (`seed_fact`) and a
   key in the emitted `seeded_facts.json`, then reference it via `seeded_fact`.
4. Run the dry run, then a single-scenario live run to sanity-check grading.

## Deferred: session recovery

`scenarios/session_recovery.json` is `status:"deferred"`. Grading whether an
agent recovers prior-session context requires a real prior host session bound to
the fixture project path (recorded + ingested), which the fixture cannot
reproduce deterministically from static files. It is kept in the corpus as a
documented gap; wire it up once the harness can record and replay a seed session
against the fixture path. Run it with `EVAL_INCLUDE_DEFERRED=1`.

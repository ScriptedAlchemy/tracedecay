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
  scenarios/*.json     # labeled scenarios (neutral prompts — see "Doctrine")
  fixture/             # the orders_fixture crate (copied to a temp dir per run)
  fixture_broken/      # orders.rs with a planted type error (diagnostics scenario)
  run.sh               # runner: build+index fixtures, seed facts, drive agents, grade
  grade.py             # deterministic grader + normalizer + neutrality lint + channels + hint-drift guard
  selftest.py          # offline grader self-tests (no agents, no tokens)
  README.md
```

## Doctrine: prompts stay neutral, adoption is earned

**Scenario prompts must never name `tracedecay`, `mcp`, a specific tool, or a
skill.** They are neutral, natural task prompts ("How does stock reservation
work?"). Whether an agent reaches for a tracedecay tool must be *earned* by the
discovery machinery under test — the MCP tool descriptions, the plugin
description, skill triggering, and the hint engine — not begged for in the
prompt. A prompt that says "use tracedecay_context" would measure obedience, not
discovery.

This is enforced, not just documented: `grade.py --lint-only` fails if any
prompt contains a banned token (`tracedecay`, `mcp`, a `tracedecay_*` tool name,
an unambiguous tool base-name like `fact_store`, or a bundled skill id like
`exploring-code`). `run.sh` runs the lint **before building fixtures or spending
a token**, so a leaked prompt aborts the run. `selftest.py` covers the lint with
positive + negative cases.

## How to run

Live agent runs cost tokens, so they are gated behind `TRACEDECAY_AGENT_EVALS=1`.
Nothing here is wired into `cargo test` or CI.

```sh
# Free dry run: builds + indexes the fixtures, seeds facts, prints the exact
# scenario x host commands, launches no agent.
evals/agent_adoption/run.sh

# Full live run, Claude only (default matrix: Opus and Sonnet):
TRACEDECAY_AGENT_EVALS=1 HOSTS=claude evals/agent_adoption/run.sh

# Smoke: three representative scenarios, one host:
TRACEDECAY_AGENT_EVALS=1 HOSTS=claude \
  SCENARIOS="explore_reserve_stock recall_discount_decision feedback_currency" \
  evals/agent_adoption/run.sh

# Both host matrices, longer budget:
TRACEDECAY_AGENT_EVALS=1 HOSTS="claude codex" \
  SCENARIO_TIMEOUT=360 evals/agent_adoption/run.sh
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
| `CHANNELS` | `full` | ablation conditions: any of `full no-hints no-skills bare cli-only` |
| `SCENARIOS` | all active | space-separated scenario ids to run |
| `EVAL_INCLUDE_DEFERRED` | `0` | also run `status:"deferred"` scenarios |
| `CLAUDE_MODELS` | `opus sonnet` | space-separated `claude --model` matrix |
| `CODEX_MODELS` | `gpt-5.5 gpt-5.6-terra` | space-separated `codex exec -m` matrix |
| `SCENARIO_TIMEOUT` | `240` | per scenario wall-clock seconds |
| `TRACEDECAY_BIN` | candidate branch (live) | explicit tracedecay binary override |
| `EVAL_OUT` | unset | dir to copy scoreboard + report into |

### Cost expectations

One scenario x host x model is a single short headless agent turn. There are 44
active scenarios, so keep smoke runs to 2-3 scenarios before starting the full
Opus/Sonnet and GPT-5.5/Terra matrix. `--max-turns`
does not exist in the installed Claude Code (2.1.x), so the only hard guardrail
is `SCENARIO_TIMEOUT`; the tool budget is scored, not enforced.

## Store isolation & auth

The runner points `TRACEDECAY_DATA_DIR` at a throwaway dir and sets
`TRACEDECAY_ENABLE_GLOBAL_DB=0`, so the fixture graph and seeded facts never
touch your real tracedecay store, and `tracedecay init` stays fast (indexing the
multi-thousand-node global DB is what makes a naive `init` hang). Each host gets
a throwaway `HOME` and config directory. Only its auth file is copied in, mode
`0400`; the copy is deleted on runner exit. The real Claude/Codex profile is
never loaded or mutated.

Live runs build a debug `tracedecay` binary from the checked-out candidate
branch with ordinary Cargo and use it for fixture setup, MCP, hooks, and the
throwaway Codex plugin install.
`TRACEDECAY_BIN` is an explicit override for release-binary comparisons. Every
Claude condition, including `full`, loads the candidate `plugin/` via
`--plugin-dir`; Codex `full` installs candidate assets into its throwaway
profile. Ambient global plugins, skills, hooks, and steering cannot leak in.

## Safety boundary

Codex runs `--ephemeral` with approval policy `never` inside its
`workspace-write` sandbox, rooted at the throwaway fixture. Claude has no
equivalent OS sandbox: it runs noninteractively in `dontAsk` mode with an
allowlist limited to read/search, agent/skill delegation, candidate TraceDecay
MCP tools, and `tracedecay tool ...` CLI fallback. TraceDecay edit tools may
still mutate the fixture and throwaway store by design, and model/API processes
still have network and any OS-keychain access granted by the host. Run live
evals only against trusted candidate code; the Claude allowlist is a permission
boundary, not an OS sandbox.

## Fixture contents

`fixture/` is the tiny `orders_fixture` crate. It is deliberately small and
unambiguously named so grounded answers (symbol names, call edges, duplicated
logic) are unmistakable. Beyond the order-flow modules it carries, `run.sh`
enriches the copied fixture at setup so more scenario tiers are gradable:

* **Git history (main fixture).** `build_fixture` seeds a real multi-branch
  history — `init` on the default branch, a `feature/pricing-notes` branch with
  two commits, a divergent commit on the default branch, and a `--no-ff` merge.
  This gives `commit_context` / `diff_context` / branch tooling genuine commits
  and a merge to reason about. **Note:** tracedecay only *tracks* the checked-out
  branch by default, so `branch_list`/`branch_diff` see one tracked branch unless
  you add multi-branch indexing; `commit_context` does surface the full commit
  log and the merge.
* **Audit-tier plants (`src/audit.rs`).** A module kept **out** of the order
  flow (so exploration/impact ground truth is untouched) plants three ship-risk
  markers: a genuine unused import (`use std::collections::BTreeMap;`), a `TODO`
  marker, and a needless `unsafe { }` block.
  * The `TODO` is reliably surfaced by `tracedecay_todos` (two matches).
  * All three plants now have working tool coverage:
    * `tracedecay_unused_imports` flags the planted `BTreeMap` at
      `src/audit.rs` (whole-crate run: `unused_import_count` 1). This was
      previously an empty-result gap: the resolver drops `Uses` edges for
      std/foreign-crate imports, so the tool falls back to a text scan of the
      file — but it counted the import identifier appearing in the **comment
      directly above the `use`** as a real reference, so the crate-unique
      import was read as "used" and never surfaced. Fixed by masking comments
      and string/char literals (`mask_rust_noise` in
      `src/mcp/tools/handlers/analysis.rs`) before the scan, plus a dedicated
      `unused_imports_md` renderer. Anchored by `analysis_unused_import_hunt`.
    * `tracedecay_unsafe_patterns` surfaces the needless `unsafe { }` block
      (dedicated `risky_patterns_md` renderer). Anchored by
      `analysis_unsafe_panic_audit`.
    * `tracedecay_todos` surfaces the `TODO`. Anchored by `diag_todo_inventory`.

### Seeding prior sessions — a gap

Scenarios like `session_recovery` want prior host sessions bound to the fixture
path (for `message_search` / `sessions_for`). The `tracedecay sessions import`
CLI **sweeps provider directories under `HOME`** (`~/.claude/projects/...`,
`~/.codex/...`); it has **no file-input mode**. Seeding a hermetic session would
mean writing a synthetic transcript into the operator's real `~/.claude` (which
the harness deliberately leaves untouched for auth) at the exact cwd-encoded
path, then ingesting — not hermetic and not cheap. So session-recovery packs stay
**deferred**. A future `tracedecay sessions import --from-file <jsonl>`
affordance; until then this tier is a documented gap.

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
  "expected_agent": "change-risk-reviewer", // optional specialist routing target
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
| `agent_selected` | 0.30 | (specialist scenarios) expected bundled agent was invoked |

Specialist scenarios use `agent_selected` as a binary routing gate: the overall
score is `1` only when the expected specialist is invoked, otherwise `0`.
Generic efficiency or a plausible direct answer cannot compensate for missing
the specialist.

Aggregated per host into `scoreboard.json` (mean score plus each rate) and a
compact `report.md`.

## Channel attribution — which channel drove adoption

Passing `first_tool_choice` tells you the agent adopted a tracedecay tool. It
does **not** tell you *why*. The grader attributes a discovery **channel** to
each transcript by inspecting everything that happened **before the first
tracedecay call**:

| channel | attributed when… |
|---------|------------------|
| `hint-driven` | a hook-injected tool hint appears before the first tracedecay call. Matched on distinctive hint phrasing mirrored from `src/hooks/tool_hints.rs` `CATEGORY_SPECS` (e.g. "route by what you're matching", "before reading whole files, consider…"), not the bare word "tracedecay" — so the system tool listing never false-positives. |
| `skill-driven` | a `tracedecay:*` skill invocation (a `Skill` tool call) precedes the first tracedecay call. |
| `steering-or-description` | nothing fired before the call: the session-start CLAUDE.md steering block or the MCP tool descriptions are the only prior mention that could have driven it. |
| `unprompted` | the `bare` ablation adopted a tracedecay tool with hints + skills + steering all removed — pure tool-description pull. |
| `cli-only` | the supported `tracedecay tool ...` shell fallback was used with plugin MCP configuration removed. |
| `none` | no tracedecay tool fired at all. |

The run's ablation `condition` gates channels that were disabled: a stray hint
signature under `no-hints` is **not** credited to hints. Per-scenario `channel`
lands in `scoreboard.json`; the aggregate `report.md` gains a **Channel
efficacy** table (count + mean score per channel per host) and a per-host
`adoption_rate`.

### Hint-signature drift guard

`hint-driven` attribution depends on `HINT_SIGNATURES` in `grade.py` staying in
sync with the hook messages in `src/hooks/tool_hints.rs` `CATEGORY_SPECS`. That
mirror is load-bearing and silent when it breaks: if the source wording drifts
and a signature stops matching, the hint text still fires in live transcripts
but the grader no longer recognizes it, so genuinely **hint-driven** adoptions
get misfiled as **steering-or-description** and the channel-efficacy table lies
without any error.

`grade.py --check-hints` enforces the mirror: it locates `tool_hints.rs`, lowers
it, and fails (non-zero, listing each offender) if any signature is no longer a
substring. `run.sh` runs it right after the neutrality lint — **before building
fixtures or spending a token** — so a drifted signature aborts the run. When the
harness is run from a published package without the Rust source tree, the check
skips cleanly (exit 0). `selftest.py` covers both the pure drift logic and the
live check against the real source, so `make`-free CI catches drift offline.
When a hook message changes, update `HINT_SIGNATURES` and re-run `selftest.py`.

## Ablation matrix — isolating each channel

`CHANNELS` runs the same scenarios under different discovery conditions so you
can measure each channel in isolation. **Ablations multiply live runs, so the
default is `full` only; opt in explicitly.**

| condition | hooks (hints) | skills | steering | MCP descriptions | mechanism |
|-----------|---------------|--------|----------|------------------|-----------|
| `full`    | on  | on  | candidate plugin | on | hermetic candidate plugin/profile; no ambient host assets |
| `no-hints`| **off** | on | fixed | on | isolates skill/description efficacy |
| `no-skills`| on | **off** | fixed | on | isolates hint/description efficacy |
| `bare`    | **off** | **off** | **none** | on | pure MCP-description / unprompted pull |
| `cli-only`| **off** | on | plugin agent/skill | **off** | supported shell fallback without MCP |

**How the ablations work (Claude host).** A plugin bundles
hooks + skills + MCP together, so cleanly removing *one* channel requires a
hermetic, componentized plugin. For every condition `run.sh`:

* copies `plugin/` into `$work/plugins/<condition>` and strips the ablated part
  (`hooks/*.json` for `no-hints`/`bare`/`cli-only`, `skills/` for
  `no-skills`/`bare`, and MCP manifests for `cli-only`),
  substituting the hook binary path;
* launches `claude` with a throwaway `HOME`, `--setting-sources project,local`
  (drops the ambient user config, global plugin, and user `CLAUDE.md`),
  `--strict-mcp-config --mcp-config <hermetic tracedecay server>` (descriptions
  held constant), `--plugin-dir <the componentized copy>`, and `--add-dir
  <fixture>`;
* for `no-hints`/`no-skills` it re-adds a **fixed** steering line via
  `--append-system-prompt` so steering is constant while one channel varies;
  `bare` gets none.

`full` uses the same candidate plugin source and hermetic profile as the
ablations, retaining every product channel. Comparisons therefore measure the
candidate under test, never whichever release happens to be installed globally.

> The ablation flag wiring is implemented to the documented Claude Code 2.1.x
> flag semantics (`--setting-sources`, `--strict-mcp-config`, `--plugin-dir`,
> `--append-system-prompt`). Confirm the isolation against one live ablation run
> — eyeball that a `no-hints` transcript carries no hint text and a `no-skills`
> transcript exposes no `tracedecay:*` skills — before trusting ablation
> aggregates.

**Codex.** Ablation conditions are Claude-only. Codex runs only `full`: the
candidate binary installs its plugin, agents, skills, hooks, and MCP bootstrap
into a throwaway `HOME`/`CODEX_HOME`, with a read-only copy of `auth.json`.
Other conditions are skipped with a notice.

Example:

```sh
# Full channel sweep on a 3-scenario smoke, Claude only:
TRACEDECAY_AGENT_EVALS=1 HOSTS=claude \
  CHANNELS="full no-hints no-skills bare" \
  SCENARIOS="explore_reserve_stock recall_discount_decision dedupe_pricing" \
  evals/agent_adoption/run.sh
```

### Transcript normalizer

`grade.py` auto-detects and normalizes both formats to one event model
(ordered tool calls + final answer):

* **Claude** `--output-format stream-json`: `assistant` events carry
  `tool_use` items; the `result` event carries the final answer.
* **Codex 0.144+** `codex exec --json`: `item.started` / `item.completed`
  envelopes normalize `command_execution`, `mcp_tool_call`,
  `collab_tool_call` (`spawn_agent`), and `agent_message`. Lifecycle pairs are
  deduplicated by `item.id`. Legacy `msg.type` events remain accepted.

Host-specific MCP tool names collapse to canonical form, e.g.
`mcp__plugin_tracedecay_graph__tracedecay_context` → `tracedecay_context`.

Successful exits that contain neither a parsed tool call nor a final answer are
recorded as `unparseable_empty` invalid runs and excluded from aggregates.

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

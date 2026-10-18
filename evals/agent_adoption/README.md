# Agent-adoption evals

This harness launches real headless Claude Code and Codex agents against a
small indexed fixture and records four independent measures per run: task
correctness, routing/adoption, tool success, and efficiency (tool budget, cost,
latency). Native reads can be the appropriate first action; invocation rate
alone is not a measure of usefulness, and no single blended number is published
as effectiveness. Scenario-declared checks decide correctness; transcript review
distinguishes helpful graph evidence from unnecessary calls.

## Layout

```
evals/agent_adoption/
  scenarios/*.json     # labeled scenarios (neutral prompts, see "Doctrine")
  fixture/             # the orders_fixture crate (copied to a temp dir per run)
  fixture_broken/      # orders.rs with a planted type error (diagnostics scenario)
  run.sh               # runner: build+index fixtures, seed facts, drive agents, grade
  grade.py             # deterministic grader + normalizer + neutrality lint + channels + hint-drift guard
  selftest.py          # offline grader self-tests (no agents, no tokens)
  learned_skills/<workflow>/  # generation sessions, manual skill, generated skill (see below)
  generate_learned_skill.sh   # runs the production skill writer on a workflow's sessions
  README.md
```

## Doctrine: prompts stay neutral, adoption is earned

**Scenario prompts must never name `tracedecay`, `mcp`, a specific tool, or a
skill.** They are neutral, natural task prompts ("How does stock reservation
work?"). Whether an agent reaches for a tracedecay tool must be *earned* by the
discovery machinery under test, the MCP tool descriptions, the plugin
description, skill triggering, and the hint engine, not begged for in the
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
Live runs are not wired into CI. The offline grader self-tests are:
`bazel test //evals/agent_adoption:selftest` (also run by the
`hermetic-evals` workflow), or `evals/agent_adoption/selftest.py` directly.

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
| `CODEX_MODELS` | `gpt-5.6-sol` | space-separated `codex exec -m` matrix |
| `CODEX_REASONING_EFFORT` | `medium` | Codex reasoning effort passed through `model_reasoning_effort` |
| `SCENARIO_TIMEOUT` | `240` | per scenario wall-clock seconds |
| `REPS` | `1` | repetitions per scenario x host x model x condition cell; `>1` suffixes transcripts with `__r<N>` and the grader pools the repetitions, so a cell of n=15 gives a usable exact-binomial interval instead of a single draw |
| `PARALLEL` | `1` | concurrent live agent runs; scenarios are read-only against shared fixtures and each `claude` process spawns its own MCP server, so `4` is a safe default for high-rep cells |
| `REPEATED_QUERY_HINTS` | `off` | `on` turns on the profile setting `user.repeated_query_hints.v1` for the eval's isolated profile before any agent launches, and suffixes transcripts with `__qhints`; see "Repeated-query hints on vs off" |
| `REAL_CLAUDE_DIR` | `$CLAUDE_CONFIG_DIR` or `~/.claude` | where the Claude credential comes from: its `.claude_code_oauth_token` and `settings.json` `env`/`model` keys; see "Store isolation & auth" |
| `TRACEDECAY_BIN` | candidate branch (live) | explicit tracedecay binary override |
| `EVAL_OUT` | unset | dir to copy scoreboard + report into |
| `LEARNED_SKILL_WORKFLOW` | unset | run only that workflow's held-out scenarios, once per variant, on `full` |
| `LEARNED_SKILL_VARIANTS` | `none generated manual` | learned-skill variants to compare |

### Cost expectations

One scenario x host x model is a single short headless agent turn. There are 44
active scenarios, so keep smoke runs to 2-3 scenarios before starting the full
Opus/Sonnet and Sol matrix. `--max-turns`
does not exist in the installed Claude Code (2.1.x), so the only hard guardrail
is `SCENARIO_TIMEOUT`; the tool budget is scored, not enforced.

## Store isolation & auth

The runner points `TRACEDECAY_DATA_DIR` at a throwaway dir and sets
`TRACEDECAY_DISABLE_GLOBAL_DB=1`, so the fixture graph and seeded facts never
touch your real tracedecay store, and `tracedecay init` stays fast (indexing the
multi-thousand-node global DB is what makes a naive `init` hang). Each host gets
a throwaway `HOME` and config directory. Codex gets a copy of `auth.json`, mode
`0400`, deleted on runner exit. Claude never gets a copy of `.credentials.json`:
its refresh token is shared with the real login, so a refresh inside an eval
would sign the real profile out. Claude runs authenticate with
`CLAUDE_CODE_OAUTH_TOKEN`, or with a `claude setup-token` grant saved as
`.claude_code_oauth_token` in the auth source directory (`REAL_CLAUDE_DIR`,
default `~/.claude`). The runner exits 5 before building fixtures when no such
credential or endpoint key exists. The `env` and `model` keys of the
real `settings.json` travel too, because a profile aimed at a non-Anthropic
endpoint authenticates through `ANTHROPIC_BASE_URL` / `ANTHROPIC_API_KEY`
there; permissions, plugins, hooks, and every other key stay behind. The real
Claude/Codex profile is never loaded or mutated.

Codex starts plugin MCP servers with a minimal environment, so the runner adds
the isolated daemon's `TRACEDECAY_*` variables to the installed plugin's
`env_vars`. Without them `serve` looks for a daemon under the throwaway `HOME`,
fails, and Codex drops the graph server without any transcript error. Name the model exactly as the profile does
(`CLAUDE_MODELS='k3[1m]'`, not `opus`) so transcripts and scoreboards carry the
model that actually answered; an alias resolved through
`ANTHROPIC_DEFAULT_*_MODEL` would be mislabelled.

Live runs build a debug `tracedecay` binary from the checked-out candidate
branch with ordinary Cargo and use it for fixture setup, MCP, hooks, and the
throwaway Codex plugin install.
`TRACEDECAY_BIN` pins a previously built candidate or release binary and avoids
the runner build path. The daemon also receives a disposable HOME, XDG paths,
host-config paths, temporary directory, and working directory. Only explicit
read-only authentication sources are carried across that boundary. Every
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
  history. `init` on the default branch, a `feature/pricing-notes` branch with
  two commits, a divergent commit on the default branch, and a `--no-ff` merge.
  This gives `commit_context` / `diff_context` / branch tooling genuine commits
  and a merge to reason about. **Note:** tracedecay only *tracks* the checked-out
  branch by default, so `branch_list`/`branch_diff` see one tracked branch unless
  you add multi-branch indexing; `commit_context` does surface the full commit
  log and the merge.
* **Audit-tier plants (`src/audit.rs`).** A module kept **out** of the order
  flow (so exploration/impact ground truth is untouched) plants a `TODO`
  marker and a needless `unsafe { }` block.
  * The `TODO` is reliably surfaced by `tracedecay_todos` (two matches).
  * Both plants have working tool coverage:
    * `tracedecay_unsafe_patterns` surfaces the needless `unsafe { }` block
      (dedicated `risky_patterns_md` renderer). Anchored by
      `analysis_unsafe_panic_audit`.
    * `tracedecay_todos` surfaces the `TODO`. Anchored by `diag_todo_inventory`.

### Seeding prior sessions: a gap

Scenarios like `session_recovery` want prior host sessions bound to the fixture
path (for `message_search` / `sessions_for`). The `tracedecay sessions import`
CLI **sweeps provider directories under `HOME`** (`~/.claude/projects/...`,
`~/.codex/...`); it has **no file-input mode**. Seeding a hermetic session would
mean writing a synthetic transcript into the operator's real `~/.claude` (which
the harness deliberately leaves untouched for auth) at the exact cwd-encoded
path, then ingesting, not hermetic and not cheap. So session-recovery packs stay
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
  "outcome": {                        // what a correct run establishes; see "Correctness"
    "kind": "answer",
    "answer": {
      "required":  [{"id": "states reserve_stock", "pattern": "\\breserve_stock\\b"}],
      "forbidden": []
    }
  },
  "max_tool_calls": 6,                // efficiency budget
  "expected_agent": "change-risk-reviewer", // optional specialist routing target
  "seeded_fact": "currency_fact_id",  // (optional) key in seeded_facts.json
  "grade_feedback": true              // (optional) also score fact_feedback behavior
}
```

### Correctness (the `outcome` block)

Every scenario declares an `outcome`; `grade.py --lint-only` (and the run
itself) fails on a missing or malformed one, and on the retired bare
`ground_truth` substring list. Correctness is graded from the scenario's own
checks and is independent of which tools the agent chose: a correct answer
obtained with native tools passes, and a TraceDecay call cannot make a wrong
answer pass.

| `kind` | what is checked | evidence |
|--------|-----------------|----------|
| `answer` | the final answer states every `required` conclusion and none of the `forbidden` ones | regex per conclusion (case-insensitive) |
| `effect` | the resulting state. `files` maps a glob over the post-run `src/**/*.rs` snapshot to required/forbidden checks; `tool_effects` requires a named tool to have executed successfully with matching input | file text; tool result |
| `ungraded` | nothing: no verifiable ground truth (run-dependent output, deferred fixture gap, or a routing-only specialist scenario). It carries a `reason` | none |

A check is `{"id": "<conclusion>", "pattern": "<regex>"}`. The score is the
fraction of required checks met, forced to `0` when any forbidden check
matches; the status is `pass` only at `1.0`. Declare a `forbidden` check for
the plausible opposite conclusion (for example "do not run `pricing_test`" in
the affected-tests scenarios), because a required keyword alone is satisfied by
an answer that recommends the opposite. Regexes are inspectable evidence, not
semantic judgment: a wrong answer phrased unexpectedly can still satisfy one,
so review transcripts of surprising passes.

`effect` scenarios (`edit_*`, `mem_record_new_decision`) are graded on what
changed, not on what the agent says. `run.sh` snapshots `src/**/*.rs` to
`<transcript>.effects.json` right after each such run and restores `src/` from
git, running them alone because the fixture is shared. Without a snapshot
(a dry run, or an older run directory) the status is `unverified`, which is
excluded from the pass rate. The file checks are structural; nothing compiles
the edited crate.

### Measures (per run, reported separately)

| field | meaning |
|-------|---------|
| `correctness` | `status` (`pass`/`fail`/`ungraded`/`unverified`), `score`, `required_missing`, `forbidden_hit`, `evidence` |
| `adoption_composite` | **routing only**, not effectiveness: weighted `first_tool_choice` 0.30, `not_forbidden_first` 0.25, `feedback` 0.30 (feedback scenarios), `agent_selected` 0.30 (specialist scenarios), renormalized to 0..1. Specialist scenarios use `agent_selected` alone |
| `subscores` | the routing parts above. `first_tool_choice`: first meaningful tool is one of `required_first`. `not_forbidden_first`: no scenario-forbidden tool before the first TraceDecay tool (legacy preference, not a universal native-tool ban). `feedback`: `tracedecay_fact_feedback` called `helpful` on the seeded fact **and the call succeeded** |
| `tool_success` | attempted / succeeded / failed / unconfirmed meaningful calls, overall and for TraceDecay tools. A failed call is an attempt (it still counts as adoption) but never a success; `unconfirmed` means the host reported no result |
| `efficiency` | `within_tool_budget` (call count <= `max_tool_calls`, and <= `max_tracedecay_calls` when declared). Spend and latency (tokens, USD, turns, duration) are in `cost` and aggregated with the pass rate, including `$/pass` (total spend over verified passes) |

Aggregated per host into `scoreboard.json` and a compact `report.md` with
separate **Task correctness**, **Routing and adoption (not effectiveness)**, and
**Tool success and efficiency** tables. Success is judged from the exact run's
own transcript and snapshot.

## Channel attribution: which channel drove adoption

Passing `first_tool_choice` tells you the agent adopted a tracedecay tool. It
does **not** tell you *why*. The grader attributes a discovery **channel** to
each transcript by inspecting everything that happened **before the first
tracedecay call**:

| channel | attributed when… |
|---------|------------------|
| `hint-driven` | a hook-injected tool hint appears before the first tracedecay call. Matched on distinctive hint phrasing mirrored from `crates/tracedecay-policy/src/intent_catalog.rs` `INTENT_CATALOG` (e.g. "route by what you're matching", "before reading whole files, consider…"), not the bare word "tracedecay", so the system tool listing never false-positives. |
| `skill-driven` | a `tracedecay:*` skill invocation (a `Skill` tool call) precedes the first tracedecay call. |
| `steering-or-description` | nothing fired before the call: the session-start CLAUDE.md steering block or the MCP tool descriptions are the only prior mention that could have driven it. |
| `unprompted` | the `bare` ablation adopted a tracedecay tool with hints + skills + steering all removed, pure tool-description pull. |
| `cli-only` | the supported `tracedecay tool ...` shell fallback was used with plugin MCP configuration removed. |
| `none` | no tracedecay tool fired at all. |

The run's ablation `condition` gates channels that were disabled: a stray hint
signature under `no-hints` is **not** credited to hints. Per-scenario `channel`
lands in `scoreboard.json`; the aggregate `report.md` gains a **Channel
efficacy** table (count, adoption composite, and correctness pass rate per
channel per host) and a per-host
`adoption_rate`.

### Hint-signature drift guard

`hint-driven` attribution depends on `HINT_SIGNATURES` in `grade.py` staying in
sync with the hook messages in `crates/tracedecay-policy/src/intent_catalog.rs` `INTENT_CATALOG`. That
mirror is load-bearing and silent when it breaks: if the source wording drifts
and a signature stops matching, the hint text still fires in live transcripts
but the grader no longer recognizes it, so genuinely **hint-driven** adoptions
get misfiled as **steering-or-description** and the channel-efficacy table lies
without any error.

`grade.py --check-hints` enforces the mirror: it locates `intent_catalog.rs`, lowers
it, and fails (non-zero, listing each offender) if any signature is no longer a
substring. `run.sh` runs it right after the neutrality lint. **before building
fixtures or spending a token**, so a drifted signature aborts the run. When the
harness is run from a published package without the Rust source tree, the check
skips cleanly (exit 0). `selftest.py` covers both the pure drift logic and the
live check against the real source, so `make`-free CI catches drift offline.
When a hook message changes, update `HINT_SIGNATURES` and re-run `selftest.py`.

## Ablation matrix: isolating each channel

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
  (`hooks/*.json` for `no-hints`/`bare`/`cli-only`, `skills/` plus
  `commands/` for `no-skills`/`bare`. Claude exposes plugin commands as
  `tracedecay:*` skills too, and MCP manifests for `cli-only`),
  substituting the hook binary path;
* launches `claude` with a throwaway `HOME` whose user settings hold only the
  carried endpoint/auth env (the ambient user config, global plugin, and user
  `CLAUDE.md` are unreachable), `--setting-sources user,project,local`,
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
>, eyeball that a `no-hints` transcript carries no hint text and a `no-skills`
> transcript exposes no `tracedecay:*` skills, before trusting ablation
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

## Repeated-query hints on vs off

The optional repeated-query hint (off by default) appends one result block
starting `tracedecay_repeated_query:` when a call nearly repeats a recent call
to the same tool. With `REPEATED_QUERY_HINTS=on`, `run.sh` sets
`user.repeated_query_hints.v1` through `tracedecay tool configuration_set` on
the isolated profile, reads it back, and aborts (exit 4) if either step fails,
for example on a binary that does not register the key. Run and record both
settings on the same scenarios:

```sh
pack="$(cd evals/agent_adoption/scenarios && grep -l repeated_query_class *.json | sed 's/.json$//' | tr '\n' ' ')"
TRACEDECAY_AGENT_EVALS=1 HOSTS=claude CLAUDE_MODELS=sonnet REPS=3 PARALLEL=4 \
  SCENARIOS="$pack" REPEATED_QUERY_HINTS=on evals/agent_adoption/run.sh
# then the same command with REPEATED_QUERY_HINTS=off
```

Scenarios in this pack carry `repeated_query_class`, the situation the
hint must handle:

| class | scenario | the hint should |
|---|---|---|
| `stuck_loop` | `loop_absent_stock_release`, `loop_retry_absent_release` | fire on a near repeat and help the agent stop |
| `narrowing` | `narrow_availability_call_chain`, `narrow_inventory_bodies` | not fire |
| `pagination` | `page_every_function` | not fire |
| `changed_scope` | `scope_branch_pricing_notes` | not fire |
| `repeated_verification` | `verify_callers_recheck` | fire only harmlessly: the prompt asks for an identical re-check |

The `__qhints` suffix keeps the two runs' transcripts apart, so one grade
covers both:

```sh
merged="$(mktemp -d)"
cp <off-run-dir>/*__*.stdout.jsonl <off-run-dir>/*__*.meta.json \
   <on-run-dir>/*__*.stdout.jsonl <on-run-dir>/*__*.meta.json "$merged"
python3 evals/agent_adoption/grade.py --run-dir "$merged"
```

Each scoreboard result carries `repeated_query_hints`, `repeated_query_class`,
`cost` (Claude `total_cost_usd`, `num_turns`, and input/output/cache tokens;
Codex tokens only; unreported fields stay `null`), and
`repeated_query_hint_next_steps`: one entry per hint block, naming the next
tool call or `answer`. `cost_and_hints` and the report's **Correctness, cost, latency,
and repeated-query hints** table put the `hints-on` and `hints-off` cells of each
host/model and condition side by side; `repeated_query_scenarios` and its
**Per repeated-query scenario** table split them per scenario. `completed` is
the share of runs that ended with an answer. Read the correctness pass rate first, then cost and
turns. A hint helped when hints-on keeps or raises the pass rate at lower cost on a
`stuck_loop` scenario; on a class where it should not fire, any hint is a
false positive, and a lower pass rate or extra cost there is harm. `after hint`
shows whether agents changed course (a different tool or an answer) or
repeated the same tool. At `REPS=3`, read differences as direction, not
significance; the pass rate rests on regex checks, so review the final
answers before claiming either way.

Both hosts deliver the hint through `tracedecay serve`. Claude runs `serve`
from the harness MCP config; Codex runs it as the plugin's `graph` server.

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

## Task routing and unnecessary calls

Scenarios may declare `expected_skill` and `allowed_skills`. Omitted allowed
skills defaults to the expected skill; explicit `[]` marks a no-skill case.
Claude exposes plugin `commands/` as `tracedecay:*` skills alongside `skills/`,
so a scenario whose task is exactly one workflow command's job (for example
`draft-commit` for a commit message) lists that command in `allowed_skills`;
routing to it is a correct workflow, not a false trigger.

Claude Code (2.1.x) defers MCP tool schemas behind `ToolSearch` once a server's
definitions exceed its context budget; only tools the server marks
`anthropic/alwaysLoad` are in the prompt from turn one. For TraceDecay that is
the always-loaded set in `crates/tracedecay-mcp-catalog/src/definitions.rs`
(`ALWAYS_LOADED_TOOL_NAMES`). Every other graph tool, such as `impact`,
`health`, `redundancy`, `affected_tests`, or `rename_preview`, is a name the
model must load first. Read `bare`/`no-skills` adoption with
that in mind: "pure MCP-description pull" only applies to the always-loaded
core, and a `ToolSearch` call in a transcript marks the model discovering a
deferred tool, not an unnecessary detour.
`max_tracedecay_calls` bounds graph/CLI calls independently of `max_tool_calls`.
The grader reports per-skill true positives, misses, false positives, and no-skill
over-trigger counts. Explicit host invocation is evidence; prose naming a skill
is not. Skill routing does not replace or increase task correctness.

Neighbor scenarios distinguish exploration, tracing, impact, review, structural
editing, and health. Negative cases cover known files, supplied documentation,
local comment suggestions, and clarification. These are routing checks, not proof
of an applied edit. Ground-truth fragments are a bounded answer check and still
need transcript review; plausible prose is not evidence of persisted effects.

Compare full, no-skills, and bare conditions on the same cases. Claude supports
these existing conditions; the runner currently skips non-full Codex conditions.
Adoption rate alone is descriptive, not a success target. Retained legacy
first-tool preferences are scenario-specific; native reads are not intrinsically
failures. Review outcomes, first useful action, unnecessary calls, and routing
errors together. Offline self-tests verify grading only, not live skill quality.

Skill-routing counts cover Claude's Skill tool calls and Codex shell reads of an
installed TraceDecay `skills/<id>/SKILL.md`. A skill load is routing, so it does
not count as the first tool or toward the tool budget.

The evaluator uses its own isolation re-entry flag. An existing general daemon
harness does not bypass evaluator HOME/XDG or working-directory isolation; the
evaluator creates its own daemon profile before launching host processes.

## Evaluating writer-generated routing cases

The same runner accepts `EVAL_SCENARIOS_DIR=/absolute/path/to/cases` (default:
checked-in `scenarios`). Read a writer's advertised artifact through the supported
verified artifact view. Export its `proposed_ops.skills[*].routing_validation`
objects, or the original output's `skills[*].routing_validation`, as one scenario
object per file in a fresh directory. Use fixed-index filenames such as
`case-0001.json`; never derive filesystem paths from generated ids. Keep each
case's neutral prompt, expected/allowed skills, outcome checks, and call budgets
unchanged. Scenario ids must be unique safe basename identifiers because they
identify transcript outputs.

Run `python3 evals/agent_adoption/grade.py --lint-only --scenarios /absolute/path/to/cases`
before scheduling the existing runner with that directory and a pinned candidate
binary. Use bounded host/model/condition selections. Validation of proposed
cases is not a live routing result; retain the resulting transcripts and task
outcomes before claiming effectiveness.

## Learned-skill qualification

This mode asks whether a skill learned from repeated corrections helps on new
prompts. Each workflow pack under `learned_skills/<workflow>/` holds:

* `sessions/*.jsonl`, synthetic Claude Code sessions in which the user corrects
  the agent the same way several times. They are the skill writer's evidence.
* `manual/<slug>/SKILL.md`, a hand-written skill for the same workflow.
* `generated/`, the skill writer's output: `skill.json`, `<slug>/SKILL.md`, and
  `PROVENANCE.json` (run id, writer prompt version, backend, model, date,
  validation decisions, artifact kinds). It exists only after a real generation
  run. Never write it by hand.

Held-out scenarios carry `"status": "learned"` and
`"learned_skill": {"workflow": ..., "expected": true|false}`. The default run
skips them. The lint fails when a held-out prompt overlaps a generation session
prompt, so the evaluation never scores a skill on its own evidence.

Generate the skill once, then compare the variants:

```sh
# Dry run: configures automation (left disabled), imports the sessions, prints the plan.
TRACEDECAY_BIN=<binary> evals/agent_adoption/generate_learned_skill.sh pricing_change_review
# Live: one codex writer turn, plus at most one repair turn.
TRACEDECAY_AGENT_EVALS=1 TRACEDECAY_BIN=<binary> \
  evals/agent_adoption/generate_learned_skill.sh pricing_change_review

TRACEDECAY_AGENT_EVALS=1 TRACEDECAY_BIN=<binary> LEARNED_SKILL_WORKFLOW=pricing_change_review \
  CLAUDE_MODELS=opus REPS=5 PARALLEL=4 evals/agent_adoption/run.sh
```

Generation reuses `run.sh`'s isolated daemon. It binds the codex executable
through `configuration_set`, enables only the skill writer, and copies codex
`auth.json` read-only into the throwaway profile. Automation is enabled only
after the writer's evidence query finds the imported sessions. The query needs
all of its terms in one message, which is why each session ends with the user
naming the repeated correction pattern.

The variant run makes `scenarios x variants x CLAUDE_MODELS x REPS` Claude
runs. `none` installs nothing. `generated` and `manual` install their
`SKILL.md` into a per-variant `$HOME/.claude/skills/<slug>/`, the directory
production materialization writes to. A variant without a `SKILL.md` is skipped
with a notice. Transcripts end in `__v-<variant>`.

The report's learned-skill table gives, per variant and model:

* `correctness`, the mean score of the scenario's outcome checks. It does not
  depend on the skill or on the agent's claims.
* Routing as tp/fn/fp/tn. A hit is a Skill call naming the variant's slug. A
  `none` variant, or a host without normalized Skill events (Codex), counts as
  `unknown` and stays out of those four counts. Invocation is a proxy for
  adoption, not proof the skill helped.
* Tokens, USD, and duration as the host reported them. Means exclude a value
  the host did not report, and the `tokens unknown` column counts those runs.
  `scoreboard.json` also keeps per-run turns and cache token splits.
* Tool calls, corrections (tool errors plus repeated identical calls inside one
  headless turn, since no human turns exist), and review cost (final-answer
  words).

The table ranks nothing and prints no verdict. Read it with `n` and the invalid
count beside every rate.

Both live steps need current host credentials: a Claude setup-token (see
"Store isolation & auth") and a Codex `auth.json` whose login is still valid.
A Codex copy whose access token has expired and whose refresh token was
already used fails every run with `401`; those runs land in the invalid count,
not in the correctness columns. Generation stops with the backend's error when the
codex account is over its usage limit.

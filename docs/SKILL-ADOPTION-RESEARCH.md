# Skill & Tool Adoption Research: Why superpowers gets invoked and TraceDecay doesn't

*Research date: 2026-07-02. Read-only study of the local superpowers plugin
(`~/.cursor/plugins/cache/cursor-public/superpowers/b7a8f76…/`), the installed
TraceDecay plugins (`~/.cursor/plugins/local/tracedecay/`, `~/.codex/…`), the
repo sources (`cursor-plugin/`, `codex-plugin/`, `src/hooks/`,
`src/mcp/tools/definitions.rs`), and TraceDecay's own analytics
(`http://127.0.0.1:7341/api/plugins/analytics/*`,
`~/.tracedecay/projects/proj_<local-id>/hook_analytics.jsonl`).*

> **Status (2026-07-03):** the P1 catalog consolidation recommended in §6 --
> including the new `using-tracedecay` skill — was implemented by
> [PR #225](https://github.com/ScriptedAlchemy/tracedecay/pull/225) the day
> after this snapshot. Skill names, directory counts, and the
> `memorize-subject` duplication described below refer to the
> pre-restructure catalog.

## TL;DR

Superpowers gets reliably invoked because it **injects a mandatory behavioral
contract at session start** (`EXTREMELY_IMPORTANT`, "you ABSOLUTELY MUST"), gives
the model a **decision procedure** (a flowchart plus a "red flags" table that
pre-empts every rationalization for skipping skills), and keeps its skill
catalog **small and process-shaped** (14 skills, each owning one workflow
stage). TraceDecay is weaker on each of those surfaces: its session-start
injection is deliberately lean status text, its rule says "prefer X first", its
25 model-invocable skills overlap heavily, and its runtime hint system — the one
mechanism that could nudge a model mid-session — is mostly inactive:
**984 hint candidates in the last week, 979 suppressed by dedupe, 6 emitted**,
and none of that lifecycle ever reaches the dashboard, which reports zeros.

The measured result: 2,865 MCP tool calls concentrate on file-reading tools
(45% are `tracedecay_read` + `tracedecay_body`), the differentiating graph
tools are nearly untouched (callers 16, impact 7, callees 6; 17 tools called
exactly once), and the underused-family analysis counts **2,513 missed
code-search opportunities and 706 missed code-context opportunities with zero
correlated usage**.

---

## 1. What superpowers does, mechanically

### 1.1 Session-start injection of a behavioral contract

`hooks/session-start` (a bash script wired to `sessionStart` in
`hooks-cursor.json` and `SessionStart` in `hooks.json`) reads the **entire**
`using-superpowers` skill and injects it into context wrapped like this:

> `<EXTREMELY_IMPORTANT>\nYou have superpowers.\n\n**Below is the full content
> of your 'superpowers:using-superpowers' skill - your introduction to using
> skills. …**`

So every session begins with the full bootstrap skill already in context — the
model never has to *decide* to read it. Contrast: TraceDecay's Cursor
session-start context is (by design, see the doc comment on
`build_cursor_session_context` in `src/hooks/steering.rs`) "intentionally
lean":

> `tracedecay index status: initialized.\nWorkflow skills: tracedecay:architecture-overview, …`
> *(a comma-separated list of 25 names, no instructions)*

### 1.2 Mandatory, non-negotiable framing

The injected `using-superpowers/SKILL.md` opens with:

> "If you think there is even a **1% chance** a skill might apply to what you
> are doing, you **ABSOLUTELY MUST** invoke the skill.
>
> IF A SKILL APPLIES TO YOUR TASK, YOU DO NOT HAVE A CHOICE. YOU MUST USE IT.
>
> This is not negotiable. This is not optional. **You cannot rationalize your
> way out of this.**"

and later: "**Invoke relevant or requested skills BEFORE any response or
action.**" — including before clarifying questions. TraceDecay's always-applied
rule (`cursor-plugin/rules/tracedecay.mdc`) uses preference language
throughout: "use the tracedecay MCP tools *first*", "Full-file Read is the
fallback, not the default", "*Prefer* tracedecay MCP tools". Preference
language gives the model permission to weigh alternatives; mandate language
removes the choice.

### 1.3 A decision procedure, not just descriptions

`using-superpowers` embeds a **graphviz flowchart** of the exact loop the model
must run on every user message: *message received → might any skill apply?
(yes, even 1%) → invoke Skill tool → announce "Using [skill] to [purpose]" →
has checklist? → create a TodoWrite todo per item → follow skill exactly →
only then respond*. Two details do heavy lifting:

- **Announcement step**: forcing the model to say "Using X to Y" makes skill
  use externally observable and self-committing.
- **TodoWrite integration**: skill checklists become todos, so the skill's
  process survives long contexts instead of being read once and forgotten.

### 1.4 The red-flags rationalization table

The red-flags table lists the model's *own escape thoughts* and rebuts each
one:

> | Thought | Reality |
> |---------|---------|
> | "This is just a simple question" | Questions are tasks. Check for skills. |
> | "Let me explore the codebase first" | Skills tell you HOW to explore. Check first. |
> | "The skill is overkill" | Simple things become complex. Use it. |
> | "I'll just do this one thing first" | Check BEFORE doing anything. |
> | "I know what that means" | Knowing the concept ≠ using the skill. Invoke it. |

Every common rationalization for skipping a skill has a pre-written counter
already in context. TraceDecay has no equivalent; a model thinking "grep is
faster here" meets no resistance.

### 1.5 Skill priority ordering and a small, staged catalog

Superpowers ships **14 skills**, each owning one distinct stage of a
development process (brainstorming → writing-plans → executing-plans /
subagent-driven-development → TDD → systematic-debugging →
verification-before-completion → finishing-a-development-branch …), plus an
explicit tie-breaker: "**Process skills first** (brainstorming, debugging) …
**Implementation skills second**". Descriptions are pure trigger phrases tied
to *moments in the workflow*, not capabilities:

> - `systematic-debugging`: "Use when encountering any bug, test failure, or
>   unexpected behavior, **before proposing fixes**"
> - `test-driven-development`: "Use when implementing any feature or bugfix,
>   **before writing implementation code**"
> - `brainstorming`: "**You MUST use this** before any creative work…"
> - `verification-before-completion`: "Use when **about to claim work is
>   complete**… evidence before assertions always"

Each trigger names a *recognizable moment* ("about to claim work is complete")
rather than a topic. The codex skills observed at `~/.codex/skills` (babysit,
deslop, code-simplifier, split-to-prs) follow the same recipe: one-line
capability summary + "Use when the user asks to …" trigger list.

### 1.6 External confirmation

The GitHub README (github.com/obra/superpowers) is explicit that the framing is
the product: "a set of composable skills **and some initial instructions that
make sure your agent uses them**" and "The agent checks for relevant skills
before any task. **Mandatory workflows, not suggestions.**" Third-party
write-ups (e.g. Joey Yi Zhao, "Superpowers Explained", May 2026) independently
identify the same two mechanisms: the SessionStart hook ("Claude forgets to
load them, or the user forgets to invoke them. That is where hooks come in")
and the "very strict, almost bossy style" of the bootstrap skill as a
deliberate design choice preventing ad-hoc work.

---

## 2. How TraceDecay surfaces its tools and skills today

| Layer | Mechanism | Character |
|---|---|---|
| Always-applied rule | `cursor-plugin/rules/tracedecay.mdc` | ~15 bullet points of routing advice; "prefer/first/fallback" phrasing; no decision procedure, no counters to rationalization |
| Session start (Cursor) | Native `sessionStart` → daemon-owned V2 admission | The hook returns only immediate daemon guidance, or an empty fail-open response when unavailable. |
| Session start (Codex) | `build_codex_session_context_for_workspace` in `src/hooks/steering.rs` | One paragraph: "Prefer tracedecay MCP tools … over broad file reads"; CLI fallback note; index status |
| Mid-session host events | Native Cursor lifecycle events → daemon-owned V2 admission | Immediate guidance, if any, is returned by the daemon; unsupported legacy surfaces stay passive. |
| Skills | `cursor-plugin/skills/` (40 dirs installed: 23 model-invocable + 15 `tracedecay-*` slash-command variants with `disable-model-invocation: true` + a `memorize-subject`/`memorizing-subject` near-duplicate pair); `codex-plugin/skills/` (25) | Descriptions are actually good "Use when…" trigger form; the problem is volume and overlap, not phrasing |
| MCP tool descriptions | `src/mcp/tools/definitions.rs` (68+ tools) | Capability descriptions ("Search for symbols … by name or keyword"), not invocation triggers; only `tracedecay_outline` and `tracedecay_context` carry usage steering (the latter a *budget cap*, i.e. anti-usage steering) |

Two structural notes:

- The Cursor `beforeSubmitPrompt` hook **cannot inject context** (per the
  comment at `src/hooks/cursor.rs`, only `user_message` is available on that
  event), so per-prompt re-steering à la Codex's `UserPromptSubmit` is not
  available on Cursor — which makes sessionStart and postToolUse the only
  injection points, and both are currently minimal.
- Skill overlap is real: a model wanting to "find where X is defined and what
  calls it" plausibly matches `searching-for-code`, `tracing-functions`,
  `reading-code-cheaply`, `exploring-types-and-traits`, and
  `finding-impacted-areas`. Superpowers never presents this ambiguity; each
  skill owns a workflow stage.

---

## 3. The measured usage picture

All numbers from the live dashboard (`/api/plugins/analytics/*`) and
`hook_analytics.jsonl` for project `proj_<local-id>`, sampled 2026-07-02.

### 3.1 MCP tool distribution: TraceDecay is used as a file reader

2,865 recorded MCP tool calls across 68 distinct tools:

| Tool | Calls | Share |
|---|---|---|
| `tracedecay_read` | 667 | 23.3% |
| `tracedecay_body` | 627 | 21.9% |
| `tracedecay_search` | 250 | 8.7% |
| `tracedecay_context` | 210 | 7.3% |
| `tracedecay_outline` | 139 | 4.9% |
| top-5 subtotal | 1,893 | 66.1% |

The graph-native tools barely register:
`tracedecay_callers` 16, `tracedecay_impact` 7, `tracedecay_callees` 6,
`tracedecay_implementations` 3, and **17 tools were called exactly once**
(`circular`, `coupling`, `gini`, `dsm`, `doc_coverage`, `field_sites`,
`constructors`, `hotspots`, `health`, `file_dependents`, …). Category split
from `/usage`: 2,617 `tracedecay_mcp`, 164 `lcm_session`, 83 `memory`.

### 3.2 Underused families: thousands of missed events, zero correlated usage

`/api/plugins/analytics/underused`:

| Family | Relevant events | Usage events | Missed |
|---|---|---|---|
| `code_search` | 2,513 | 0 | **2,513** |
| `code_context` | 706 | 0 | **706** |
| `call_graph` | 0 | 0 | 0 |
| `impact_analysis` | 0 | 0 | 0 |

Every grep/read-shaped event the analyzer saw went to native tools instead of
TraceDecay. Caveat: `usage_events` is 0 for *all* families even though the MCP
log shows 210 `tracedecay_context` / 250 `tracedecay_search` calls — the usage
side of the correlation (`infer_usage_events` in `src/analytics.rs`) appears
disconnected from the MCP call log, so the *ratio* is unreliable even though
the missed-opportunity counts are still directionally useful.

### 3.3 The hint system is mostly inactive — and its telemetry is broken

`/api/plugins/analytics/hints` reports **zero** emitted/followed/ignored/
suppressed across all 10 categories. The raw hook log tells the real story
(2,192 lines, 2026-06-24 → 2026-07-01):

| Event | Count |
|---|---|
| `hint_candidate` | 984 (962 `file_read`, 22 `search`) |
| `suppressed_duplicate` | 979 |
| `hint_emitted` | **6** |
| `hook_invoked` | 24 |

Two independent failures:

1. **Suppression by design.** `ToolHintDedupe::should_emit`
   (`src/hooks/tool_hints.rs:137`) is `self.seen.insert((session_id, category))`
   — one hint per category per session, persisted to disk, *forever*. In
   long-running Cursor sessions doing hundreds of Reads, 99.4% of candidates
   are suppressed. The one mechanism that could catch a model mid-grep fires
   at most once and never again.
2. **Telemetry never reaches the dashboard.** Hooks write lifecycle events
   only to `hook_analytics.jsonl`; the `/hints` endpoint
   (`src/dashboard/analytics_api.rs`) reads durable `analytics_events` (with a
   legacy `dashboard_hint_events` fallback), which only MCP-side code
   (`src/mcp/tool_analytics.rs`) populates. Result: the dashboard reports a
   silent all-zeros hint system, so this failure mode was invisible.

### 3.4 Skills

Plugin-skill (SKILL.md) invocations aren't tracked at all — Cursor loads them
directly, so TraceDecay has no counter for them. The managed skill store
(`tracedecay tool skill_list`) holds 2 skills; the sampled one has
**12 views, 0 uses, 3 patches**, and its own improvement recommendation
already flags "skill has been patched but still has no recorded successful
uses". `tracedecay_skill_list` was called 3 times ever; the Hermes skill
inventory was called once before its input was restricted to `~/.hermes`.

---

## 4. Side-by-side

| Axis | superpowers | TraceDecay |
|---|---|---|
| Session start | Full bootstrap skill injected inside `<EXTREMELY_IMPORTANT>` | Status line + bare skill-name list (Cursor); one "prefer" paragraph (Codex) |
| Enforcement language | "1% chance → you ABSOLUTELY MUST"; "not negotiable"; "before ANY response" | "prefer … first", "fallback, not the default" |
| Anti-rationalization | 12-row red-flags table rebutting the model's escape thoughts | None |
| Decision procedure | Flowchart + priority ordering (process > implementation) + announce step | Per-skill routing bullets; no cross-skill procedure |
| Persistence in long contexts | Checklists become TodoWrite todos | Nothing; hint re-steering deduped to ~once per session |
| Catalog size / shape | 14 skills, one per workflow stage, near-zero overlap | 23–25 model-invocable skills with heavy topical overlap (+15 slash variants, 1 duplicate pair) |
| Tool descriptions | n/a (skills are the product) | 68 tools described by capability, not by when to pick them over native tools |
| Feedback loop | n/a | Exists on paper (hints/underused analytics) but suppressed and disconnected |

---

## 5. Root causes of low adoption

1. **Passive, preference-graded steering.** Every TraceDecay surface says
   "prefer"; models under time/token pressure treat preferences as tie-breakers
   and default to the native tools they were RL-trained on. Superpowers shows
   that mandate framing + rationalization counters materially change this.
2. **The session-start slot is spent on status, not behavior.** The lean
   injection was a deliberate token economy (`src/hooks/steering.rs`), but it
   delegates all steering to a rule that sits among *dozens* of other
   always-applied rules and skill listings, with no priority claim.
3. **The mid-session correction loop is mostly inactive.** Once-per-session-per-category
   dedupe means the 962 file-read candidates produced ~3 nudges; and because
   hint telemetry never reaches the dashboard, the all-zeros table looked like
   "no data" rather than "system down".
4. **Skill sprawl and overlap.** 25 trigger descriptions that mutually overlap
   force the model to adjudicate between skills before it has read any of
   them; the cheapest resolution is to use none. (The individual descriptions
   are fine — most already use "Use when…" form — the *set* is the problem.)
5. **Tool descriptions optimized for capability, not invocation.**
   `tracedecay_search`: "Search for symbols … by name or keyword" describes
   what it does, not when to reach for it *instead of Grep*. Meanwhile the one
   description with strong behavioral language is `tracedecay_context`'s
   **CALL BUDGET cap — steering that suppresses usage.** The read-shaped tools
   (`read`, `body`) have the most "obvious" descriptions, which is exactly
   where usage concentrates.
6. **No usage instrumentation for plugin skills**, so adoption can't even be
   measured, let alone improved, for the 25 SKILL.md workflows.

---

## 6. Recommendations (prioritized)

### P0 — Inject a behavioral contract at session start
*Files: `src/hooks/steering.rs` (`build_cursor_session_context`,
`build_codex_session_context_for_workspace`, `CURSOR_PLUGIN_SKILLS`);
new skill `cursor-plugin/skills/using-tracedecay/SKILL.md` (mirror in
`codex-plugin/skills/`).*

Create a `using-tracedecay` bootstrap skill and inject its full content at
sessionStart, superpowers-style. Proposed injection frame:

```
<EXTREMELY_IMPORTANT>
This project has a live TraceDecay code graph. For ANY codebase question —
finding code, reading code, tracing calls, estimating blast radius — you MUST
try the matching tracedecay tool BEFORE Grep, Glob, codebase search, or file
reads. If there is even a 1% chance a tracedecay skill applies, invoke it.

Red flags — if you think any of these, STOP, you are rationalizing:
| Thought | Reality |
|---|---|
| "Grep is faster for this" | tracedecay_search is one call and pre-ranked. |
| "I'll just read the whole file" | tracedecay_outline / tracedecay_body answer at 1/10 the tokens. |
| "This is a simple lookup" | Simple lookups are exactly what the graph is for. |
| "I already know this codebase" | The graph is fresher than your memory. Check it. |
| "The MCP call might fail" | The CLI fallback (`tracedecay tool …`) always works. |

Index status: {status}. Tokens saved this session: {n}.
</EXTREMELY_IMPORTANT>
```

Keep it under ~40 lines; the token cost is what buys reliability.

### P0 — Resurrect the hint loop
*Files: `src/hooks/tool_hints.rs` (`should_emit`), `src/hooks/mod.rs`
(`deduped_project_hint`), `src/mcp/tool_analytics.rs` +
`src/dashboard/analytics_api.rs` for telemetry.*

1. Replace forever-dedupe with a re-arming policy: allow a category to fire
   again after N ignored native-tool events (e.g. every 15 file reads) or a
   time cooldown, capped per session. Escalate wording on the 2nd/3rd emission.
2. Write `hint_emitted` / `followed` / `ignored` into the durable
   `analytics_events` store the dashboard actually reads, so
   `/api/plugins/analytics/hints` reflects reality and follow-through becomes
   measurable. (Also fix the `usage_events`-always-0 correlation in
   `src/analytics.rs` so the underused-family ratio is meaningful.)

### P1 — Rewrite the always-applied rule from preference to procedure
*File: `cursor-plugin/rules/tracedecay.mdc`.*

Replace "prefer X first" bullets with an explicit mapping of *moments* to
*mandatory actions*, e.g.: "About to call Grep/Glob/codebase-search for
symbols or concepts? → call `tracedecay_search`/`tracedecay_context` first —
not optional in an indexed project." Add a 4–5 row red-flags table. Keep the
CLI-fallback and truncation-handle bullets (those are good).

### P1 — Consolidate the skill catalog to ~10 workflow-stage skills
*Files: `cursor-plugin/skills/`, `codex-plugin/skills/`,
`CURSOR_PLUGIN_SKILLS` in `src/hooks/steering.rs`,
`tests/agent_suite/plugin_skill_contract_test.rs`.*

Merge along workflow stages, mirroring superpowers' shape: **explore**
(searching-for-code + reading-code-cheaply + exploring-types-and-traits),
**trace** (tracing-functions), **assess-impact** (finding-impacted-areas +
assessing-test-coverage + running-impacted-tests), **edit-safely**
(atomic-code-edits + refactoring-safely + finding-duplicate-logic),
**review** (reviewing-a-diff + auditing-code-safety), **health**
(code-health-report + architecture-overview + tracking-session-health),
**memory** (recall + curate), **sessions**, **fix-build**, **using-the-cli**.
Delete the `memorize-subject`/`memorizing-subject` duplicate. Keep the
`tracedecay-*` slash commands as-is (they're user-invoked and don't compete
for model attention).

### P2 — Lead tool descriptions with invocation triggers
*File: `src/mcp/tools/definitions.rs`.*

Prefix the ~10 highest-value tools with a "Use instead of…" sentence, e.g. for
`def_search`: "Use INSTEAD of Grep/codebase-search whenever you're looking for
a function, type, or concept by name — returns ranked symbols in one call."
For `def_callers`/`def_impact`: "Use BEFORE editing or renaming any function…".
Reconsider the tone of `context_description`'s CALL BUDGET block — cap
enforcement can live in the handler response instead of pre-emptively
discouraging calls in the description.

### P2 — Instrument plugin-skill usage
Hook-side detection (the `WorkflowSkill`/`TraceDecayWorkflowSkill` categories
already exist in `src/analytics.rs`) should record when a SKILL.md is read
(Cursor exposes reads via `postToolUse`/`afterFileEdit` events on the skill
paths), giving a baseline to verify that P0/P1 actually move adoption.

---

## Appendix: raw analytics snapshots (2026-07-02)

- `/overview`: 2,865 events, 68 tools; hooks seen: PostToolUse 14,
  UserPromptSubmit 6, SessionStart 1, sessionEnd 1, workspaceOpen 2.
- `/underused`: code_search 2,513/0, code_context 706/0, call_graph 0/0,
  impact_analysis 0/0 (relevant/usage).
- `/hints`: all 10 categories 0/0/0/0 (emitted/followed/ignored/suppressed),
  `source: analytics_events`.
- `hook_analytics.jsonl` (2,192 lines, Jun 24 – Jul 1): hint_candidate 984,
  suppressed_duplicate 979, hint_emitted 6, workspace_status 149,
  codex_subagent_start 50, hook_invoked 24.
- `skill_list`: 2 managed skills; `skill-writer-evidence-validation`:
  views 12, uses 0, patches 3, priority-high patch_review recommendation.

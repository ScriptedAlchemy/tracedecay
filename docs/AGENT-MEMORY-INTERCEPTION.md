# Agent Memory Interception: Codex CLI & Cursor × TraceDecay Fact Store

**Status:** research + design proposal (2026-07-02; plugin paths refreshed
2026-07-03).
**Goal:** make Codex CLI and Cursor use the TraceDecay holographic fact store
(`tracedecay_fact_store` add/search/probe/reason, `memory_facts` table, HRR
vectors + trust scores) as their agent memory for both **recall** (facts reach
the model at the right moment) and **storage** (new durable facts get written),
instead of — or layered on top of — each agent's native memory mechanism.

Current plugin source lives under the shared `plugin/` tree: shared skills in
`plugin/skills/`, Claude commands in `plugin/commands/`, shared agents in
`plugin/agents/`, and Cursor-only surfaces in `plugin/overlays/cursor/`.

---

## 1. How Codex reads memory today

### 1.1 Native memories (experimental, **enabled on this machine**)

`~/.codex/config.toml` has:

```toml
[features]
memories = true    # experimental per `codex features list`
```

Codex's memories feature is a background extract-and-consolidate pipeline that
materializes plain-markdown files under `~/.codex/memories/` (a git repo):

| File | Role |
| --- | --- |
| `memory_summary.md` | Stage-2 consolidated "user profile + preferences + what's in memory" digest — the primary artifact injected into new sessions |
| `MEMORY.md` | Task-group organized durable entries (per-repo task groups, user preferences, reusable knowledge, failure post-mortems) with pointers into rollout summaries |
| `raw_memories.md` | Stage-1 per-thread raw memory records (thread id, cwd, rollout path, keywords) |
| `rollout_summaries/*.md` | One summary file per eligible past session (rollout) |
| `extensions/ad_hoc/` | Ad-hoc memory extension entries |

Mechanics (per [developers.openai.com/codex/memories](https://developers.openai.com/codex/memories)
and the config reference):

- **Generation** is asynchronous: after a thread has been idle
  ≥ `memories.min_rollout_idle_hours` (default 6h), Codex extracts a per-thread
  memory (`memories.extract_model`), then periodically consolidates raw
  memories into `MEMORY.md` / `memory_summary.md`
  (`memories.consolidation_model`). Short-lived/active sessions are skipped;
  secrets are redacted.
- **Recall** is controlled by `memories.use_memories` (default `true`): Codex
  injects existing memories into future sessions at startup. There is no
  per-prompt retrieval — it is session-start injection of the consolidated
  summary plus a search-over-`MEMORY.md` affordance (the summary explicitly
  tells the model to "Search `MEMORY.md` first for …").
- `memories.generate_memories` (default `true`) gates whether new threads
  become generation inputs.
- `memories.disable_on_external_context` (default **`false`**) can exclude
  threads that used MCP/web-search from memory generation. Since tracedecay's
  MCP server is active in most sessions here, native memory generation
  currently *does* consume MCP-heavy threads (both systems learn from the same
  sessions — duplication risk, see §5).
- Managed via `/memories` in the TUI and `codex debug clear-memories`.
- `[features.memories] custom_tools = true` (nested form) additionally exposes
  memories read/retrieval **tools** to the model — i.e. Codex is moving toward
  model-invocable memory reads, which is exactly the slot a
  `tracedecay_fact_store` MCP tool already occupies.

### 1.2 Other context surfaces Codex reads at session start

- **`AGENTS.md`** — project-doc mechanism, auto-loaded from repo root (and
  parent/`~/.codex/AGENTS.md` global variant). Stable, always read.
- **Skills** (`~/.codex/skills/`, plus plugin-bundled skills) — frontmatter
  name/description always visible; body lazy-loaded on match.
- **Plugins** (`~/.codex/plugins/cache/<marketplace>/<name>/<version>/`) —
  bundle MCP servers, skills, and **lifecycle hooks**.
- **Rules** (`~/.codex/rules/default.rules`) — command prefix allow/deny rules
  only, *not* a prompt-context surface (unlike Cursor rules).

### 1.3 Codex hook surface (the interception points)

Codex hooks are stable (`codex features list` → `hooks stable true`) and are
declared in a plugin's `hooks/hooks.json` (Claude-style schema: event →
matcher → command). Events used/verified by the tracedecay plugin
(`~/.codex/plugins/cache/personal/tracedecay/0.0.23/hooks/hooks.json`, trusted
hashes recorded under `[hooks.state]` in `config.toml`):

| Event | Payload includes | Output contract | Context injection? |
| --- | --- | --- | --- |
| `SessionStart` | session id, cwd, source (`compact` for post-compaction restarts) | `hookSpecificOutput.additionalContext` | **Yes** |
| `UserPromptSubmit` | session id, cwd, **prompt text** | `hookSpecificOutput.additionalContext` | **Yes — per prompt** |
| `SubagentStart` | session id, agent type | `hookSpecificOutput.additionalContext` | **Yes** |
| `PostToolUse` | tool name, command | (used for ingest/steering) | Yes |
| `PostCompact` | rollout path | side effects (summary replacement) | Indirect |

**Key asymmetry vs Cursor:** Codex's `UserPromptSubmit` hook receives the
prompt text *and* can inject `additionalContext` — a true per-prompt memory
recall channel. TraceDecay already exploits this channel for tool-routing
hints (`src/hooks/codex.rs::hook_codex_user_prompt_submit` →
`codex_user_prompt_submit_context_for_event` →
`codex_additional_context_json("UserPromptSubmit", …)`), just not for facts.

---

## 2. How Cursor reads memory today

### 2.1 Native "Memories"

- Auto-generated short rules derived from chats, produced by a **background
  sidecar model** (hybrid with agent tool-calls for explicit "remember this"),
  scoped per project, stored **server-side** on the Cursor account (not in a
  user-inspectable local file). Managed in Settings → Rules → Memories
  (review/approve/edit/delete). Requires Privacy Mode disabled; toggle is
  "Generate Memories".
- Injected into agent context automatically alongside rules. There is no
  local file, no CLI, and no hook that exposes or intercepts the memory
  read/write path. **Cursor memories are a black box: not interceptable,
  only disableable.**

### 2.2 Rules and context surfaces (the reliable injection path)

Precedence: Team Rules → Project Rules → User Rules, merged.

- **Project rules:** `.cursor/rules/*.mdc` (frontmatter `alwaysApply`,
  `globs`, `description`).
- **Plugin rules:** plugins under `~/.cursor/plugins/local/<name>/rules/*.mdc`
  are injected as always-applied workspace rules — this is how tracedecay's
  `tracedecay.mdc` rule reaches every session today.
- **`AGENTS.md`** at repo root; User Rules in settings.
- Community + Cursor-staff consensus (forum, 2026): *"`.cursor/rules` with
  `alwaysApply: true` is the most reliable way to inject context into every
  prompt today."*

### 2.3 Cursor hook surface

Hooks live in `~/.cursor/hooks.json`, project `.cursor/hooks.json`, or plugin
`hooks/hooks.json` (tracedecay uses the plugin variant, verified at
`~/.cursor/plugins/local/tracedecay/hooks/hooks.json`). Events (per
cursor.com/docs/hooks): `sessionStart`/`sessionEnd`, `preToolUse`/`postToolUse`/
`postToolUseFailure`, `subagentStart`/`subagentStop`, `beforeShellExecution`/
`afterShellExecution`, `beforeMCPExecution`/`afterMCPExecution`,
`beforeReadFile`/`afterFileEdit`, `beforeSubmitPrompt`, `preCompact`, `stop`,
`afterAgentResponse`/`afterAgentThought`, `workspaceOpen`.

Context-injection capability per event (verified against docs + forum + the
comment in `src/hooks/cursor.rs::hook_cursor_before_submit_prompt`):

| Event | Injection field | Notes |
| --- | --- | --- |
| `sessionStart` | `additional_context` (+ `env`) | Documented "added to the conversation's initial system context". **Known reliability bugs** (forum reports of injected context being dropped due to a timing gap) — treat as best-effort |
| `postToolUse` | `additional_context` | Works; also has reported plumbing gaps |
| `beforeSubmitPrompt` | **none** — output schema is `{continue, user_message}` only | Receives the prompt + attachments, but **cannot** add context or modify the prompt (`updated_input` silently stripped). Per-prompt recall injection is architecturally impossible in Cursor today; there is an open feature request |
| `stop` / `sessionEnd` / `afterAgentResponse` | none | Observation only — good for **storage** side-effects, not recall |
| `preCompact` | none | Observation only |

So for Cursor: **recall** must ride on `sessionStart.additional_context`
(best-effort), an always-applied rule file (reliable), or model-initiated MCP
tool calls (reliable but discretionary). **Storage** interception rides on
`stop`/`sessionEnd`/transcript ingest — which TraceDecay already does.

---

## 3. What TraceDecay installs today

### 3.1 `tracedecay install --agent cursor` (`crates/tracedecay-agent-hosts/src/agents/cursor.rs`)

Writes the Cursor projection of the shared plugin bundle
(`crates/tracedecay-agent-hosts/src/agents/plugin_bundle.rs::cursor_files`) to
`~/.cursor/plugins/local/tracedecay/`:

- **`mcp.json`** — stdio server `tracedecay serve --path ${workspaceFolder}`
  (all fact-store/memory/graph tools available to the model).
- **`hooks/hooks.json`** — 9 hooks: `sessionStart`, `beforeSubmitPrompt`,
  `postToolUse`, `afterFileEdit`, `afterShellExecution`, `preCompact`,
  `sessionEnd`, `stop`, `workspaceOpen`, each shelling to
  `tracedecay hook-cursor-*` (dispatch: `src/hook_cmd.rs`, impls:
  `src/hooks/`).
- **`rules/tracedecay.mdc`** — always-applied rule; its **Recall** bullet
  steers models to `tracedecay_message_search` / `tracedecay_fact_store`
  search and the `project-memory` skill.
- **`skills/`** — shared model-invocable skills, excluding the
  `tracedecay-*` dispatcher skills that Cursor exposes as native commands.
- **`commands/`** — Cursor-native workflow commands from
  `plugin/overlays/cursor/commands/`.
- **`agents/`** — Cursor agent definitions from
  `plugin/overlays/cursor/agents/`.

What the hooks currently do (all fail-open):

- `sessionStart` (`hook_cursor_session_start`) — catch-up transcript ingest,
  then emits `additional_context` built by `build_cursor_session_context`
  (`src/hooks/steering.rs`): **index status + skill list + tokens-saved
  counter. No facts.** Sets `TRACEDECAY_PROJECT_ROOT` env.
- `beforeSubmitPrompt` — hot transcript ingest only; emits
  `{"continue": true}` (no context possible, see §2.3).
- `postToolUse` — tool-routing hints via `additional_context`
  (`cursor_post_tool_use_decision`).
- `stop`/`sessionEnd`/`preCompact` — transcript ingest into the LCM store,
  pre-compaction summary nodes. This is the **storage-side raw material**:
  every Cursor session ends up searchable via `tracedecay_message_search` and
  becomes evidence for the session_reflector.

### 3.2 `tracedecay install --agent codex` (`crates/tracedecay-agent-hosts/src/agents/codex.rs`)

Installs the Codex projection of the shared plugin bundle
(`crates/tracedecay-agent-hosts/src/agents/plugin_bundle.rs::codex_files`) to
`~/.codex/plugins/cache/personal/tracedecay/<version>/` plus a personal
marketplace entry (`install_codex_marketplace_entry`) and
`[plugins."tracedecay@personal"] enabled = true` in `config.toml`:

- **`.mcp.json`** (`codex_plugin_mcp`, `crates/tracedecay-agent-hosts/src/agents/codex.rs:563`) — same
  stdio tracedecay server.
- **`hooks/hooks.json`** (`codex_plugin_hooks`, `crates/tracedecay-agent-hosts/src/agents/codex.rs:582`) —
  `SessionStart`, `UserPromptSubmit`, `SubagentStart`,
  `PostToolUse` (matcher `Bash|apply_patch`), `PostCompact`
  (matcher `auto|manual`). Hooks require one-time `/hooks` trust
  (`print_hook_trust_guidance`); trusted hashes live in `[hooks.state]`.
- **`skills/`** — shared skills from `plugin/skills/` plus the
  `agent-managed/` overlay.
- **No rule surface exists in Codex**, so the steering text Cursor gets via
  `tracedecay.mdc` is injected through `SessionStart`/`UserPromptSubmit`
  `additionalContext` instead (`build_codex_session_context`,
  `codex_user_prompt_submit_context_for_event` — index status + skills + tool
  hints. **No facts.**).
- `PostCompact` replaces encrypted compaction placeholders with LCM-backed
  summaries via a `codex app-server` child.

Codex transcripts (rollouts under `~/.codex/sessions/`) are ingested into the
LCM store as well (session-recall skills cover "Cursor/Codex/agent
transcripts"), so Codex sessions also feed reflection.

### 3.3 The fact store itself

- Per-project holographic store (`memory_facts` + entities, HRR dim 2048,
  4 banks, amari_fhrr algebra; `src/memory/{store,retrieval,trust,types}.rs`).
  `tracedecay_memory_status` on this repo: 9 facts, 113 entities, trust all
  ≥ 0.75.
- MCP/CLI surface: `tracedecay_fact_store` with actions
  add/search/probe/related/reason/contradict/get/update/remove/list, write-time
  near-duplicate & conflict detection, secret rejection, trust calibration
  guidance; `tracedecay_fact_feedback` (helpful/unhelpful trust deltas);
  `tracedecay_memory_status`. Cross-project reads via
  `project_id`/`project_path` selectors (read-only actions).
- Retrieval API (`src/memory/retrieval.rs::FactRetriever`): `search`,
  `probe`, `related`, `reason`, `contradict` — directly callable **in-process
  from the hook binary** (the hooks are the same `tracedecay` binary; no MCP
  round-trip needed).

### 3.4 Storage loop: session_reflector (exists, automation disabled by default)

`crates/tracedecay-agent-hosts/src/automation/{runner,session_reflector,fact_proposals}.rs`:

- Scheduler-driven task (`AgentTaskKind::SessionReflector`,
  `crates/tracedecay-agent-hosts/src/automation/scheduler.rs:301`) gathers LCM session evidence, prompts an
  automation backend (`build_session_reflector_prompt`), and validates
  returned **fact proposals** against evidence citations
  (`validate_fact_proposals` — each proposal must cite raw messages / store
  ids / summary nodes; trust bounded by `proposal_trust_value`).
- Accepted proposals flow through the automation apply policy
  (`fact_proposals.rs::record_session_fact_proposals`): self-managed memory
  apply is the normal model-managed path, while the dashboard exposes outcomes,
  telemetry, and explicit apply/reject controls for configured review modes.
- **Defaults are conservative:** automation starts disabled
  (`enabled: false`, `backend: Disabled`; `crates/tracedecay-agent-hosts/src/automation/config.rs:101-113`).
  The write path from transcripts → facts exists but is off unless the user
  enables automation.

### 3.5 Host skill inventory and deployment

`tracedecay_hermes_skill_bridge` provides a read-only inventory of skills,
pending approvals, usage, and archives from the standard `~/.hermes` install.
It accepts no alternate home or profile selector. Skill deployment to agents
goes through the managed-skill overlay
(`managed_skills.rs`, `skill_targets.rs`, `install_*_managed_skill_overlay`)
so agent-authored skills land in the `agent-managed/` directory of each
installed plugin. This is the template for "TraceDecay materializes generated
content into agent surfaces on a schedule," which design D reuses for memory.

---

## 4. Gap analysis: "facts exist" vs "agents recall them"

| Stage | Codex | Cursor |
| --- | --- | --- |
| Facts stored | ✅ fact store (9 facts here) + LCM transcripts | same store |
| Model *can* recall | ✅ MCP `tracedecay_fact_store` search + skill | ✅ same |
| Model is *told* to recall | ⚠️ soft steering in SessionStart/UserPromptSubmit context; `project-memory` skill matches only when the model thinks "recall" | ⚠️ one Recall bullet in `tracedecay.mdc`; same skill-match dependency |
| Facts *pushed* into context | ❌ none — hook context is index status + hints only | ❌ none |
| Automatic storage | ⚠️ session_reflector exists but disabled by default; skills say "add facts **only when the user asks**" (`project-memory` guardrail) | same |
| Native memory overlap | ⚠️ Codex memories **on** (`features.memories=true`), learning from the same threads in parallel | Unknown toggle state; server-side, uninspectable |

The delta is precisely: **nothing proactively retrieves facts at
session/prompt time**, and **nothing writes facts without an explicit user
request or a (disabled) automation run**. Recall depends entirely on the
model electing to call an MCP tool; storage depends on the user saying
"remember this."

---

## 5. Integration designs

### A. Codex per-prompt & session-start fact injection via existing hooks

Codex is the lowest-effort path because `UserPromptSubmit` carries the prompt
text and honors `hookSpecificOutput.additionalContext`, and the hook binary is
`tracedecay` with in-process store access (no MCP hop, fits the 5s timeout).

- `SessionStart`: in `codex_session_context_for_event` (`src/hooks/codex.rs`),
  after workspace status, run `FactRetriever::search`/`list` for the top-K
  high-trust project facts (e.g. K=8, `min_trust` 0.6, category-diverse,
  newest-first tiebreak) and append a compact `## Project memory` block with
  fact ids ("rate with tracedecay_fact_feedback, correct with fact_store
  update"). Reuse the token-budget discipline the context builders already
  have.
- `UserPromptSubmit`: in `codex_user_prompt_submit_context_for_event`
  (`src/hooks/codex.rs`), embed the prompt (`prompt_like_text`) via the
  existing HRR encoder and inject only facts above a similarity × trust
  threshold, deduped per session the same way tool hints are deduped
  (`deduped_codex_hint` pattern, `remember_hint_in_process`). Empty result →
  inject nothing (most prompts).
- `SubagentStart`: optionally include the same session-start block so
  subagents inherit memory.

Implementation pointers: `src/hooks/codex.rs`, `src/hooks/cursor.rs`,
`src/hooks/steering.rs`, and shared helpers in `src/hooks/mod.rs`,
`src/memory/retrieval.rs` (`FactRetriever::search/probe`), analytics via
`record_hint_analytics` so injection quality is measurable. No plugin schema
change; hook hashes change → users re-trust via `/hooks` (already documented
in the Codex plugin README).

### B. Cursor session-start injection + a materialized memory rule

Per-prompt injection is impossible in Cursor (§2.3), so combine the two
channels that exist:

1. **`sessionStart.additional_context`** — extend
   `build_cursor_session_context` / `cursor_session_context_for_root`
   (`src/hooks/steering.rs`, `src/hooks/cursor.rs`) with the same top-K fact
   block as design A.
   Cheap, but treat as best-effort given the open forum bugs about dropped
   `additional_context`.
2. **Materialized always-applied memory rule** — generate
   `~/.cursor/plugins/local/tracedecay/rules/tracedecay-memory.mdc`
   (`alwaysApply: true`) from the fact store: a "Project memory (generated —
   curate via tracedecay_fact_store / dashboard)" section listing top-K
   high-trust facts with ids. This is the *reliable* channel per Cursor's own
   guidance. Refresh triggers: `workspaceOpen`/`sessionStart` hooks (rewrite
   if stale > N minutes — hooks already have write access to the plugin dir)
   and/or a scheduler task. Keep it small (facts are one-liners; cap ~1–2 KB)
   and deterministic (sorted) so diffs are reviewable.

Implementation pointers: add the managed rule to the Cursor projection in
`crates/tracedecay-agent-hosts/src/agents/plugin_bundle.rs` / `crates/tracedecay-agent-hosts/src/agents/cursor.rs`, refresh it from
`hook_cursor_workspace_open` (`src/hooks/cursor.rs`), and mark it managed the
same way generated skills are marked (`managed_skill_format.rs`) so uninstall
and doctor checks cover it.

### C. Rule/skill text: make storage proactive

Today `project-memory`'s guardrail says add facts "**only when the
user asks**" — the opposite of agent-memory behavior. Change the instruction
(rule Recall bullet in `plugin/rules/tracedecay.mdc` + the
`plugin/skills/project-memory/SKILL.md` skill, shared across every host) to:

- *Recall:* "before starting non-trivial work, search `tracedecay_fact_store`
  for prior decisions" (currently phrased as fallback, not default).
- *Storage:* "when you learn a durable preference, decision, or pitfall
  (user corrections especially), store it with `fact_store add` with
  calibrated trust" — the add-path already defends against junk
  (near_duplicate / possible_conflict / secret rejection, §3.3).

This mirrors Cursor's hybrid design (sidecar + tool calls), with the tool-call
half pointed at TraceDecay.

### D. Enable the reflection loop (sidecar-equivalent storage)

session_reflector is the background sidecar analog: transcripts (both agents,
already ingested by the hooks) → evidence-cited fact operations → model-managed
apply policy → store, with dashboard inspection/telemetry. The work is
activation, not construction:

- Surface a one-command enable (`tracedecay automation enable
  session-reflector`) that picks a backend and uses model-managed memory apply
  defaults; document the explicit dashboard-review mode for high-risk rollout.
- Wire a nudge into `doctor`/`memory_status` when transcripts accumulate but
  automation is disabled ("N sessions ingested, 0 reflected").

Pointers: `crates/tracedecay-agent-hosts/src/automation/config.rs` (defaults/validation),
`crates/tracedecay-agent-hosts/src/automation/runner.rs::run_session_reflector_with_backend`,
dashboard curation UI (concurrent work in `dashboard/` — coordinate, don't
touch).

### E. Codex-native-memory coexistence policy

With `features.memories = true`, Codex builds a parallel memory in
`~/.codex/memories/` from the same sessions. Options:

1. **Coexist (default, zero change):** native memories keep user-level
   cross-repo profile; fact store owns per-project durable facts. Acceptable
   but two sources of truth drift.
2. **Prefer TraceDecay:** recommend `memories.use_memories = false` (stop
   injection, keep generation) or `features.memories = false` in install
   docs/doctor output once design A ships — A's injection replaces it with
   curated, trust-scored, per-project recall. Do **not** silently edit the
   user's `config.toml`; make it a doctor suggestion.
3. **Harvest:** one-way import of `~/.codex/memories/raw_memories.md` +
   `rollout_summaries/*.md` into fact proposals (they're clean markdown with
   cwd/thread metadata — easy to parse into `AddFactRequest`s routed through
   the same validation/apply path as D). Gives the fact store Codex's
   already-consolidated knowledge on day one.

For Cursor: native Memories can't be intercepted or exported; the only lever
is the Settings toggle. Once B+C are live, recommend users disable "Generate
Memories" to keep one memory system (document in `KIRO-INTEGRATION.md`-style
agent doc; can't be automated).

### F. Materialized `AGENTS.md` / memory-file generation

Scheduled materialization of facts into files agents read natively without
any tool call: a `## Memory (generated by tracedecay)` fenced section in repo
`AGENTS.md`, or a `~/.codex/memories/extensions/tracedecay/` entry (Codex
reads the memories dir natively when the feature is on). Compared to B2 this
reaches *every* agent (Claude, Gemini, etc. — `crates/tracedecay-agent-hosts/src/agents/` has 15
integrations) via one artifact, but it edits user/repo-owned files (needs
fenced-section ownership, `.gitignore` questions, merge conflicts) and its
freshness is only as good as the schedule. Recommend only as the
generalization step after A–C prove out, implemented as an automation task
alongside D and reusing the managed-file conventions from the skill overlay.

### Non-options investigated and rejected

- **Cursor `beforeSubmitPrompt` recall injection** — output schema is
  `{continue, user_message}` only; `updated_input`/`additional_context` are
  silently stripped (confirmed by Cursor staff on the forum; open feature
  request). Revisit if Cursor ships the field.
- **Rewriting the user prompt via `user_message`** — `user_message` is a
  user-facing notice, not model context; abusing it shows UI noise.
- **Intercepting Cursor native Memories** — server-side, no local artifact,
  no hook. Only the on/off toggle exists.
- **Codex `notify` mechanism** — fire-and-forget desktop notification hook;
  no context channel.

---

## 6. Recommended sequence

1. **A + B1** (hook-based injection, both agents) — one PR across `src/hooks/`
   + `src/memory/retrieval.rs` helpers; measurable via existing hook
   analytics.
2. **B2 + C** (materialized Cursor memory rule + proactive storage wording) —
   one PR in `crates/tracedecay-agent-hosts/src/agents/plugin_bundle.rs`, `crates/tracedecay-agent-hosts/src/agents/cursor.rs`, and plugin
   rule/skill text (shared skill text under `plugin/skills/`).
3. **D** (reflector enablement UX) — config/doctor/dashboard nudge.
4. **E** (coexistence policy + optional Codex-memories harvest importer).
5. **F** (generalized AGENTS.md materialization across all 15 agent
   integrations) — only if A–C leave recall gaps for agents without hook
   surfaces.

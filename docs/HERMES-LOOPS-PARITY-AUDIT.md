# Hermes Self-Improvement Loops — Parity Audit

Status: comparison audit, July 2026. Hermes agent codebase examined at
`~/projects/hermes-agent` (registered tracedecay project
"hermes-agent"). TraceDecay side examined at HEAD of
the current TraceDecay worktree after dashboard automation work landed.

This audit compares Hermes's self-improvement machinery (background review,
memory writing, skill writing, curator, cron) against TraceDecay's automation
subsystem (`crates/tracedecay-agent-hosts/src/automation/*`, daemon scheduler, dashboard `/api/automation/*`)
and produces a prioritized gap-closure plan. Companion docs:
`SELF-IMPROVING-LOOPS-CONTRACTS.md` (contracts), `MEMORY-CURATION-AUTONOMY.md`
(curation runbook).

---

## 1. How Hermes does it

### 1.1 Background review after turns (`agent/background_review.py`, 608 lines)

The core loop is **turn-triggered, not scheduled**. After every completed
foreground turn, `turn_finalizer.py:393` may call
`AIAgent._spawn_background_review`, which forks a second `AIAgent` on a daemon
thread and replays the **full conversation snapshot** into it with a review
prompt. Trigger conditions (`agent/turn_context.py:209-217`,
`agent/turn_finalizer.py:375-381`):

- **Memory review**: every `memory.nudge_interval` user turns (default **10**,
  `agent/agent_init.py:1113`), counted per session.
- **Skill review**: whenever the just-finished turn's tool-calling iterations
  push `_iters_since_skill` past `skills.creation_nudge_interval` (default
  **10** iterations).
- When both fire, a combined prompt is used. Review runs *after* the response
  is delivered, so it never competes with the user's task.

Engineering details worth noting:

- The fork inherits the parent's live runtime — provider, model, credentials,
  and the **byte-identical cached system prompt** — so it hits the same
  prefix cache (~26% measured cost reduction of the review pass).
- The fork runs with a **tool whitelist** limited to the `memory` and `skills`
  toolsets; everything else is denied at dispatch. It has `max_iterations=16`,
  i.e. it is a real agent loop that can call `skills_list`/`skill_view` to
  inspect the library before deciding to patch vs create.
- If the parent runs on `codex_app_server` (which bypasses Hermes's own tool
  dispatch), the fork is downgraded to `codex_responses` so Hermes still owns
  the loop.
- A compact action summary ("💾 Self-improvement review: Skill 'X' patched")
  is printed to the user immediately after the review completes.

### 1.2 Review prompts (the editorial policy)

The prompts are the most refined part of the system
(`_SKILL_REVIEW_PROMPT` / `_COMBINED_REVIEW_PROMPT`,
`agent/background_review.py:45-233`). Key policy encoded in prose:

- **Be active by default**: "most sessions produce at least one skill update;
  a pass that does nothing is a missed learning opportunity."
- **Class-level umbrella skills** with `references/`, `templates/`, `scripts/`
  support directories — not one narrow skill per session.
- **Strict preference ordering**: (1) patch a skill that was *loaded this
  session*, (2) patch an existing umbrella, (3) add a support file under an
  umbrella, (4) only then create a new skill — and its name must survive the
  "does this only make sense for today's task?" test.
- **User frustration is a first-class skill signal**, not just a memory
  signal: "stop doing X" gets embedded in the skill that governs that task.
- **A do-NOT-capture list**: environment-dependent failures, negative claims
  about tools ("X is broken" hardens into self-imposed refusals), transient
  errors that resolved, and one-off task narratives.
- **Protected skills**: bundled and hub-installed skills are read-only for
  the review; pinned skills can be patched but not deleted/consolidated.

### 1.3 Memory writing (`tools/memory_tool.py`, `agent/memory_manager.py`, `agent/memory_provider.py`)

- Built-in memory is two flat files under `~/.hermes/memories/`:
  **MEMORY.md** (agent's own notes) and **USER.md** (user profile), with
  §-delimited multiline entries and **character budgets** (2200 / 1375 chars).
  Single `memory` tool with `add/replace/remove/read` actions using substring
  matching, atomic writes, cross-process file locking, drift detection
  (refuses to clobber external edits), and an injection/threat scanner on
  every entry before it enters the system prompt.
- The whole store is injected into the system prompt as a **frozen snapshot**
  at session start (prefix-cache friendly); mid-session writes go to disk but
  don't invalidate the prompt.
- `MemoryManager` supports exactly one pluggable **external MemoryProvider**
  (Honcho, Mem0, Hindsight, …) with lifecycle hooks: `prefetch(query)` before
  each turn, `sync_turn(user, asst)` after, `on_memory_write` mirroring, and
  `on_pre_compress` extraction. Provenance metadata
  (`write_origin=background_review`, session ids, platform) is attached to
  every mirrored write.

### 1.4 Skill writing and management (`tools/skill_manager_tool.py`, 1125 lines)

- `skill_manage` actions: `create`, `edit` (full SKILL.md rewrite), `patch`
  (targeted find/replace in SKILL.md **or any support file**), `delete`,
  `write_file`, `remove_file`. Skills live at `~/.hermes/skills/<name>/`
  with `references/`, `templates/`, `scripts/`, `assets/`.
- New/edited skills are **live for the next session immediately** — no deploy
  step, because Hermes loads the same directory it writes.
- Optional security scan of agent-created skills (`skills_guard`, off by
  default), and an optional **write-approval gate**
  (`tools/write_approval.py`): per-subsystem boolean; when on, background
  writes are staged to `~/.hermes/pending/{memory,skills}/<id>.json` and
  reviewed via `/memory pending`, CLI, or the web dashboard. **Default is
  off — writes flow freely.**

### 1.5 Skill-library curator (`agent/curator.py`, 1835 lines)

A separate, slower consolidation loop, distinct from per-turn review:

- **Inactivity-triggered**, no daemon: when the agent has been idle
  ≥ `min_idle_hours` (default 2) and the last run was ≥ `interval_hours` ago
  (default 168 = weekly), `maybe_run_curator()` forks a review agent.
- It only touches **agent-created** skills (provenance-filtered via the
  `.usage.json` sidecar), **never deletes — only archives** (recoverable),
  snapshots the library before real runs, and seeds its first run instead of
  mutating immediately. Pinned skills are exempt from lifecycle transitions.
- Lifecycle states in the sidecar (`tools/skill_usage.py`): `active` →
  `stale` (unused > 30 days) → `archived` (unused > 90 days), pinned as an
  orthogonal flag. Counters bumped by real `skill_view`/`skill_manage` calls.

### 1.6 Cron / scheduled jobs (`cron/jobs.py`, `cron/scheduler.py`, ~3400 lines)

A general-purpose user-facing automation platform, not a fixed task list:

- Jobs stored in `~/.hermes/cron/jobs.json`; output archived to
  `~/.hermes/cron/output/{job_id}/{timestamp}.md`. The gateway ticks every
  60 s (`hermes cron tick` also works standalone); a file lock serializes
  overlapping ticks.
- Per-job features (`create_job`, `cron/jobs.py:523`): arbitrary prompt, cron
  expression or human-readable schedule, repeat counts, **attached skills**
  (ordered list), **pre-run scripts** whose stdout is injected as context (or
  `no_agent=True`, where the script *is* the job), **context chaining** from
  other jobs' latest output, **per-job model/provider/base_url overrides**,
  per-job toolset restriction, `workdir` with AGENTS.md injection, and
  **delivery targets**: telegram, discord, slack, whatsapp, signal, sms,
  email, webhook, github comments, local files, "origin".
- Safety: cron-spawned agents always have `cronjob`, `messaging`, `clarify`
  toolsets disabled (no self-scheduling), and the fully-assembled prompt —
  including loaded skill content — passes an injection scanner before run.
- Adjacent: a **blueprint catalog** and a **suggestion engine**
  (`cron/blueprint_catalog.py`, `cron/suggestions.py`) that proposes
  automations to the user, plus webhook subscriptions as event triggers.

### 1.7 Feedback / validation / eval loops in Hermes

Comparatively thin. There is usage telemetry (sidecar counters), the curator's
periodic LLM-judged consolidation, the optional security scan, and the
optional write-approval staging — but **no artifact chain, no generated
evals, no validation gate, no evidence hashing, and no default human
approval**. Quality control is mostly encoded in prompt policy plus the
curator's conservative invariants.

---

## 2. How TraceDecay does it

### 2.1 Scheduler and lifecycle (`crates/tracedecay-agent-hosts/src/automation/scheduler.rs`, `lifecycle.rs`, `src/daemon.rs`)

- The daemon runs one `run_automation_scheduler_loop` per project
  (`src/daemon.rs:1228`), ticking every `scheduler_tick_secs` (default 60).
  Three fixed tasks: `memory_curator` (every 15 min), `session_reflector`
  (every 15 min), `skill_writer` (every 60 min) — all **wall-clock interval
  scheduled**, `AutomationSchedule` supports `manual`, `interval`, `hourly`,
  `daily`, `weekly`, `every N<unit>` (`scheduler.rs:251`) but no cron
  expressions and no event triggers.
- Per-task gates: file lock with stale-lock recovery, non-retryable-failure
  circuit breaker, failure cooldown (default 300 s), and `min_idle_secs`.
  **Note**: `min_idle_secs` measures elapsed time since the *last automation
  run of the same task* (`scheduler.rs:203-208`), not user/session
  inactivity — despite the "idle window" naming in
  `SELF-IMPROVING-LOOPS-CONTRACTS.md`. TraceDecay currently has no gate on
  actual agent activity.
- Every run — including skips, deduped — is appended to a run ledger
  (`run_ledger.rs`) with status, trigger, evidence/input hashes, error
  classification (`Retryable/Permanent/Timeout/Unavailable/MalformedOutput`,
  `backend.rs:119-201`), accepted/rejected counts, and per-run artifacts.

### 2.2 The three tasks

- **memory_curator** (`memory_curator.rs`): runs the deterministic
  similarity-dedup + hygiene planner (`dashboard/memory_curate.rs`), sends the
  bounded `llm_review` clusters to the backend, then **re-validates the
  returned ops against freshly recomputed evidence** before any apply. Apply
  policy: auto-apply when `auto_apply_memory_ops=true` — automation applies
  without any human approval gate (`require_dashboard_approval` is deprecated
  and ignored); otherwise ops surface for dashboard review. Destructive-op
  counts (permanent deletes, merge losers) are reported explicitly.
- **session_reflector** (`runner.rs:170-420`, `session_reflector.rs`):
  evidence = `lcm_grep` over the LCM session store with a **fixed keyword
  query** (`"remember prefer decision requirement workflow"`, limit 20 hits,
  recency-sorted, summaries included). The backend must return a strict JSON
  `facts` array; each fact is validated (category whitelist, numeric trust,
  **mandatory `source_span` citing a specific evidence hit**) and recorded as
  a `FactProposalRecord` (`fact_proposals.rs`) in
  `pending_approval/applied/rejected` states for telemetry, and accepted facts
  are auto-applied under the same policy pair as above.
- **skill_writer** (`runner.rs:430-707`, `skill_writer.rs`): evidence =
  LCM grep (query `"workflow correction repeated skill tool pattern"`) +
  **existing managed skills** (metadata, body up to 4000 chars, support-file
  previews) + skill-usage summaries + stale recommendations + underused-tool
  signals + derived improvement recommendations. Proposals (create/update
  with `base_checksum` optimistic concurrency) are validated and land as
  `pending_approval` drafts; `auto_enable_skills` (default false) can promote
  them to `active`.

### 2.3 Backend (`backend.rs`)

Single-shot prompt → strict-JSON response through
`run_prompt_with_codex_app_server`. There is **no tool loop**: the model
cannot browse, grep, or read anything beyond the evidence bundle serialized
into the prompt. Contracts carry `prompt_version` (all `:v1`), a response
schema, and a deterministic `input_hash` over
task+contract+prompt+evidence+context. `external_command` backend is declared
but not implemented.

### 2.4 Artifact chain (`artifacts.rs`, `artifact_payloads.rs`)

Every backend-validated run writes six chained artifacts, each hash-linked to
its predecessors: **traces → feedback → generated_evals → validation_gate →
optimizer_diagnosis → codex_handoff**. These are inspectable via
`/api/automation/runs/{run_id}/artifacts[/{kind}]`. The handoff is the durable
output for broader improvement work and preserves the configured apply policy.

### 2.5 Managed skills and distribution (`managed_skills.rs`, `skill_targets.rs`)

- Lifecycle: `pending_approval → active → disabled/archived`, pinned flag,
  checksums, provenance, staged updates with `approve`/`discard-update`.
  Dashboard endpoints cover the full lifecycle
  (`crates/tracedecay-dashboard-api/src/automation_skills_api.rs`, routes in
  `src/dashboard.rs:483-559`).
- Distribution is **export-based**: active skills are rendered into a native
  overlay (`skills/agent-managed/` under the Cursor/Codex plugin root) or a
  compact prompt index + `tracedecay_skill_view` MCP serving for prompt-only
  hosts (Claude Code, OpenCode, Kimi, Kiro). Hermes is host-owned and
  explicitly excluded as a target.
- **Gap in the loop**: export runs only during `tracedecay install` /
  `update-plugin` / agent install paths (`crates/tracedecay-agent-hosts/src/agents/cursor.rs:397`,
  `crates/tracedecay-agent-hosts/src/agents/codex.rs:508-534`, `crates/tracedecay-agent-hosts/src/agents/kiro.rs:367`,
  `src/automation_cli.rs:365`). The dashboard `approve` handler
  (`automation_skills_api.rs:167`) flips state but does **not** re-export, so
  an approved skill does not reach any agent until the next install/update.
- Usage telemetry (`skill_usage.rs`): sidecar ledger with
  view/use/patch counts, ingested from MCP tool analytics
  (`ingest_project_analytics_events`); stale scoring and improvement
  recommendations feed back into skill_writer evidence.

### 2.6 Hermes-owned skill state

Hermes owns its profile skills, pending approvals, usage, and curator state.
TraceDecay does not expose or route that profile-local state through an MCP
bridge; TraceDecay managed skills use the same plugin-bundle lifecycle as the
other host integrations.

---

## 3. Gap analysis

### 3.1 What Hermes has that TraceDecay lacks

| # | Gap | Hermes | TraceDecay today |
|---|-----|--------|------------------|
| G1 | **Turn/activity-coupled triggering** | Review fires right after the relevant turn, with counters tied to actual user turns and tool iterations | Fixed wall-clock intervals; runs happen whether or not anything new occurred, and fresh sessions can wait up to 15–60 min. `min_idle_secs` doesn't observe real activity |
| G2 | **Full-conversation evidence** | The review fork replays the entire conversation snapshot | Keyword `lcm_grep` (fixed query strings, 20 hits). Signals that don't contain "remember/prefer/workflow/correction…" are invisible; no session replay, no turn adjacency |
| G3 | **Agentic review loop** | Review fork is a 16-iteration agent that can `skills_list`/`skill_view` before deciding to patch vs create | Single-shot prompt → strict JSON; the model only sees what the evidence bundle pre-serialized |
| G4 | **Editorial prompt policy** | Class-level umbrella skills, patch-over-create preference ladder, frustration-as-signal, do-NOT-capture list, protected-skill rules | ~1-paragraph schema-focused prompts (`runner.rs:741-753`); no update-over-create preference, no anti-capture rules, no naming policy |
| G5 | **Instant skill deployment** | Written skills are live next session (same directory) | Approval flips state only; overlay export waits for the next `install`/`update-plugin` |
| G6 | **General-purpose scheduled jobs** | User-defined cron jobs with prompts, cron exprs, attached skills, scripts, context chaining, model overrides, delivery to 15+ channels, webhook triggers, blueprints, suggestions | Exactly 3 fixed self-improvement tasks; interval schedules only; no user-defined jobs, no delivery targets, no event/webhook triggers |
| G7 | **Immediate user visibility** | "💾 Self-improvement review: …" surfaces inline in the session right after it happens; pending-write review via slash commands | Results visible only if the user opens the dashboard or reads the run ledger; no push/notification channel |
| G8 | **Consolidation curator** | Weekly idle-gated curator that pins/archives/consolidates overlapping agent-created skills, with pre-run snapshots | Stale scoring + improvement recommendations exist, but no consolidation loop and no overlap detection between managed skills |
| G9 | **Combined memory+skill pass** | One combined review when both triggers fire — one model call, shared context | Three independent tasks that each re-collect evidence and pay a separate backend call |
| G10 | **Memory injected into the loop** | MEMORY.md/USER.md snapshot enters every session's system prompt; provider prefetch each turn | TraceDecay facts reach agents only via MCP recall tools; the loops write memory but nothing pushes curated memory into host prompts |

### 3.2 What TraceDecay has that Hermes lacks

| # | Strength | TraceDecay | Hermes |
|---|----------|-----------|--------|
| S1 | **Model-managed memory with safety policy** | Fact automation is self-managed by the model/automation loop; destructive memory ops remain policy-gated and telemetry-visible without a dashboard preview gate | `write_approval` default **off**; background review writes memory and skills freely (the source of the "wrong assumptions" complaints its own docstring admits) |
| S2 | **Deterministic validation gate** | LLM output re-validated against recomputed evidence (allowed fact ids, confidence floor, mandatory source spans, checksum-guarded skill updates) | Trusts the review fork's writes; only optional post-hoc security scan |
| S3 | **Artifact/evidence chain** | traces → feedback → generated evals → validation gate → optimizer diagnosis → codex handoff, hash-linked, per run, API-inspectable | Cron output archives and a curator report file; no structured chain |
| S4 | **Run ledger and failure discipline** | Trigger provenance, input/evidence hashes, error classification, non-retryable circuit breaker, cooldowns, skip dedup, stale locks | Best-effort daemon thread; failures log a warning and vanish |
| S5 | **Structured memory** | HRR/FHRR-encoded fact store with trust scores, categories, entities, contradiction checks, FTS, oplog | Two flat §-delimited markdown files with char budgets (plus optional external providers) |
| S6 | **Multi-host skill distribution** | One managed store exported to Cursor, Codex, Claude Code, OpenCode, Kimi, Kiro (overlay or prompt-index + MCP body serving), plus shareable Codex plugin artifacts | Single-host `~/.hermes/skills` |
| S7 | **Dashboard observability surface** | Full CRUD + lifecycle UI/API for skills, fact automation outcomes/proposals, scheduler pause/resume, run artifacts | CLI slash-commands and a thinner web dashboard for pending writes |
| S8 | **Standalone/delegated host split** | Clean contract for owning vs bridging the loops (incl. read-only Hermes bridge) | N/A — Hermes is always the host |

### 3.3 Verdict

TraceDecay built the **governance layer** Hermes never had (S1–S4) but is
missing the **signal layer** that makes Hermes's loops actually learn
(G1–G4): Hermes reviews the right context at the right moment with a smart
prompt and an agentic loop; TraceDecay reviews a keyword-filtered sample on a
timer with a thin prompt. Closing G1–G5 keeps every TraceDecay safety
property intact — they are orthogonal to manual approval modes.

---

## 4. Prioritized recommendations

| P | Rec | What to do | Where it lands |
|---|-----|-----------|----------------|
| **P0** | **R1. Export approved skills immediately** | On approve (and disable/archive of an active skill), re-run the overlay/prompt-index export for every configured target, so dashboard approval actually deploys. Record export results on the skill payload. | `crates/tracedecay-dashboard-api/src/automation_skills_api.rs:167` (`approve`), `crates/tracedecay-agent-hosts/src/automation/managed_skills.rs:431` (`approve_managed_skill`), reuse `crates/tracedecay-agent-hosts/src/automation/skill_targets.rs::install_managed_skills`; target list from `default_managed_skill_targets` + agent detection in `crates/tracedecay-agent-hosts/src/agents/mod.rs:55` |
| **P0** | **R2. Port the Hermes editorial policy into the prompts** | Rewrite `build_skill_writer_prompt` / `build_session_reflector_prompt` with: patch-over-create preference ladder, class-level naming rule, frustration-as-first-class-signal, and the do-NOT-capture list (env failures, negative tool claims, transients, one-off narratives). Bump `prompt_version` to `:v2` so ledger/input hashes distinguish eras. | `crates/tracedecay-agent-hosts/src/automation/runner.rs:741-753`, `crates/tracedecay-agent-hosts/src/automation/backend.rs:220-226`; source text to adapt: `hermes-agent/agent/background_review.py:45-233` |
| **P1** | **R3. Activity-coupled triggering** | Add `AutomationTrigger::SessionActivity`: have session-ingestion (hooks / LCM ingest) mark "new evidence since last run" per task, and let the scheduler tick run reflector/skill_writer when new sessions landed — instead of (or in addition to) the blind interval. Redefine `min_idle_secs` to measure time since the **last LCM ingestion/session activity** (true idle, matching the contracts doc's wording), not time since the task's own last run. | `crates/tracedecay-agent-hosts/src/automation/scheduler.rs:203-208` (`schedule_decision`), `crates/tracedecay-agent-hosts/src/automation/run_ledger.rs` (`AutomationTrigger`), `src/daemon.rs:1228` (`run_automation_scheduler_loop`); activity timestamps available from the LCM sessions DB used in `runner.rs::automation_lcm_db_path` |
| **P1** | **R4. Session-replay evidence, not just keyword grep** | For the reflector and skill writer, add a "recent completed sessions" evidence mode: pull the last N sessions' turn-ordered slices (or LCM summary DAG nodes) rather than only grep hits on fixed queries. Keep the grep as a secondary recall channel. The `session_id` option already exists on `SessionReflectorAutomationOptions` — drive it from recently-completed sessions. | `crates/tracedecay-agent-hosts/src/automation/runner.rs:170-260` and `:574-700` (evidence builders), `src/sessions/lcm` replay/summary APIs; keep `evidence_hash` semantics intact |
| **P1** | **R5. Surface loop results to the user in-session** | Emit a compact result line ("skill draft 'x' staged; 2 fact updates applied; 1 validation warning") through channels agents already see: an MCP notification / next-tool-response nudge (the hint infrastructure exists), plus a dashboard badge count. This is Hermes's "💾 Self-improvement review" moment and drives inspection of outcomes and telemetry. | `src/daemon.rs:431-530` (scheduler task logging), MCP server hint/nudge path in `src/mcp/server.rs`; dashboard: fact automation counts already derivable from `fact_proposals.rs` + `managed_skills.rs::list_managed_skills` |
| **P2** | **R6. Managed-skill consolidation pass (curator parity)** | Add an overlap/consolidation review to skill_writer evidence (pairwise similarity of managed skill bodies/titles) and allow an explicit `merge`/`archive` recommendation kind that stages — never auto-applies — consolidations. Honor pinned exactly as Hermes curator does; keep "archive not delete". | New logic beside `crates/tracedecay-agent-hosts/src/automation/skill_usage/recommendations.rs`; proposal handling in `crates/tracedecay-agent-hosts/src/automation/skill_writer.rs::validate_and_apply_skill_proposals`; similarity helpers exist in the memory/dedup stack |
| **P2** | **R7. Combined reflector+skill pass option** | When both tasks are due in the same tick, run one combined backend call with shared evidence (Hermes `_COMBINED_REVIEW_PROMPT` pattern) returning `{facts:[], skills:[]}`. Halves backend cost and gives the model cross-signal (a correction often yields both a fact and a skill patch). | `crates/tracedecay-agent-hosts/src/automation/runner.rs` (new entry point), `crates/tracedecay-agent-hosts/src/automation/backend.rs` (new `AgentTaskKind::CombinedReview` contract), scheduler dispatch in `src/daemon.rs` |
| **P2** | **R8. Deliver curated memory into host prompts** | Close the loop's consumption side: export a bounded, trust-ranked "durable facts" snapshot (Hermes MEMORY.md analogue) into the same overlay/prompt-index channel skills already use, so approved facts inform sessions without an MCP recall call. Char-budgeted and injection-scanned like Hermes's frozen snapshot. | New exporter beside `crates/tracedecay-agent-hosts/src/automation/skill_targets.rs`; fact selection from `crates/tracedecay-runtime-core/src/memory/store.rs` (trust + category filters); wire into `crates/tracedecay-agent-hosts/src/agents/{cursor,codex}.rs` install paths |
| **P3** | **R9. User-defined scheduled jobs** | Generalize the scheduler beyond the 3 fixed tasks: a job record (prompt, schedule incl. cron exprs, attached managed skills, optional pre-run command, delivery to file/webhook) executed via the same backend + ledger + artifact machinery. This is Hermes cron parity scoped to TraceDecay's infra; delivery targets can start with local file + webhook only. | New `crates/tracedecay-agent-hosts/src/automation/jobs.rs` + schedule extension in `crates/tracedecay-agent-hosts/src/automation/scheduler.rs:251` (`parse_schedule`); dashboard CRUD beside `crates/tracedecay-dashboard-api/src/automation_config_api.rs`; reference design: `hermes-agent/cron/jobs.py:523` |
| **P3** | **R10. Outcome feedback for applied changes** | Track post-approval outcomes: does an approved skill's use_count rise? Are applied facts later recalled/marked helpful, or corrected/deleted? Feed these into `feedback`/`generated_evals` artifact payloads so the chain measures real quality, and into stale scoring. | `crates/tracedecay-agent-hosts/src/automation/skill_usage.rs` (already ingests analytics), `crates/tracedecay-agent-hosts/src/automation/artifact_payloads.rs` (`feedback_payload`), memory recall-feedback hooks in `src/memory` |

Deliberately **not** recommended: copying Hermes's write-freely default
(`write_approval=false`) or its flat-file memory. TraceDecay's
policy-guarded apply posture and structured fact store are the differentiators;
the fixes above raise signal quality and loop latency without weakening
either.

---

## 5. Source map

Hermes (`~/projects/hermes-agent`):

- `agent/background_review.py` — review fork + prompts
- `agent/turn_context.py:209`, `agent/turn_finalizer.py:375` — trigger logic
- `agent/agent_init.py:1113,1203` — nudge-interval defaults (10 / 10)
- `tools/memory_tool.py`, `agent/memory_manager.py`, `agent/memory_provider.py` — memory
- `tools/skill_manager_tool.py`, `tools/skill_usage.py`, `tools/write_approval.py` — skills
- `agent/curator.py` — weekly consolidation curator
- `cron/jobs.py`, `cron/scheduler.py`, `cron/blueprint_catalog.py`, `cron/suggestions.py` — cron platform

TraceDecay (`~/projects/tracedecay`):

- `crates/tracedecay-agent-hosts/src/automation/{scheduler,lifecycle,run_ledger}.rs` — gates, locks, ledger
- `crates/tracedecay-agent-hosts/src/automation/{runner,memory_curator,session_reflector,skill_writer}.rs` — the three tasks
- `crates/tracedecay-agent-hosts/src/automation/backend.rs` — codex_app_server backend, contracts, error classes
- `crates/tracedecay-agent-hosts/src/automation/{artifacts,artifact_payloads,artifact_policy}.rs` — artifact chain
- `crates/tracedecay-agent-hosts/src/automation/{managed_skills,managed_skill_model,skill_targets,skill_usage}.rs` — skill store + export
- `crates/tracedecay-agent-hosts/src/automation/fact_proposals.rs` — fact apply-policy staging
- `crates/tracedecay-agent-hosts/src/automation/hermes_*.rs` — read-only Hermes bridge
- `src/daemon.rs:1055-1300` — per-project scheduler loops
- `src/dashboard/automation_*_api.rs`, `src/dashboard.rs:417-559` — `/api/automation/*`
- `docs/SELF-IMPROVING-LOOPS-CONTRACTS.md`, `docs/MEMORY-CURATION-AUTONOMY.md` — contracts

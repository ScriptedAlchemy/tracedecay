# Self-Improving Loop Contracts

This is the durable contract for TraceDecay-owned self-improvement loops. The
daemon scheduler, CLI, and dashboard own curation, managed skills, scheduler
jobs, and artifact generation; host adapters only load the exported results.
The first standalone backend is the Codex app-server adapter, and the same
contracts support other delegated or CLI hosts.

## Host Matrix

| Host | TraceDecay-owned behavior | Host-owned behavior | Skill delivery |
| --- | --- | --- | --- |
| Cursor | Config, ledgers, curation validation, managed skill storage, telemetry sidecars, native overlay export | Native host loading and any host-local transcript signals | Policy-valid managed `SKILL.md` packages under the generated plugin overlay |
| Codex | Config, ledgers, curation validation, managed skill storage, telemetry sidecars, native overlay export, shareable plugin artifact generation | Native plugin discovery, app-server execution when selected as backend | Policy-valid managed `SKILL.md` packages under the Codex plugin overlay or plugin artifact |
| Hermes | Config, ledgers, curation validation, managed-skill storage, scheduler/export, telemetry | Hermes profile skills and host execution | Policy-valid skills exported through the CLI/dashboard lifecycle; Hermes-owned profile skills remain host-owned |
| Claude Code | Config, ledgers, curation validation, managed-skill storage, scheduler/export, telemetry | Prompt-file loading and host-local execution | Policy-valid `SKILL.md` files and prompt index exported by the CLI/dashboard lifecycle |
| OpenCode | Config, ledgers, curation validation, managed-skill storage, scheduler/export, telemetry | Prompt-file loading and host-local execution | Policy-valid `SKILL.md` files and prompt index exported by the CLI/dashboard lifecycle |
| Kimi | Config, ledgers, curation validation, managed-skill storage, scheduler/export, telemetry | Prompt-file loading and host-local execution | Policy-valid `SKILL.md` files and prompt index exported by the CLI/dashboard lifecycle |
| Kiro | Config, ledgers, curation validation, managed-skill storage, scheduler/export, telemetry | Kiro MCP registry, agent, steering, and execution | Policy-valid profile/workspace `SKILL.md` index exported by the CLI/dashboard lifecycle |
| Prompt-only agents | Config, ledgers, curation validation, managed-skill storage, scheduler/export, telemetry | Prompt ingestion and execution | Policy-valid `SKILL.md` files and prompt index exported by the CLI/dashboard lifecycle |

The managed-skill journey is concrete: the daemon scheduler's `skill_writer`
produces a candidate, and the curator/orchestrator validates and automatically
activates and materializes every policy-valid result. Inspect outcomes and
receipts with `tracedecay automation skills list`,
`tracedecay automation skills view <id>`, or the dashboard; those surfaces can
also issue explicit pause, disable, quarantine, or rollback overrides but never
gate materialization. `tracedecay automation skills install --target <host>
--output <path>` (or the host lifecycle's export) then publishes the current
active `SKILL.md` and prompt index. Hosts load those exported files; no
read-through compatibility route remains.

## Cadence And Automation Defaults

Hermes is the reference behavior for self-improvement cadence, but TraceDecay owns its own scheduler in standalone mode. Hermes memory review and skill review are turn/iteration nudges: memory defaults to every 10 user turns when memory is enabled, skill review defaults to every 10 tool-calling iterations, and both run as a whitelisted background review fork after the foreground response. Hermes skill-library curator is separate: it runs after `curator.interval_hours` elapses, defaults to 168 hours, requires the idle gate (`curator.min_idle_hours`, default 2 hours), seeds the first run instead of mutating immediately, snapshots before real runs, and archives rather than deletes.

TraceDecay standalone automation is time-scheduled by the daemon, not by Codex native automations or host cron. The default scheduler tick is 60 seconds. `tracedecay install --agent codex --automation` enables the Codex app-server backend. The curator/orchestrator policy-validates every result and automatically applies memory operations or activates/materializes managed skills, recording durable receipts. CLI and dashboard review observes those outcomes and may issue explicit pause, disable, quarantine, or rollback overrides; review is never an approval gate. The task cadences are:

| Task | Default cadence | Default mutation behavior |
| --- | --- | --- |
| `memory_curator` | Every 15 minutes, with a 5-minute cooldown | Revalidates curation evidence, then automatically applies policy-valid operations and records effect receipts; administration can pause, quarantine, or roll back outcomes. |
| `session_reflector` | Every 15 minutes, with a 5-minute cooldown | Validates session fact proposals, then automatically materializes policy-valid facts and records applied, rejected, and rollback outcomes. |
| `skill_writer` | Every 60 minutes, after a 15-minute idle window, with a 5-minute cooldown | Validates managed-skill changes, then automatically activates and exports policy-valid skills with materialization receipts; administration can disable, quarantine, or roll back outcomes. |

Scheduling is activity-coupled, not purely wall-clock. The scheduler reads the newest LCM session-message timestamp for the project store as its session-activity signal on every tick:

- `min_idle_secs` is a true idle window: the task only runs after the project has been quiet (no LCM session ingest) for at least that long. A missing session store counts as idle.
- `session_reflector` and `skill_writer` additionally require new session activity since their last successful run; when nothing new landed they skip with `no_new_session_activity` instead of re-reviewing the same transcripts. Because skips do not consume the interval clock, the task fires on the first tick after fresh activity lands (once the idle window is satisfied).
- `memory_curator` reviews the fact store rather than session transcripts, so it keeps the plain interval/cooldown cadence.

The daemon loop is the host for these jobs. It should not create Codex top-level chats for scheduler work, and it should not rely on Codex native recurring automations for liveness. Host backends provide the model call; TraceDecay owns evidence collection, validation, ledgers, apply policy, and scheduler state.

## Standalone And Delegated Modes

`standalone` means TraceDecay owns backend calls, evidence collection, validation, run ledger writes, policy decisions and receipts, dashboard telemetry payloads, and optional scheduler execution. Backend output can propose changes, but TraceDecay validates every proposed mutation before it is automatically applied or materialized.

`delegated_host` means the host owns intelligence and mutation decisions. TraceDecay exposes contracts and storage views, validates proposed operations when asked, and records typed evidence. It must not call its own backend for memory curation, session reflection, or skill writing in this mode.

## Curation Operation Contract

Curation proposals are advisory until TraceDecay validation accepts them. Every proposal must identify the reviewed evidence item it targets, include a supported operation kind, provide a confidence/reason, and pass the existing evidence guard before any apply policy is considered.

Timestamp semantics follow the Hermes memory-curator rule:

1. Prove same subject first.
2. Prove same atomic claim second.
3. Prefer semantic freshness fields such as `asserted_at`, `effective_at`, `observed_at`, `occurred_at`, or `created_at`.
4. Treat maintenance `updated_at` as storage metadata, not truth freshness.
5. Use deterministic tie-breakers only after the subject, claim, and semantic timestamp checks are resolved.

## Managed Skill Contract

TraceDecay-owned managed skills live under the profile `agent_managed/skills` store and static bundled skills stay immutable. Managed skill metadata includes id, title, summary, category, targets, lifecycle state, pinned flag, checksum, timestamps, and provenance. Support files are restricted to `references`, `templates`, `scripts`, and `assets`.

Agent-authored or backend-authored candidates are policy-validated, then
policy-valid changes automatically activate and materialize. CLI and dashboard
surfaces expose receipts and explicit disable, quarantine, and rollback
overrides; they do not gate activation. Pinned and user-authored skills are
excluded from automatic archive or patch recommendations; shipped and
Hermes-owned skills remain outside TraceDecay-owned mutation surfaces.

## Telemetry And Recommendations

Skill telemetry is a sidecar ledger, not frontmatter. The ledger tracks view/use/patch counts, last timestamps, created_by, state, pinned, targets, and provenance. TraceDecay may normalize its own analytics into this ledger. In delegated host mode, TraceDecay reads host usage/provenance data as evidence and does not write host state.

Archive/prune recommendations are explainable review recommendations only. They cannot auto-delete skills. Skill improvement recommendations must cite repeated corrections, failed workflows, underused tool evidence, or validation artifacts before proposing a patch.

## Local Skill Versus Plugin Artifact

Use a local managed skill when the workflow is personal, project-specific, unstable, or still pending validation.

Use a managed overlay when a policy-valid skill should be available to a local native host without changing shipped TraceDecay skills.

Generate a Codex plugin artifact when a policy-valid workflow is stable, shareable, and should travel with plugin metadata, native `skills/`, optional `.mcp.json`, optional hooks, and marketplace metadata.

## Improvement Artifacts

Every automation run that reaches backend validation should be able to produce a review chain:

- traces
- feedback
- generated evals
- validation gate
- optimizer diagnosis
- Codex handoff

The handoff is the durable output for broader code or behavior changes. It must preserve policy-validation decisions and list validation requirements before any generated recommendation is applied.

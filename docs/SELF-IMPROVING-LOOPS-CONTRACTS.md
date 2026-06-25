# Self-Improving Loop Contracts

This document is the durable contract for TraceDecay-owned self-improvement loops. Hermes is a reference implementation and compatibility bridge; it is not required to run TraceDecay curation, managed skills, scheduler jobs, or artifact generation. The first standalone backend is the Codex app-server adapter, and the same contracts are intended to support other delegated or CLI hosts later.

## Host Matrix

| Host | TraceDecay-owned behavior | Host-owned behavior | Skill delivery |
| --- | --- | --- | --- |
| Cursor | Config, ledgers, curation validation, managed skill storage, telemetry sidecars, native overlay export | Native host loading and any host-local transcript signals | Approved managed `SKILL.md` packages under the generated plugin overlay |
| Codex | Config, ledgers, curation validation, managed skill storage, telemetry sidecars, native overlay export, shareable plugin artifact generation | Native plugin discovery, app-server execution when selected as backend | Approved managed `SKILL.md` packages under the Codex plugin overlay or plugin artifact |
| Hermes | Read-only bridge over profile skills, pending approvals, usage, curator/write-approval state, and hosted proposals | Auxiliary LLM calls, background review, `skill_manage`, write approvals, skill mutations, curator decisions | Hermes profile skills remain Hermes-owned |
| Claude Code | Config, ledgers, managed-skill index generation, MCP skill body serving | Prompt-file loading and any host-local execution | Compact `CLAUDE.md` prompt index plus `tracedecay_skill_view` |
| OpenCode | Config, ledgers, managed-skill index generation, MCP skill body serving | Prompt-file loading and any host-local execution | Compact `AGENTS.md` prompt index plus `tracedecay_skill_view` |
| Kimi | Config, ledgers, managed-skill index generation, MCP skill body serving | Prompt-file loading and any host-local execution | Compact prompt index plus `tracedecay_skill_view` |
| Kiro | Config, ledgers, managed-agent prompt-index content, MCP skill body serving | Managed-agent file ownership and host execution | Existing managed-agent path with prompt index plus `tracedecay_skill_view` |
| Prompt-only agents | Config, ledgers, prompt-index generation, MCP skill body serving | Prompt ingestion and execution | Compact prompt index plus `tracedecay_skill_view` |

## Standalone And Delegated Modes

`standalone` means TraceDecay owns backend calls, evidence collection, validation, run ledger writes, approval staging, dashboard review payloads, and optional scheduler execution. Backend output can propose changes, but TraceDecay validates every proposed mutation before it can be applied.

`delegated_host` means the host owns intelligence and mutation decisions. TraceDecay exposes contracts and storage views, validates proposed operations when asked, and records bridge-visible evidence. It must not call its own backend for memory curation, session reflection, or skill writing in this mode. Legacy `hermes_hosted` config is only an alias for `delegated_host`.

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

Agent-authored or backend-authored changes enter pending approval first. Activation, disable, archive, restore, and staged updates are explicit lifecycle operations. Pinned, user-authored, shipped, and Hermes-owned skills are protected from automatic mutation.

## Telemetry And Recommendations

Skill telemetry is a sidecar ledger, not frontmatter. The ledger tracks view/use/patch counts, last timestamps, created_by, state, pinned, targets, and provenance. TraceDecay may normalize its own analytics into this ledger. In delegated Hermes mode, TraceDecay reads Hermes usage/provenance data as bridge evidence and does not write Hermes state.

Archive/prune recommendations are explainable review recommendations only. They cannot auto-delete skills. Skill improvement recommendations must cite repeated corrections, failed workflows, underused tool evidence, or validation artifacts before proposing a patch.

## Local Skill Versus Plugin Artifact

Use a local managed skill when the workflow is personal, project-specific, unstable, or still pending validation.

Use a managed overlay when an approved skill should be available to a local native host without changing shipped TraceDecay skills.

Generate a Codex plugin artifact when an approved workflow is stable, shareable, and should travel with plugin metadata, native `skills/`, optional `.mcp.json`, optional hooks, and marketplace metadata.

## Improvement Artifacts

Every automation run that reaches backend validation should be able to produce a review chain:

- traces
- feedback
- generated evals
- validation gate
- optimizer diagnosis
- Codex handoff

The handoff is the durable output for broader code or behavior changes. It must preserve approval gates and list validation requirements before any generated recommendation is applied.

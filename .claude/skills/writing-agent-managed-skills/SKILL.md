---
name: writing-agent-managed-skills
description: 'TraceDecay Dev: Use when creating, revising, validating, or auditing TraceDecay agent-managed automation skills and automatic skill-writer outcomes.'
---

# TraceDecay Dev: Writing Agent-Managed Skills

Agent-managed skills are profile-owned artifacts produced by automation. The
skill writer validates accepted output, activates it, and materializes it to
supported hosts within the same run. Treat the terminal record, advertised run
artifacts, managed-skill state, and usage telemetry as the public evidence.
Bundled `plugin/skills/` changes remain a separate release path.

## Workflow

1. Inspect the current artifact with `tracedecay_skill_list`, then
   `tracedecay_skill_view` with support files for the exact skill id.
2. Find its writer run with `tracedecay_automation_run_list`, then read the
   exact terminal record with `tracedecay_automation_run_view`.
3. Read only artifact kinds advertised by that record with
   `tracedecay_automation_run_artifact_view`. For a writer run these may include
   `generated_evals`, `validation_gate`, `optimizer_diagnosis`, and
   `codex_handoff`.
4. Verify the automatic outcome: validation evidence must match the terminal
   state, accepted skills must be active and materialized, and rejected output
   must not appear as active managed state.
5. For bundled reusable guidance, edit `plugin/skills/<slug>/SKILL.md` and run
   the agent-suite validation. Do not copy profile-owned state into the bundle.
6. Validate discovery text: the description names concrete triggers, the body
   is host-portable, and support files remain bounded and relevant.
7. Check post-activation adoption through analytics diagnostics and skill usage
   events before declaring the skill effective.

## Quality Bar

| Check | Pass condition |
|---|---|
| Trigger | Another agent can identify when to load it from description alone. |
| Body | Short imperative workflow, no session narrative, no stale environment facts. |
| Evidence | Run artifact or transcript shows the failure this skill prevents. |
| Validation | The terminal record and advertised validation evidence agree, or the bundled skill passes plugin tests. |
| Adoption | Usage telemetry or follow-up session proves it was invoked. |

## Guardrails

- Do not invent a human settlement step after a writer run; validation,
  activation, and materialization are automation-owned.
- Never copy managed-skill state into bundled skills unless the pattern is
  reusable across projects.
- Use direct skill create, update, disable, archive, or restore commands only
  for an exact administrative instruction, independently of writer-run review.
- Do not mutate profile stores from subagents. Subagents may inspect and
  recommend only.

## Deliverable

Return the skill id or bundled path, terminal writer-run status, automatic
activation/materialization state, validation artifacts read, edits made or
recommended, and post-activation adoption evidence.

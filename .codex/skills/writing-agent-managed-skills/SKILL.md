---
name: writing-agent-managed-skills
description: 'Use when creating, revising, validating, approving, installing, or auditing TraceDecay agent-managed automation skills and skill-writer drafts.'
---

# Writing Agent-Managed Skills

Agent-managed skills are profile-owned drafts produced by automation. Treat
them as generated artifacts with lifecycle state, validation evidence, and
usage telemetry; bundled `plugin/skills/` changes are a separate release path.

## Workflow

1. Inspect before editing: `tracedecay_skill_list` for inventory, then
   `tracedecay_skill_view` with support files for the target draft.
2. Read the writer run evidence:
   `tracedecay_automation_run_artifact_view` for `generated_evals`,
   `validation_gate`, `optimizer_diagnosis`, and `codex_handoff` artifacts.
3. Decide the path:
   - managed draft fix: update through `tracedecay automation skills ...`
     commands or dashboard flow;
   - bundled reusable guidance: edit `plugin/skills/<slug>/SKILL.md` and run
     agent-suite validation.
4. Validate discovery text: description starts with `Use when`, names concrete
   triggers, avoids workflow summaries, and stays host-portable.
5. Check adoption after install with analytics diagnostics and skill usage
   events before declaring the skill effective.

## Quality Bar

| Check | Pass condition |
|---|---|
| Trigger | Another agent can identify when to load it from description alone. |
| Body | Short imperative workflow, no session narrative, no stale environment facts. |
| Evidence | Run artifact or transcript shows the failure this skill prevents. |
| Validation | Draft has a validation gate or bundled skill passes plugin tests. |
| Adoption | Usage telemetry or follow-up session proves it was invoked. |

## Guardrails

- Never approve or install a managed skill without explicit user intent.
- Never copy managed-skill state into bundled skills unless the pattern is
  reusable across projects.
- Do not mutate profile stores from subagents. Subagents may inspect and
  recommend only.

## Deliverable

Return the skill id or bundled path, lifecycle state, validation artifacts read,
edits made or recommended, adoption evidence, and the exact approve/install
command when mutation is appropriate.

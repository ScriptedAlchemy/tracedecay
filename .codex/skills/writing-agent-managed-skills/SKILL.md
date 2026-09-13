---
name: writing-agent-managed-skills
description: "Create, revise, or audit TraceDecay managed skills and their writer validation, activation, and materialization."
---

# Writing Agent-Managed Skills

Managed skills are profile-owned artifacts. The writer validates, activates,
and materializes accepted output automatically; bundled `plugin/skills/`
and repository development skills follow their own source release path.

## Choose the relevant path

- For an existing managed skill, use `tracedecay_skill_list`, then
  `tracedecay_skill_view` with support files for its exact id. Inspect writer
  evidence with `tracedecay automation runs list` and
  `tracedecay automation runs view <run_id>`. Read only artifact kinds the
  record advertises, through `tracedecay_automation_run_artifact_view` or
  `tracedecay automation runs artifact <run_id> <kind> --json`.
- For source-owned guidance, edit its source `SKILL.md` and relevant support
  files. Reconcile shared text across repository host copies while preserving
  host metadata. Do not edit hash-tracked managed materializations as source.
- For a requested managed-skill administrative change, use the supported skill
  administration surface for the exact target. A writer-run audit alone does
  not authorize create, update, disable, archive, or restore operations.
  Subagents inspect and recommend; they do not mutate profile stores.

## Write guidance that earns its context

- Give the description a concise capability and precise trigger. Do not require
  a magic opening phrase, attract neighboring tasks, or list every tool.
- Keep only guidance that changes a capable agent's decisions: domain facts,
  useful routing, and real correctness, safety, or authorization constraints.
- Keep shared essentials in `SKILL.md`; link substantial conditional procedures
  or examples from support files and say when to read them. A short skill needs
  no router. Do not load every reference at entry.
- Preserve user scope and existing authorization. Avoid mandatory rituals,
  universal audit lanes, fixed output counts, and premature approval pauses.
  Complete authorized preparation and validation before any required decision.
- Prefer one canonical rule over repeated host copies or conflicting prose.
  Keep necessary host copies aligned; do not copy profile-owned state into a
  bundle merely because it exists.

## Validate and finish

Use the owning validator for structural or packaging changes. For changed
routing or substantial behavior, evaluate a realistic intended task and a
neighboring task that should not trigger it, using the existing neutral routing
evaluator when available. Keep routing success separate from task usefulness;
invocation counts alone prove neither effectiveness nor failure.

For writer review, accepted output must be active and materialized, rejected
output must not be active, and terminal status must agree with advertised
validation and deployment evidence. Do not invent a manual settlement gate.

Report the target, changes or findings, relevant validation, and unresolved
limitations. Include writer state and adoption evidence only when that was
part of the task; future telemetry is not a prerequisite to finishing an edit.

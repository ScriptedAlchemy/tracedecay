---
name: inspecting-managed-skills
description: 'Inspect TraceDecay agent-managed skill proposals, activation state, run evidence, or Hermes bridge health. Bundled skill authoring is a separate workflow.'
---

# Inspecting managed skills

Managed skills are profile-owned runtime artifacts, distinct from bundled plugin
skills. Inspect the exact skill or run through supported automation surfaces.
Artifact views verify advertised hashes; filesystem guesses and raw database
rows are not equivalent evidence.

Separate proposal, validation, activation, deployment, and observed use. A
successful generation run does not prove that a host loaded the skill or that it
helped a task. Inspect terminal effects and operator overrides before explaining
an activation decision, and preserve failed or skipped states.

Hermes uses its standard home integration; do not invent a second home selector
or bridge authority. Inspect the supported bridge state without modifying host
files as a side effect of diagnosis. Current operation schemas define available
controls; this inspection workflow itself is read-only.

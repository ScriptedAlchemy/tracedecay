---
name: introspecting-tracedecay-usage
description: "Audit TraceDecay adoption and usage evidence to identify concrete repository or skill improvements."
---

# Introspecting TraceDecay Usage

Bound the project set, time window, and question. Use evidence to identify a
failure before changing code or guidance. A focused question needs only the
relevant evidence; a requested broad audit can combine the lanes below.

## Choose evidence

- **Adoption:** `tracedecay analytics diagnostics`; add `--all` only for a
  cross-project question. Use `tracedecay:diagnosing-analytics` for interpretation.
- **Session behavior:** start with `tracedecay_message_search`; use
  `tracedecay:managing-session-context` for deeper LCM retrieval or replay.
- **Managed skills and automation:** start with `tracedecay_skill_list`; open
  the exact skill or run implicated by the evidence. Use
  `tracedecay:inspecting-managed-skills` for managed state and the repo-local
  `inspecting-automation-cycles` skill for run outcomes.
- **Prior decisions and memory:** use `tracedecay:project-memory` when recall or
  curation is relevant. A usage audit does not itself require adding facts or
  running curation. Requested curation settles automatically after validation;
  inspect its receipts rather than introducing a manual approval gate.
- **Code structure:** use `tracedecay:code-health` when evidence points to an
  implementation hotspot, rather than running a whole-repo audit by default.

Use supported CLI, MCP, and dashboard analytics. The legacy
`scripts/project-analytics.sh` reads profile databases directly; do not use it
as a fallback. Report unavailable metrics as unavailable, not zero.

## Interpret and improve

High hook volume, low tool usage, or zero skill invocations are candidate
signals. Confirm a missed useful opportunity in task evidence before calling
one an adoption failure. LCM compression and depth are health signals, not
quality scores. `project_id: null` can identify global analytics.

Group related examples, locate their shared cause, and choose the smallest
fix that addresses it: trigger text for misrouting, an exposed diagnostic for
missing evidence, or an implementation change for faulty behavior. An audit
request calls for findings; apply fixes when the user has requested improvement.
Do not turn one anecdote into a universal rule.

Verify changed behavior with the narrowest relevant check. Report supporting
sessions or run ids, the finding, any changes and verification, and limitations.
Include tool-reported `tracedecay_metrics:` savings only when available.

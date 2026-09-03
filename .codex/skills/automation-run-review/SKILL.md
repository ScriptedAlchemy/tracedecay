---
name: automation-run-review
description: "Use when reviewing self-improvement automation run status, validation evidence, automatic skill activation, or memory-curation outcomes."
managed-by: tracedecay-automation
skill-id: "automation-run-review"
content-hash: sha256:62c256a24e756b8b9270ed092477e24daa7d80aa721328de3d7749b8c232aa9d
skill-version: 1783629450
---

# Automation run review

Review automation without mutating it:

1. Use `tracedecay_automation_run_list` to find the exact run id.
2. Use `tracedecay_automation_run_view` to inspect terminal status, counts,
   validation report, effects, and advertised artifact kinds.
3. Use `tracedecay_automation_run_artifact_view` only for an artifact kind
   advertised by that run; verify the returned hash-backed payload against the
   terminal record.
4. For skill-writer runs, verify automatic validation, activation, and
   materialization against `tracedecay_skill_list` and
   `tracedecay_skill_view`. For memory-curator runs, verify committed effects
   with read-only fact queries.
5. Report failures after committed effects as reconciliation work. Never rerun
   blindly or manufacture a manual settlement step.

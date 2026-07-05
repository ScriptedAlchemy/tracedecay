---
name: introspecting-tracedecay-usage
description: Use when turning TraceDecay's own analytics, session history, automation runs, managed skills, memory facts, diagnostics, or code-health signals into repo improvements, evals, or bundled skill updates.
---

# Introspecting TraceDecay Usage

Use this skill for TraceDecay self-improvement driven by real agent behavior,
not guesswork. Keep the pass evidence-first: logs and summaries identify the
failure mode, then code, tests, or bundled skills encode the fix.

## Required Audit Lanes

1. **Adoption and telemetry:** Use `tracedecay:diagnosing-analytics`. Run
   `tracedecay analytics diagnostics` (add `--all` when the question is
   cross-project). Compare hook/tool volume with TraceDecay tool usage.
2. **Past-session behavior:** Use `tracedecay:managing-session-context`.
   Start with `tracedecay_message_search`; use `tracedecay_lcm_status` before
   deeper LCM work, then `tracedecay_lcm_grep` or replay only as needed.
3. **Managed skills and automation:** Use
   `tracedecay:inspecting-managed-skills`. Start with
   `tracedecay_skill_list`; view skills or run artifacts only when the list
   points to a specific stale, patched, failed, or unused item.
4. **Durable memory:** Use `tracedecay:project-memory`. Search with
   `tracedecay_fact_store` before re-deriving project decisions. For curation,
   run read-only inventory and dry-runs first; never remove facts without
   explicit approval.
5. **Code-health and implementation target:** Use `tracedecay:code-health`.
   Let weak dimensions and hotspots decide whether the fix is code, tests,
   docs, evals, or skill guidance.

## Interpretation Rules

- High hook volume with low TraceDecay tool usage is an adoption gap. Improve
  trigger text, tool hints, or eval coverage before blaming users.
- Managed skills with many patches and zero successful uses are validation
  failures. Inspect their evidence and either tighten the managed-skill loop
  or add a bundled operator skill that prevents the repeated mistake.
- LCM depth and compression ratios are health signals, not direct quality
  scores. Use raw/session recall to confirm what agents actually missed.
- Memory curation candidates are proposals, not permission. Prefer merge or
  update when provenance matters; deletion needs fresh user approval.
- Analytics fields can be global. `project_id: null` means the run was global,
  not broken.

## Improvement Loop

1. Capture counts and examples from every required audit lane.
2. Group evidence into one failure class: discovery, fallback, diagnostics
   interpretation, automation validation, memory curation, or code structure.
3. Choose the smallest durable fix:
   - bundled skill update when agents know the feature but skip the workflow;
   - eval/test when the workflow should be enforced;
   - code change when the data is unavailable, misleading, or too hard to
     interpret;
   - managed-skill action when the issue lives only in the profile store.
4. Verify with the narrowest relevant command, then run the matching skill
   contract or code test.

## Deliverable

Report the exact commands/tools used, headline counts, cited sessions or run
ids when available, the selected failure class, the improvement made, and the
verification result. Include any `tracedecay_metrics:` savings line.

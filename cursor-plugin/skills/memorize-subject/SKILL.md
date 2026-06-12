---
name: memorize-subject
description: Research a subject with parallel read-only agents, then store durable facts in TraceDecay memory.
disable-model-invocation: true
---

# /memorize-subject

Apply the `tracedecay:memorizing-subject` skill.

- **Args:** interpret the text after the command as the subject, topic, code area, branch, PR, or scope to memorize.
- If no subject was given, ask for one before doing any research.
- Follow that skill's workflow: fan out read-only research, collect candidate durable facts with citations, dedupe via `tracedecay_fact_store`, then store only accepted facts.
- Calibrate `trust` per that skill's tiers (≥0.85 verified / ~0.7 ordinary / ~0.5 unsure) — avoid defaulting everything to high trust — and act on each add's `diff` report (`near_duplicate` / `possible_conflict` / `rejected_secret_like`).

Output: facts stored with trust, duplicates skipped, rejected candidates, and any low-trust facts stored for later curation.

---
name: memorize-subject
description: Research a subject with parallel read-only agents, then store durable facts in tokensave memory.
disable-model-invocation: true
---

# /memorize-subject

Apply the `tokensave:memorizing-subject` skill.

- **Args:** interpret the text after the command as the subject, topic, code area, branch, PR, or scope to memorize.
- If no subject was given, ask for one before doing any research.
- Follow that skill's workflow: fan out read-only research, collect candidate durable facts with citations, dedupe via `tokensave_fact_store`, then store only accepted facts.

Output: facts stored, duplicates skipped, and uncertain candidates that need user approval.

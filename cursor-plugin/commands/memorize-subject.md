---
name: memorize-subject
description: Research a subject with parallel read-only agents, then store durable facts in tokensave memory.
---

# /memorize-subject

Apply the `tokensave:memorizing-subject` skill.

- **Args:** interpret `$ARGUMENTS` as the subject, topic, code area, branch, PR, or scope to memorize.
- If `$ARGUMENTS` is absent, ask for the subject before doing any research.
- Follow that skill's workflow: fan out read-only research, collect candidate durable facts with citations, dedupe via `tokensave_fact_store`, then store only accepted facts.

Output: facts stored, duplicates skipped, and uncertain candidates that need user approval.

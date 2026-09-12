---
name: tracedecay-recall-memory
description: Recall prior decisions, durable facts, and past session conversations for this project.
---

# /tracedecay-recall-memory

Use `tracedecay:project-memory`; for raw conversation recall, use `tracedecay:managing-session-context`.

Interpret `$ARGUMENTS` as the topic to recall; if absent, ask what to find.
Search durable facts for retained knowledge and session history for exact prior
conversation. Preserve fact trust and provenance, session ids, timestamps, and
coverage limits. Stay read-only unless the user supplies fact feedback; route
fact updates, supersession, or removal to `/tracedecay-curate-memory`.

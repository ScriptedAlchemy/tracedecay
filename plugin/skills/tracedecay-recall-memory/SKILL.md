---
name: tracedecay-recall-memory
description: 'Use to recall prior decisions, durable facts, and past session conversations for this project.'
---

# Recall memory

Use to recall prior decisions, durable facts, or past session conversations for this project.

Route durable decisions/facts through the `tracedecay:project-memory` skill, and raw conversation recall through the `tracedecay:recalling-session-context` skill.

- **Target:** the question or topic to recall. If none is given, ask what to look up.
- Route durable decisions/facts through `fact_store` search; route "what happened in that session" through `tracedecay_message_search` and the LCM retrieval ladder. Stay read-only.
- If the user asks to update, delete, merge, or prune stored facts, switch to `tracedecay:project-memory`.

Output: the recalled decisions/messages with their sources (fact, session id, timestamp).

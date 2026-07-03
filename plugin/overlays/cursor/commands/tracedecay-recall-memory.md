---
description: Recall prior decisions, durable facts, and past session conversations for this project.
---

# /tracedecay-recall-memory

Apply the `tracedecay:project-memory` skill, and for raw conversation recall the `tracedecay:recalling-session-context` skill.

- **Args:** interpret `$ARGUMENTS` as the question or topic to recall; if absent, ask what to look up.
- Route durable decisions/facts through `fact_store` search; route "what happened in that session" through `tracedecay_message_search` and the LCM retrieval ladder. Follow both skills' read-only guardrails.
- If the user asks to update, delete, merge, or prune stored facts, switch to `/tracedecay-curate-memory` / `tracedecay:project-memory`.

Output: the recalled decisions/messages with their sources (fact, session id, timestamp).

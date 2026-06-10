---
name: tokensave-recall
description: Recall prior decisions, durable facts, and past session conversations for this project.
disable-model-invocation: true
---

# /tokensave-recall

Apply the `tokensave:recalling-project-memory` skill, and for raw conversation recall the `tokensave:recalling-session-context` skill.

- **Args:** interpret the text after the command as the question or topic to recall; if absent, ask what to look up.
- Route durable decisions/facts through `fact_store` search; route "what happened in that session" through `tokensave_message_search` and the LCM retrieval ladder. Follow both skills' read-only guardrails.

Output: the recalled decisions/messages with their sources (fact, session id, timestamp).

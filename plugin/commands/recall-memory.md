---
description: Recall prior decisions, durable facts, and past session conversations for this project.
argument-hint: "[subject]"
---

# Recall memory

Interpret `$ARGUMENTS` as the topic to recall; if absent, ask what to find.
Follow `project-memory` for durable facts and `managing-session-context` for raw
conversation history. Search only the registered project in scope and retrieve
enough evidence to answer the question, preserving trust, provenance, session
ids, timestamps, and coverage limits.

This command is read-only unless the user explicitly supplies fact feedback.
Route requests to update, supersede, or remove facts to `curate-memory`.

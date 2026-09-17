# Codex hook-input fixtures

`current-user-message-shapes.jsonl` preserves the field names, envelope order,
content item kinds, and timing of one adjacent `response_item.message(user)` /
`event_msg.item_completed(UserMessage)` pair observed in a recent Codex rollout.
Transcript text is replaced and native identifiers are one-way anonymized.

No goal-context wrapper was present in the sampled current-message pairs. Goal
wrapper examples therefore remain explicit parser-contract cases in Rust tests,
not evidence about the frequency or ordering of current Codex events.

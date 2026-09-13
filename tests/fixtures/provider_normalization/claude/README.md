# Claude provider-normalization golden inputs

`assistant_tool_use.input.json` matches the real Claude Code transcript shape
already used by `tests/transcript_ingest_suite/claude.rs::write_claude_transcript`
(type/sessionId/uuid/message.id/content[] with text + tool_use).

`assistant_thinking_text_tool_use.input.json` is a payload-redacted production
record whose observed block order is `thinking`, `text`, `tool_use`. Provider
keys and nesting are preserved; authored text, reasoning, signature, tool ID,
and arguments are fixture-safe replacements.

`compact_summary_pair.{boundary,summary}.input.json` match the real Claude Code
compact pair shape: a complete `system/compact_boundary` with
`compactMetadata.preservedSegment.anchorUuid`, followed by a synthetic `user`
record with `isCompactSummary:true`, `isVisibleInTranscriptOnly:true`, and the
documented continuation wrapper around the summary body. These fixtures exercise
strict provider-summary pair/envelope extraction only.

Claude's production observation path parses each native JSONL record once with
`parse_normalized_observation_record_v1`, normalizes it through
`sessions::claude::canonical`, and sanitizes the resulting
`CanonicalObservationEnvelopeV1`. The golden assertions exercise that same
production boundary.

## Protocol gaps (intentional)

- **UnknownVersion:** Claude transcript JSONL is unversioned at the record
  schema layer in this tree (no checked-in unsupported-version contract). Do
  not invent `ObservationCoverageReason::UnknownVersion` fixtures.
- **Canonical envelopes:** expected-envelope goldens must use the production
  `sessions::claude::canonical` path; do not hand-build lookalike envelopes.
- **IdentityCollision via JSONL rewrite:** observation identity includes
  `file_generation` + byte range. Production redelivery covers ExactDuplicate
  no-overwrite; store-layer tests cover typed IdentityCollision. Do not forge
  same-generation/same-range collisions outside the parser.
- **Codex plaintext:** Codex empty/encrypted compaction remains ineligible;
  these Claude fixtures must not be reused to invent Codex plaintext.

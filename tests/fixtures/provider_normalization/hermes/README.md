# Hermes provider-normalization fixtures

These inputs preserve the native Hermes SQLite `messages` row fields used by
the production reader. Payload text, identifiers, and tool arguments are
fixture-safe replacements.

- `assistant_tool_call.input.json` is an empty-authored-content assistant row
  with native `tool_calls`.
- `assistant_reasoning.input.json` is an empty-authored-content assistant row
  with native `reasoning`.

Tests must materialize these fields into the SQLite schema and ingest through
`native_observation_record` and `normalize_native_observation`; they must not
construct canonical facts directly.

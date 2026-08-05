# Cursor Composer provider-normalization fixtures

These generated records exercise the bounded `composerData`/bubble fields
currently consumed by the adapter. They are behavior samples, not captured
Cursor Desktop records or evidence of the provider database schema.

`envelope_todos.input.json` covers todo `id`, `content`, and `status` in array
order. Its `lastUpdatedAt` is explicitly `null`, so tests use an ordered
content fingerprint as the mutable-envelope checkpoint and do not infer
revision semantics.

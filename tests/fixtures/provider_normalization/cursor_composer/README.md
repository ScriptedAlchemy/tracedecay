# Cursor Composer provider-normalization fixtures

These records preserve Cursor Composer `composerData`/bubble field names and
nesting while replacing user payload values.

`envelope_todos.input.json` has the observed native todo fields (`id`,
`content`, `status`) in provider array order. Its `lastUpdatedAt` is explicitly
`null`, so tests and production code use an ordered content fingerprint as the
mutable-envelope checkpoint and do not infer revision semantics.

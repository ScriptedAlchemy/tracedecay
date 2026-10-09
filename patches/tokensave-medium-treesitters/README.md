# tokensave-medium-treesitters (TraceDecay path patch)

Path-patched `tokensave-medium-treesitters` 0.2.0 that omits
`tree-sitter-kotlin-sg`. TraceDecay registers Kotlin from `arborium-kotlin`
directly in every tier so the
shipping binary links a single Kotlin parse table.

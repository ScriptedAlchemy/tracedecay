# tokensave-medium-treesitters (TraceDecay path patch)

Path-patched `tokensave-medium-treesitters` 0.2.0 that omits
`tree-sitter-kotlin-sg`. TraceDecay registers Kotlin from `arborium-kotlin`
directly in every tier so the
shipping binary links a single Kotlin parse table.

The lite tier's grammars are depended on directly instead of through
`tokensave-lite-treesitters` 0.2.0, which pins `tree-sitter` 0.26. The bundle
has no `tree-sitter` dependency, so the workspace picks the runtime version.

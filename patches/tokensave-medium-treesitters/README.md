# tokensave-medium-treesitters (TraceDecay path patch)

This local patch to `tokensave-medium-treesitters` 0.2.0 omits
`tree-sitter-kotlin-sg`. TraceDecay registers Kotlin from `arborium-kotlin`
in every tier so the binary links a single Kotlin parse table.

The bundle depends directly on the lite tier's grammars to avoid
`tokensave-lite-treesitters` 0.2.0, which pins `tree-sitter` 0.26. The bundle
has no `tree-sitter` dependency, so the workspace picks the runtime version.

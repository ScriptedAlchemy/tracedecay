# TraceDecay tree-sitter-rust compatibility wrapper

This workspace-only crate preserves the upstream `tree-sitter-rust` package
name and API for dependencies resolved through the root `[patch.crates-io]`.

The canonical patched generated grammar, scanner, queries, and license ship in
`crates/tracedecay-code-extraction/vendor/tree-sitter-rust`. Keeping those
assets in the published extraction crate makes that package self-contained;
this wrapper compiles and exposes the same files for workspace consumers.

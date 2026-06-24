# Contributing to tracedecay

Thanks for your interest in contributing! This guide covers everything you need to get started.

## Getting Started

```bash
git clone https://github.com/ScriptedAlchemy/tracedecay.git
cd tracedecay
cargo build
cargo nextest run --workspace --no-fail-fast
```

Requires **Rust 1.70+** (edition 2021).

## Project Structure

```
src/
  extraction/    Language-specific extractors (tree-sitter based)
  db/            Database layer (libSQL)
  graph/         Knowledge graph queries and traversal
  mcp/           MCP server (tools + handlers)
  context/       Context builder for AI-ready output
  resolution/    Cross-file reference resolution
  sync.rs        Incremental sync engine
  main.rs        CLI entry point
tests/           Integration tests (one per module/language)
tests/fixtures/  Sample source files for extraction tests
vendor/          Vendored tree-sitter grammars
docs/            Design docs and guides
```

## Feature Flags

tracedecay supports 31 languages via feature flags:

| Feature | Languages |
|---------|-----------|
| `lite` (default subset) | Rust, Go, Java, Scala, TypeScript/JS, Python, C, C++, Kotlin, C#, Swift |
| `medium` | +Dart, Pascal, PHP, Ruby, Bash, Protobuf, PowerShell, Nix, VB.NET |
| `full` (default) | +Lua, Zig, Obj-C, Perl, Batch, Fortran, COBOL, MSBasic2, GW-BASIC, QBasic |

Build with fewer languages for faster compile times during development:

```bash
cargo build --no-default-features --features lite
cargo nextest run --no-default-features --features lite
```

## Making Changes

1. **Fork and branch** from `master` for stable changes, `beta` for experimental features.
2. **Write tests.** Every extraction change should have a corresponding test in `tests/`. Follow the existing pattern: create a fixture in `tests/fixtures/` and assert on extracted nodes/edges.
3. **Run the full test suite** before submitting:
   ```bash
   cargo nextest run --workspace --no-fail-fast
   ```
4. **Format your code** with the standard Rust toolchain:
   ```bash
   cargo fmt
   cargo clippy --workspace --all-targets
   ```

### Clippy policy

The CI `Clippy` job runs the same command contributors should run locally before
pushing:

```bash
cargo clippy --workspace --all-targets
```

This check is blocking in CI: the workflow fails if `cargo clippy --workspace
--all-targets` exits non-zero. The crate-level lint policy in `src/lib.rs`
currently denies `clippy::all`, `clippy::unwrap_used`, and
`clippy::expect_used`; new violations of those lints must be fixed or justified
with the narrowest practical `#[allow(...)]` at the affected item. Do not add a
broad allow or weaken the crate policy just to get CI green.

`clippy::pedantic` remains advisory. Pedantic diagnostics are emitted as
warnings and should be addressed when they point to a real maintainability issue,
but they do not block CI unless a future policy change promotes a specific lint
to `deny`.

There is no separate Clippy baseline file today. If a policy change intentionally
promotes additional advisory lints to blocking, update `src/lib.rs`, fix or
narrowly allow the existing violations in the same change, and update this
section so the contributor command and blocking/advisory split still match CI.

## Adding a New Language Extractor

1. Add a tree-sitter grammar dependency (or vendor it under `vendor/`).
2. Create `src/extraction/{lang}_extractor.rs` implementing the `Extractor` trait.
3. Register it in the `LanguageRegistry` with a feature flag (e.g., `lang-{name}`).
4. Add a fixture file `tests/fixtures/sample.{ext}` and a test file `tests/{lang}_extraction_test.rs`.
5. Update the feature flag tables in `Cargo.toml` and this document.

## Running Specific Tests

```bash
# All tests for a specific language
cargo nextest run --test rust_extraction_test

# A single test by name
cargo nextest run test_find_stale_files

# Only sync-related tests
cargo nextest run sync
```

## Commit Messages

Follow conventional commit style:

```
fix: handle UTF-16 encoded files in sync
feat: add Dart annotation extraction
refactor: simplify reference resolver lookup
```

Keep the first line under 72 characters. Add a body explaining *why* if the change isn't obvious.

CI validates commit subjects with:

```bash
scripts/check-conventional-commits.sh origin/master..HEAD
```

## Pull Requests

- Target `master` for bug fixes and stable features.
- Target `beta` for experimental or breaking changes.
- Keep PRs focused — one logical change per PR.
- Include test coverage for new behavior.
- Update `CHANGELOG.md` under an `[Unreleased]` section.

## Reporting Issues

Open an issue at https://github.com/ScriptedAlchemy/tracedecay/issues with:

- tracedecay version (`tracedecay --version`)
- OS and architecture
- Steps to reproduce
- Expected vs. actual behavior

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). Be respectful and constructive.

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).

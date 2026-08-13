# Contributing to tracedecay

Thanks for your interest in contributing! This guide covers everything you need to get started.

## Getting Started

```bash
git clone https://github.com/ScriptedAlchemy/tracedecay.git
cd tracedecay
cargo build
cargo nextest run --workspace --all-features --no-fail-fast
```

Requires **Rust 1.85+** (edition 2024) and **Node.js 22+ with npm**.

The dashboard bundle at `dashboard/app-dist/` is generated output and is
git-ignored, so a fresh clone has none. `build.rs` therefore runs `npm ci`
(when `dashboard/node_modules` is absent) and `npm run build` before embedding
the UI, and fails the Rust build if npm is missing. `TRACEDECAY_SKIP_DASHBOARD_BUILD`
only helps when an `app-dist` already exists but is stale — in a fresh clone it
skips the build and then trips the "`dashboard/app-dist/index.html` is missing
after build" assertion. CI builds the bundle once in the `dashboard-assets`
job and every Rust job downloads it as an artifact. GitHub Releases ship
prebuilt binaries; workspace Cargo packages are private.

The full `cargo nextest run --workspace --all-features` suite has not yet had a
clean end-to-end run in this checkout. Run it and read the failures rather than
assuming a green baseline; treat new failures in code you touched as yours.

## Final V2 storage

Read [the V2 operating-model summary](docs/V2-OPERATING-MODEL.md) before
changing storage, retrieval, or host ingestion, then follow the linked
authoritative roadmap plans. `tracedecay-graph-db` is the sole final Grafeo
boundary; SQLite is relational only. V2 persisted data is reset or recreated
when incompatible—do not add a prior-store reader, conversion, backfill,
shadow path, or dual write.

Tests and local validation must use isolated temporary home, profile, project,
and socket paths. Never start, install, or test a V2 daemon against an
installed `master` profile.

## Source tree reference

This tree map is for source orientation only. The
[V2 roadmap](docs/plans/tracedecay-v2/00-plan-set-index.md) owns product
precedence and acceptance.

```
src/
  extraction/    Language-specific extractors (tree-sitter based)
  db/            Database abstraction over the rusqlite runtime
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

tracedecay supports more than 50 languages. `Cargo.toml` is the source of truth
for the exact feature membership:

| Feature | Coverage |
|---------|----------|
| `lite` | Core extractors such as Rust, Go, Java, TypeScript/JS, Python, C/C++, Kotlin, C#, and Swift |
| `medium` | `lite` plus Dart, Pascal, PHP, Ruby, Bash, Protobuf, PowerShell, Nix, and VB.NET |
| `full` (default) | `medium` plus all remaining `lang-*` features listed in `Cargo.toml` |

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
   cargo nextest run --workspace --all-features --no-fail-fast
   ```
   Cargo-launched test processes are isolated from your real `~/.tracedecay`
   profile: `.cargo/config.toml` pins `TRACEDECAY_DATA_DIR` to
   `target/test-profile/.tracedecay` (enforced by
   `tests/core_cli_suite/test_profile_isolation_test.rs`). Tests that need a private profile
   should still override it per-test, e.g. via
   `common::TraceDecayStorageEnvGuard` or `common::apply_tracedecay_home_env`.
4. **Format your code** with the standard Rust toolchain:
   ```bash
   cargo fmt
   cargo clippy --workspace --all-targets
   ```

### Public wire compatibility changes

For a public name or wire protocol, retain compatibility only with evidence
that the external contract shipped. Persisted V2 storage has no compatibility
conversion. Keep compatibility work out of storage; document the retained
external boundary alongside its behavior.

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
4. Add a fixture file `tests/fixtures/sample.{ext}` and a test module `tests/extraction_suite/{lang}.rs`, then register it with a `mod {lang};` declaration in `tests/extraction_suite/main.rs`.
5. Update the feature flag tables in `Cargo.toml` and this document.

## Validating Plugins and Skills

Changes under `plugin/` or `src/agents/` are covered
by a layered validation system: vendored JSON-schema checks, per-host skill
frontmatter contracts, cross-bundle sync/parity tests, and a CI
schema-validation workflow. `plugin/skills/` is the shared source of truth for
bundled skills — do not fork host-specific copies. Before submitting, run:

```bash
cargo nextest run -E 'binary(=agent_suite)'
```

See [`docs/PLUGIN-VALIDATION.md`](docs/PLUGIN-VALIDATION.md) for the full
layer breakdown, schema refresh procedure, and how to add a skill or a new
ecosystem bundle correctly.

## Changing the Rust-to-Dashboard Wire Contract

`dashboard/src/contracts/generated.ts`, `dashboard/src/contracts/index.ts`, and
`dashboard/codegen/schemas/dashboard-contracts.schema.json` are generated, not
hand-written. The Rust `schemars` output is authoritative: the codegen CLI
shells out to `cargo test --test dashboard_contract_schema_export -- --ignored
writes_dashboard_contract_schema`, regenerates all three files, and compares
them byte-for-byte with what is committed.

After changing any Rust type that crosses the dashboard API boundary:

```bash
cd dashboard
npm run contracts:generate   # rewrite the generated files
npm run contracts:check      # what CI runs; exits 1 on any drift
```

`contracts:check` is a blocking CI step in the `dashboard` job of `ci.yml`, not
an advisory one. Do not hand-edit the generated files — the check will fail and
the fix is to regenerate and commit.

The generated contract files (and the embedded Cursor extension bundle) are
marked `linguist-generated` in `.gitattributes`, so GitHub collapses them in
PR diffs and excludes them from language stats; review the Rust source of the
contract change instead.

## Running Specific Tests

```bash
# All extractor tests for a specific language (module inside the
# consolidated extraction_suite binary)
cargo nextest run -E 'binary(=extraction_suite) and test(/^rust::/)'

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

Install the local `commit-msg` hook once per checkout:

```bash
scripts/install-git-hooks.sh
```

CI validates commit messages with commitlint (`commitlint.config.cjs`):

```bash
git show --no-patch --format=%B HEAD | npm run --silent lint:commit --
```

Run the same command locally (per commit) before pushing to lint every
non-merge commit in a branch range. Merge commits are skipped to match CI
behavior.

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

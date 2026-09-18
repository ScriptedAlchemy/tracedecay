# Contributing to tracedecay

Thanks for your interest in contributing! This guide covers everything you need to get started.

## Getting Started

```bash
git clone https://github.com/ScriptedAlchemy/tracedecay.git
cd tracedecay
cargo build -p tracedecay-cli
cargo test-ci
```

Use the Rust toolchain pinned in `rust-toolchain.toml` (edition 2024) and
**Node.js 22+ with npm**. Install `cargo-nextest` to run the `test-ci` and
`test-all` aliases defined in `.cargo/config.toml`. Commands below run from the
repository root unless noted.

Use your current checkout; no particular absolute path or historical PR branch
is required. See [AGENTS.md](AGENTS.md) for checkout safety and shared-work rules.

The dashboard bundle at `dashboard/app-dist/` is generated output and is
git-ignored, so a fresh clone has none. The CLI build script
(`crates/tracedecay-cli/build.rs`), the only crate that embeds the bundle,
builds the frontend into an immutable, digest-named copy under its own
`OUT_DIR` rather than embedding `app-dist`: `npm ci` runs when
`dashboard/node_modules` lacks the marker for the current `package-lock.json`,
`npm run build` runs when the frontend inputs' content fingerprint changes,
and the Rust build fails if npm is missing. Setting
`TRACEDECAY_SKIP_DASHBOARD_BUILD` stages a prebuilt `dashboard/app-dist`
instead, and only when `TRACEDECAY_DASHBOARD_BUNDLE_SHA256` holds that
bundle's digest (print it with `python3 scripts/check-dashboard-bundle.py
dashboard/app-dist --print-digest`); a missing or mismatched digest fails the
build. CI builds the bundle once in the `dashboard-assets` job and every Rust
job downloads it as an artifact. GitHub Releases ship prebuilt binaries;
workspace Cargo packages are private.

Do not assume a green baseline for the full suite: run it and read the
failures; treat new failures in code you touched as yours.

## Final V2 storage

Read [the V2 operating-model summary](docs/V2-OPERATING-MODEL.md) before
changing storage, retrieval, or host ingestion, then follow the linked
authoritative roadmap plans. `tracedecay-graph-db` is the sole final Grafeo
boundary; SQLite is relational only. V2 persisted data is reset or recreated
when incompatible, do not add a prior-store reader, conversion, backfill,
shadow path, or dual write.

Tests and local validation must use isolated temporary home, profile, project,
and socket paths. Never start, install, or test a V2 daemon against an
installed `master` profile.

## Source tree reference

This tree map is for source orientation only. The
[V2 roadmap](docs/plans/tracedecay-v2/00-plan-set-index.md) owns product
precedence and acceptance.

```
crates/tracedecay/     Composition-root library and its integration suites
crates/tracedecay-cli/ Shipped tracedecay binary and CLI integration suites
crates/               Workspace members (code-extraction, graph-db, domain, hosts, …)
dashboard/            Embedded React dashboard
plugin/               Host bundles (Claude, Codex, Cursor, Kimi, OpenCode)
tests/                Shared fixtures, distribution suites, and cross-crate gates
docs/                 Design docs and the V2 roadmap
```

## Feature Flags

tracedecay supports more than 50 languages. The root `Cargo.toml` is a virtual
workspace, not a package. `crates/tracedecay-cli/Cargo.toml` and
`crates/tracedecay/Cargo.toml` expose the language tiers;
`crates/tracedecay-code-extraction/Cargo.toml` owns grammar feature membership:

| Feature | Coverage |
|---------|----------|
| `lite` | Core extractors such as Rust, Go, Java, TypeScript/JS, Python, C/C++, Kotlin, C#, and Swift |
| `medium` | `lite` plus Dart, Pascal, PHP, Ruby, Bash, Protobuf, PowerShell, Nix, and VB.NET |
| `full` (default) | `medium` plus the remaining grammars selected by the extractor crate's `full` feature |

Build with fewer languages for faster compile times during development:

```bash
cargo build -p tracedecay-cli --no-default-features --features lite
cargo nextest run -p tracedecay-code-extraction --no-default-features --features lite
```

## Making Changes

1. **Use the task's branch**, or branch from `master` for a new contribution. Confirm the target before working on a release-channel branch.
2. **Write tests in the owning crate.** Extraction changes belong with `crates/tracedecay-code-extraction/`; follow nearby inline tests or crate-local integration suites and assert on extracted nodes/edges.
3. **Run focused tests** for the affected behavior, then broaden for unresolved risk. Documentation-only edits do not require application builds. For the hosted acceptance selection:
   ```bash
   cargo test-ci
   ```
   Cargo-launched test processes are isolated from your real `~/.tracedecay`
   profile: `.cargo/config.toml` pins `TRACEDECAY_DATA_DIR` to
   `target/test-profile/.tracedecay` (enforced by
   `crates/tracedecay-cli/tests/core_cli_suite/test_profile_isolation_test.rs`). Tests that need a private profile
   should still override it per-test, e.g. via
   `common::TraceDecayStorageEnvGuard` or `common::apply_tracedecay_home_env`.
   `cargo test-all` additionally enables every optional feature; it is broader
   than the hosted selection. Check each suite's `required-features` before
   selecting it and confirm that the run executed tests rather than matching zero.
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
--all-targets` exits non-zero. The composition-root lint policy in
`crates/tracedecay/src/lib.rs` currently denies `clippy::all`, `clippy::unwrap_used`, and
`clippy::expect_used`; new violations of those lints must be fixed or justified
with the narrowest practical `#[allow(...)]` at the affected item. Do not add a
broad allow or weaken the crate policy just to get CI green.

`clippy::pedantic` remains advisory. Pedantic diagnostics are emitted as
warnings and should be addressed when they point to a real maintainability issue,
but they do not block CI unless a future policy change promotes a specific lint
to `deny`.

There is no separate Clippy baseline file today. If a policy change intentionally
promotes additional advisory lints to blocking, update the owning crate's lint
policy, fix or narrowly allow the existing violations in the same change, and update this
section so the contributor command and blocking/advisory split still match CI.

## Adding a New Language Extractor

1. Add a tree-sitter grammar dependency (or vendor it under `vendor/`).
2. Create `crates/tracedecay-code-extraction/src/{lang}_extractor.rs` implementing the `LanguageExtractor` trait.
3. Register it in `LanguageRegistry` with a feature flag (e.g., `lang-{name}`) in that crate's `lib.rs` and `Cargo.toml`.
4. Add a test module under `crates/tracedecay-code-extraction/tests/main/` and register it in that directory's `main.rs`; follow nearby source and fixture patterns.
5. Update the feature flag tables in that crate's `Cargo.toml` and this document.

When sharing traversal helpers, preserve child ordering, named versus anonymous
node handling, and direct-child versus descendant semantics. Compare against
`crates/tracedecay-code-extraction/src/traversal.rs` and keep language-specific
behavior local; cover the consuming extractor before and after consolidation.

## Validating Plugins and Skills

Changes under `plugin/` or `crates/tracedecay-agent-hosts/` are covered
by a layered validation system: vendored JSON-schema checks, per-host skill
frontmatter contracts, cross-bundle sync/parity tests, and a CI
schema-validation workflow. `plugin/skills/` is the shared source of truth for
bundled skills, do not fork host-specific copies. Before submitting, run:

```bash
cargo nextest run -p tracedecay --features test-helpers --test agent_suite
```

See [`docs/PLUGIN-VALIDATION.md`](docs/PLUGIN-VALIDATION.md) for the full
layer breakdown, schema refresh procedure, and how to add a skill or a new
ecosystem bundle correctly.

## Changing the Rust-to-Dashboard Wire Contract

`dashboard/src/contracts/generated.ts`, `dashboard/src/contracts/index.ts`, and
`dashboard/codegen/schemas/dashboard-contracts.schema.json` are generated, not
hand-written. The Rust `schemars` output is authoritative: the codegen CLI
exports the schema through the `tracedecay-dashboard-api` library's ignored
`contract_schema::tests::writes_dashboard_contract_schema` test, regenerates all
three files, and compares them byte-for-byte with what is committed.

After changing any Rust type that crosses the dashboard API boundary:

```bash
cd dashboard
npm run contracts:generate   # rewrite the generated files
npm run contracts:check      # what CI runs; exits 1 on any drift
```

`contracts:check` is a blocking CI step in the `dashboard` job of `ci.yml`, not
an advisory one. Do not hand-edit the generated files. The check will fail and
the fix is to regenerate and commit.

The generated contract files (and the embedded Cursor extension bundle) are
marked `linguist-generated` in `.gitattributes`, so GitHub collapses them in
PR diffs and excludes them from language stats; review the Rust source of the
contract change instead.

## Running Specific Tests

```bash
# All extractor tests for a specific language
cargo nextest run -p tracedecay-code-extraction --test main -E 'test(/^rust::/)'

# A single test by name
cargo nextest run -p tracedecay-code-extraction --test main test_rust_file_node_is_root

# Only sync-related tests
cargo nextest run -p tracedecay --features test-helpers sync
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
- Confirm the target branch with the maintainer for release-channel work; do not
  infer it from an archived plan or PR number.
- Keep PRs focused. One logical change per PR.
- Include test coverage for new behavior.
- Do not hand-edit `CHANGELOG.md`; release automation generates it from
  conventional commit messages.

### What CI spends on a pull request

CI runs on GitHub's free hosted runners: 20 concurrent jobs for the whole
account, 5 of them macOS. Every job a pull request queues is a job the
integration branch waits behind, so a run spends only what its state earns:

| State | Runs |
|---|---|
| Pull request | Nothing automatically. Dispatch CI on its branch ref when the head is ready. |
| `CI` dispatch | Repository gates, benchmark-harness self-tests, and the Linux lane: build, clippy, feature gates, dashboard, and Linux test partitions. |
| `CI` with `run_os=true` | Adds the macOS and Windows matrices. |
| `CI` with `run_hosts=true` | Adds stock host integrations. |
| `CI` with `run_perf=true` | Adds hotpath parity. |
| Hotpath workflow dispatch | Runs the selected profile, coverage, or runtime-core suite. |
| Push to `master` | Everything. |

Run `gh workflow run ci.yml --ref <branch>` when a PR head is ready. Add
`-f run_os=true`, `-f run_hosts=true`, or `-f run_perf=true` only for those
lanes. Opening, pushing, labeling, and marking ready create no run at all. A
newer master push cancels the one in flight. Closing or merging a PR cancels
its remaining runs and drops its Actions caches. Nothing runs on a
timer: the packaged-crate distribution battery and the Hawk lint are
`workflow_dispatch` only.

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

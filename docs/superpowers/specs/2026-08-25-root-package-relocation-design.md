# TraceDecay Root Package Relocation Design

## Status

Proposed for the draft pull request. This design relocates the existing
`tracedecay` library package without changing product behavior or using the
move as evidence that additional runtime code is dead.

## Goal

Make the repository root a virtual Cargo workspace. The workspace package
layout becomes explicit:

- `crates/tracedecay-cli` owns the `tracedecay` executable and command-line
  parsing.
- `crates/tracedecay` owns the daemon, MCP server, dashboard embedding, runtime
  composition, and the public `tracedecay` Rust library.
- The existing domain, application, use-case, storage, query, and runtime
  crates remain the canonical owners of their current capabilities.

The package move must preserve the package name, binary name, product version,
feature behavior, release artifacts, persisted formats, and public runtime
behavior.

## Why This Is Architectural

The current repository is a valid Cargo layout: a manifest may be both a
workspace root and a package. The root package is not an obsolete binary. It is
a 400,000-line library whose largest owned areas are daemon composition and MCP
serving, and `tracedecay-cli` depends on it.

Relocation is nevertheless useful if it establishes one durable rule: every
Rust package lives under `crates/`, while the repository root owns only
workspace-wide configuration and product assets. A path-only move that leaves
version, packaging, fixtures, and test ownership ambiguous would not meet this
goal.

## Current State

- The workspace-root `Cargo.toml` contains both `[workspace]` and `[package]`.
- The root package is named `tracedecay` and exposes only a library target.
- `crates/tracedecay-cli` owns the sole binary named `tracedecay` and is the
  only workspace package with a normal dependency on the root library.
- The root package owns `src/`, its build script, Rust integration tests,
  benches, and an explicit package-asset whitelist.
- The root manifest's literal package version is the product-version authority
  consumed by release automation and `tracedecay-agent-hosts`.
- Root build and test code currently resolves repository assets through
  `CARGO_MANIFEST_DIR`.
- Several tiny root modules are compatibility re-exports of canonical
  `tracedecay-usecases` implementations. Separately, host-admission integration
  fixtures are compiled into the library so external integration tests can
  reach them.

## Considered Approaches

### 1. Keep the mixed workspace/package root

This is the lowest-risk and entirely normal Cargo layout. It avoids path churn,
but it does not satisfy the requested uniform `crates/` package layout and
continues to make repository assets and package-owned sources look like the
same architectural unit.

### 2. Relocate the package intact under `crates/tracedecay` (selected)

Move the package as one semantic unit, then separately remove proven
compatibility and test-support leakage. This preserves the existing
composition boundary while making the repository root virtual. It is noisy but
mechanical, and its invariants can be tested without redesigning runtime
behavior.

### 3. Split daemon, MCP, and dashboard composition into new packages

This could reduce rebuild scope and public surface further, but it changes
dependency direction and runtime interfaces. Combining it with relocation
would make regressions impossible to attribute. It is explicitly deferred
until dependency and build-time evidence justifies each extraction.

## Target Layout

```text
Cargo.toml                         # virtual workspace and shared policy
crates/
  tracedecay/
    Cargo.toml                     # package tracedecay, library target
    build.rs                       # product library build authority
    build-support/
    src/
    tests/
    benches/
  tracedecay-cli/
    Cargo.toml                     # package tracedecay-cli
    src/main.rs                    # binary tracedecay
dashboard/                         # canonical frontend source remains shared
plugin/                            # canonical host bundle source remains shared
benchmark_data/                    # canonical benchmark fixtures remain shared
vendor/                            # canonical vendored parsers remain shared
```

No second copy of dashboard, plugin, benchmark, fixture, or vendor data may be
checked in beneath `crates/tracedecay`.

## Package and Dependency Contracts

The relocated package remains named `tracedecay`; this is not a rename. Its
features and dependency graph remain byte-for-byte equivalent unless a path
must change for relocation.

`crates/tracedecay-cli` changes its dependency path from `../..` to
`../tracedecay`. No other package should gain a direct dependency on
`tracedecay` as part of the move.

The root manifest gains `crates/tracedecay` as an explicit workspace member and
loses `[package]`, `[lib]`, package features, package dependencies, dev
dependencies, tests, and benches. Workspace dependencies, profiles, patches,
and lint policy remain at the root.

## Product Version Authority

The repository must retain one literal product version that release tooling
can update and host bundles can bake without building the CLI.

The selected authority is `[workspace.package].version` in the virtual root
manifest. `crates/tracedecay` and `crates/tracedecay-cli` inherit it with
`version.workspace = true`. Existing product-version parsing, release-please
configuration, release integrity checks, and build-script rerun edges are
migrated to this field in the same commit. Other workspace crates keep their
existing package-version policy unless they are already intended to track the
product version.

No fallback to a crate-local version, Git tag, or fabricated default is
allowed.

## Repository Asset Resolution

Relocation changes `CARGO_MANIFEST_DIR`, so repository assets need an explicit
authority. Code must not scatter `../..` joins throughout production modules.

The relocated package build script computes the repository root from its
manifest location, verifies expected sentinels (`Cargo.toml`, `dashboard/`, and
`plugin/`), and exports a compile-time repository-root-relative layout for
build-time inclusion. Test and benchmark helpers use one crate-owned
`repository_layout` helper to resolve checked-out fixtures.

Installed production behavior may not depend on the source checkout. Assets
needed at runtime remain embedded or copied into the distribution artifact.
Tests and developer benchmarks may use checkout-only fixture paths.

## Dashboard and Package Archive

The dashboard source remains at `dashboard/`. The relocated build script still
builds that source and embeds the canonical `dashboard/app-dist` manifest.

Cargo packages cannot safely claim arbitrary parent-directory files as package
contents. Therefore distribution assembly becomes explicit:

1. Build the canonical dashboard.
2. Stage the exact package asset whitelist in an isolated distribution tree.
3. Place the relocated `tracedecay` manifest and sources at that tree's package
   root alongside the selected dashboard, plugin, benchmark, fixture, and
   vendor assets.
4. Run `cargo package` and all existing extracted-package consumers against the
   staged tree.

The distribution acceptance script remains the authority for proving archive
contents, feature wiring, build identity, host bundles, and the executable. It
must compare the staged manifest to `crates/tracedecay/Cargo.toml` rather than
the virtual workspace manifest.

## Tests and Benches

Package-owned Rust integration tests and benches move with the package. Shared
fixture directories remain at the repository root and are resolved through the
single repository-layout helper.

The move must preserve target names, `required-features`, ignored-test policy,
and anti-vacuity behavior. It may not silently drop auto-discovered tests.
Before removing root `tests/` or `benches/`, an inventory must map every current
Rust target to its relocated target. Non-Rust shell/Python acceptance tests and
shared fixtures stay at the repository root.

## Compatibility Shims

Relocation alone does not prove a public item is unused. After the mechanical
move is green, the small root compatibility modules that merely glob-re-export
canonical `tracedecay-usecases` modules are handled in a separate commit:

1. Migrate internal callers to the canonical crate.
2. Check CLI, integration-test, schema, macro, and string-keyed consumers.
3. Delete only shims with no required external contract.

The root package is unpublished, so an unshipped compatibility path does not
justify permanent duplication. Persisted and wire contracts remain unchanged.

## Test-Support Boundary

Host-admission integration support is a distinct concern from dead production
surface. External integration tests currently require those helpers from a
non-`cfg(test)` library build.

The relocated package introduces an explicit `test-helpers` feature. Test-only
modules and public fixture types compile only under
`cfg(any(test, feature = "test-helpers"))`. Integration targets that consume
them declare the feature explicitly; production and `production`-feature
builds must not contain them. `test-transport` may depend on `test-helpers`, but
the two meanings remain distinct.

No test-support crate or public trait facade is added merely to avoid declaring
the feature.

## Migration Sequence

1. Establish workspace product-version authority and update its consumers.
2. Add the relocated package manifest and move build/source ownership in one
   compile-preserving commit.
3. Move package-owned Rust tests and benches, introduce repository-layout
   resolution, and prove the exact target inventory.
4. Update distribution staging, dashboard embedding, release automation,
   documentation, and source-package checks.
5. Gate host-admission fixtures behind `test-helpers` and update consumers.
6. Migrate and remove compatibility shims whose callers are fully known.
7. Run focused, workspace, dashboard, distribution, and production binary
   journeys before requesting final review.

Each step is committed independently. The branch may temporarily contain
mechanical moves, but every pushed commit must leave Cargo metadata coherent;
behavioral changes never share a commit with bulk path relocation.

## Verification

At minimum, the completed branch must prove:

- `cargo metadata --no-deps --format-version 1` reports a virtual root,
  `crates/tracedecay` as package `tracedecay`, and exactly one binary named
  `tracedecay` owned by `tracedecay-cli`.
- `cargo check -p tracedecay --lib --no-default-features --locked` passes.
- `cargo check -p tracedecay-cli --bin tracedecay --locked` passes.
- The production and all-feature package graphs retain the intended feature
  sets, including feature-off Hotpath behavior.
- Relocated unit and integration targets execute non-vacuously.
- Dashboard contract generation/check, typecheck, focused UI tests, and build
  pass.
- Distribution acceptance builds the staged source archive and its external
  consumer probes.
- Release-version and host-bundle drift checks read the workspace product
  version exactly.
- Default production compilation contains no host-admission test-support
  surface.
- `git diff --check`, formatting, and relevant clippy gates pass.

## Rollback

Before merge, rollback is a normal branch revert. No persisted data or runtime
configuration changes are involved. The mechanical relocation commit is kept
separate so it can be reverted without retaining half-moved package paths.

## Non-Goals

- Renaming the `tracedecay` library or executable.
- Splitting daemon, MCP, dashboard, or runtime composition into additional
  packages in this pull request.
- Changing serialized, database, JSON-RPC, MCP, LSP, hook, or dashboard wire
  contracts.
- Treating raw dead-code counts as proof that a crate or public API can be
  deleted.
- Adding compatibility aliases solely for branch-local paths.

# TraceDecay Root Package Relocation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the repository root into a virtual Cargo workspace and relocate the existing `tracedecay` daemon/MCP composition library to `crates/tracedecay` without changing product behavior.

**Architecture:** Preserve package/API identity while changing filesystem and manifest ownership. Establish workspace product-version authority first, move the package atomically, then migrate tests, release staging, test helpers, and compatibility shims in separately reviewable commits.

**Tech Stack:** Rust 2024, Cargo resolver 3, Bash/Python distribution checks, Rsbuild/TypeScript, GitHub Actions and release-please.

**Spec:** `docs/superpowers/specs/2026-08-25-root-package-relocation-design.md`

## Global Constraints

- The library remains package `tracedecay`; the executable remains `tracedecay` in package `tracedecay-cli`.
- Persisted and wire contracts do not change.
- `[workspace.package].version` is the only literal product version.
- No checked-in duplicate dashboard, plugin, benchmark, fixture, or vendor assets are added.
- Existing parallel JSON-RPC edits are adopted only after their producer finishes and verification is green.
- Every commit is coherent and stages only task-owned files.

---

### Task 1: Workspace Product-Version Authority

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/tracedecay-cli/Cargo.toml`
- Modify: `crates/tracedecay-agent-hosts/src/product_version/root_manifest.rs`
- Modify: `crates/tracedecay-agent-hosts/src/product_version.rs`
- Modify: `crates/tracedecay-agent-hosts/build.rs`
- Modify: `release-please-config.json`

**Interfaces:**
- Produces: `root_manifest::resolve(&Path) -> Option<String>` reading `[workspace.package].version`.

- [ ] Add this RED test in `root_manifest.rs`:

```rust
#[test]
fn a_virtual_workspace_product_version_round_trips_through_a_file() {
    let directory = tempfile::tempdir().expect("temporary directory");
    std::fs::write(
        directory.path().join(ROOT_MANIFEST_FILE),
        "[workspace]\nresolver = \"3\"\n\n[workspace.package]\nversion = \"0.1.0-beta.38\"\n",
    )
    .expect("write manifest");
    assert_eq!(resolve(directory.path()).as_deref(), Some("0.1.0-beta.38"));
}
```

- [ ] Run the exact test and confirm one failure because the parser still requires a root `[package]`:

```bash
scripts/require-exact-test.sh cargo test -p tracedecay-agent-hosts --lib --locked \
  product_version::root_manifest::tests::a_virtual_workspace_product_version_round_trips_through_a_file -- --exact
```

- [ ] Parse only the literal workspace package version; preserve fail-closed behavior for missing, inherited, malformed, or ambiguous values.
- [ ] Add the current literal version to `[workspace.package]`; make the root package and CLI inherit it; update release-please to update the workspace field once.
- [ ] Run:

```bash
cargo test -p tracedecay-agent-hosts --lib --locked product_version::
python3 scripts/test-check-distribution-feature-wiring.py
cargo metadata --no-deps --format-version 1
git diff --check
```

- [ ] Commit:

```bash
git commit -m "refactor(version): move product version to workspace"
```

### Task 2: Relocate the Composition Library

**Files:**
- Create: `crates/tracedecay/Cargo.toml`
- Move: `src/**` to `crates/tracedecay/src/**`
- Move: `build.rs` and `build-support/dashboard_manifest.rs` under `crates/tracedecay/`
- Modify: `Cargo.toml`
- Modify: `crates/tracedecay-cli/Cargo.toml`
- Modify: `crates/tracedecay-agent-hosts/build.rs`

**Interfaces:**
- Produces: package `tracedecay` at `crates/tracedecay` with unchanged library, features, dependencies, and public symbols.

- [ ] Capture untracked pre-move metadata at `/tmp/tracedecay-root-package-before.json`; record package targets, feature keys, and dependency package names.
- [ ] Move root package sections into `crates/tracedecay/Cargo.toml`, translate dependency paths, add the member, and leave workspace policy at root.
- [ ] Use `git mv` for source/build ownership. Resolve the canonical dashboard from the workspace root without embedding an absolute path.
- [ ] Change the CLI path dependency from `../..` to `../tracedecay`; update agent-hosts build paths for `build_identity.rs`.
- [ ] Verify:

```bash
cargo metadata --no-deps --format-version 1 > /tmp/tracedecay-root-package-after.json
cargo check -p tracedecay --lib --no-default-features --locked
cargo check -p tracedecay-cli --bin tracedecay --locked
```

Expected: virtual root; one library package `tracedecay` under `crates/`; one binary `tracedecay` owned by the CLI; pre/post feature and dependency names match.

- [ ] Commit:

```bash
git commit -m "refactor(workspace): relocate composition library"
```

### Task 3: Relocate Package Tests and Benches

**Files:**
- Move: package-owned Rust `tests/**` to `crates/tracedecay/tests/**`
- Move: `benches/**` to `crates/tracedecay/benches/**`
- Create: `crates/tracedecay/tests/common/repository_layout.rs`
- Modify: relocated fixture and benchmark path callers
- Modify: `crates/tracedecay/Cargo.toml`

**Interfaces:**
- Produces: one `repository_root() -> &'static Path` test/bench authority validating `Cargo.toml`, `dashboard/`, and `plugin/` sentinels.

- [ ] Inventory all 29 top-level Rust tests, 14 suite `main.rs` targets, four benches, and explicit target declarations. Keep non-Rust scripts/shared fixtures at root.
- [ ] Write a RED test requiring `repository_root()` to find the three sentinels.
- [ ] Move Rust targets and replace direct root assumptions based on `CARGO_MANIFEST_DIR` with the one helper.
- [ ] Compare Cargo target inventory to the pre-move metadata and run non-vacuously:

```bash
cargo test -p tracedecay --lib --locked
cargo test -p tracedecay --test runtime_surface_acceptance --locked
cargo test -p tracedecay --test mcp_suite --locked
cargo test -p tracedecay --test session_suite --locked
cargo bench -p tracedecay --bench queries --no-run --locked
```

- [ ] Commit:

```bash
git commit -m "refactor(test): move composition package targets"
```

### Task 4: Distribution and Dashboard Packaging

**Files:**
- Modify: `scripts/check-distribution-acceptance.sh`
- Modify: `scripts/check-production-feature-profile.py`
- Modify: `scripts/check-distribution-feature-wiring.py`
- Modify: `scripts/test-check-distribution-feature-wiring.py`
- Modify: `crates/tracedecay/build.rs`
- Modify: affected `.github/workflows/*.yml`

**Interfaces:**
- Produces: isolated package staging from `crates/tracedecay` plus the existing exact root asset whitelist.

- [ ] Change distribution fixture expectations first and confirm they fail because scripts still select the virtual root manifest as the product package.
- [ ] Stage exact dashboard/plugin/benchmark/fixture/vendor assets into a temporary package tree; reject missing assets and compare source/manifest digests before `cargo package`.
- [ ] Update only workflow paths that mean the package manifest; workspace commands remain rooted at `Cargo.toml`.
- [ ] Verify:

```bash
python3 scripts/test-check-distribution-feature-wiring.py
scripts/check-distribution-acceptance.sh
cd dashboard && npm run contracts:check && npm run typecheck && npm test && npm run build
cargo build -p tracedecay-cli --bin tracedecay --locked
target/debug/tracedecay --version
```

- [ ] Commit:

```bash
git commit -m "build(distribution): stage relocated product package"
```

### Task 5: Gate Host-Admission Test Support

**Files:**
- Modify: `crates/tracedecay/Cargo.toml`
- Modify: `crates/tracedecay/src/host_admission.rs`
- Modify: consuming targets under `crates/tracedecay/tests/**`

**Interfaces:**
- Produces: feature `test-helpers`; `test-transport` includes it; fixtures compile only under `cfg(any(test, feature = "test-helpers"))`.

- [ ] Add an external compile probe showing `HostAdmissionTestRuntimeV1` currently resolves without a test feature.
- [ ] Gate support modules/reexports and explicitly feature-gate every integration consumer.
- [ ] Confirm the production probe no longer resolves the helper, while representative session, MCP, storage, and CLI journeys pass with the declared test feature.
- [ ] Commit:

```bash
git commit -m "test(host-admission): gate integration fixture surface"
```

### Task 6: Adopt JSON-RPC Work and Remove Proven Shims

**Files:**
- Adopt after producer completion: six daemon JSON-RPC files currently modified in this worktree
- Modify/delete: compatibility modules only after complete caller migration

**Interfaces:**
- Consumes: canonical `tracedecay-jsonrpc` and `tracedecay-usecases` APIs.
- Produces: no duplicate root authority for migrated types.

- [ ] Do not edit producer-owned files while its Cargo process is active. Inspect its final diff and result; adopt only behavior-preserving canonical imports.
- [ ] Run focused daemon/projectless/bootstrap checks and commit the six files independently:

```bash
git commit -m "refactor(jsonrpc): use canonical daemon wire types"
```

- [ ] For each thin usecase shim, resolve Rust, CLI, integration, macro, schema, and string-keyed consumers; migrate callers and delete only when no supported consumer remains.
- [ ] Run mapped tests after each cohesive owner group and commit each group separately, e.g.:

```bash
git commit -m "refactor(usecases): remove root diagnostic shims"
```

### Task 7: Final Integration

**Files:**
- Modify only paths required by integration diagnostics.
- Update: draft PR verification and summary.

- [ ] Merge the latest explicit clean `codex/code-index-catchup-pipeline` floor and regenerate canonical outputs after conflict resolution.
- [ ] Run:

```bash
cargo fmt --all -- --check
cargo check -p tracedecay --lib --all-features --locked
cargo check -p tracedecay-cli --bin tracedecay --all-features --locked
cargo clippy -p tracedecay -p tracedecay-cli --all-targets --all-features --locked -- -D warnings
git diff --check
```

- [ ] Run workspace-appropriate tests, dashboard gates, distribution acceptance, SDK drift checks, and production CLI/daemon smoke. Record exact non-vacuous results.
- [ ] Review for absolute build paths, duplicated assets, default test helpers, stale root-manifest assumptions, lost tests, and compatibility aliases.
- [ ] Push coherent commits, update PR evidence, and mark ready only after required checks and independent review are clear.

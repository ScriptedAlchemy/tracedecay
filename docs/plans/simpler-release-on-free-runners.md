# Simpler TraceDecay beta release on free runners

**Status:** design proposal. Not roadmap authority. `00-plan-set-index.md`
remains the product plan set. Implementation lands in later PRs; this
document is the cutover contract.

**Constraint:** GitHub-hosted `ubuntu-*` / `macos-*` / `windows-latest`
only. No paid or self-hosted runners. Do not make the job faster by adding
jobs or shards. Do not merge this design as a workflow change.

## Outcome

Beta release becomes a packager, not a second CI.

One target on a free runner does one production compile of `tracedecay`,
wraps that exact binary, smokes the archive and MCPB, and uploads attested
assets. Compile-safety, crate-extract rebuilds, and workspace `--release
--all-features` tests leave the release critical path.

Stable keeps the same packaging path plus the publication steps only stable
owns (npm, `server.json`, `latest`). Beta stays a prerelease and never
takes those.

## Diagnosis (measured)

Source: `Release (Beta)` run
[35271070760](https://github.com/ScriptedAlchemy/tracedecay/actions/runs/35271070760)
for `v0.1.0-beta.40`, 2026-09-17. Every build job failed before packaging.
`publish` was skipped. The tag existed with no new assets.

| Step | aarch64-linux | x86_64-linux | aarch64-macos | x86_64-windows |
| --- | ---: | ---: | ---: | ---: |
| Dashboard build | 11s | 13s | 9s | 19s |
| Portable harness unit tests | 10s | 13s | 15s | 29s |
| Resolve feature profile (`cargo tree` ×2) | 4m 3s | 4m 19s | 4m 36s | 5m 3s |
| Test release distribution (Linux only) | skipped | **2h 23m, failed** | skipped | skipped |
| Verify all-feature release compile | 24m 55s | never reached | 45m 27s | skipped |
| Build production binary for packaging | 19m 52s | never reached | 35m 14s | 55m 40s |
| Distribution acceptance | 23m 42s, failed | never reached | 39m 52s, failed | 28s, failed |
| Package archive / MCPB / upload | never reached | never reached | never reached | never reached |
| Job wall | 74m, failed | 143m, failed | 127m, failed | 65m, failed |

The Linux ARM column is the shape the request described: ~4m profile, ~25m
all-feature verify, ~20m package compile, ~24m acceptance, then death
before an asset exists. Linux x86 is worse: the extra
`cargo test --workspace --release --all-features` step burned the runner
and never reached the binary that ships.

The same battery is copied into three workflows today:

- `.github/workflows/release-beta.yml`
- `.github/workflows/release.yml`
- `.github/workflows/release-pr-distribution-acceptance.yml` (Linux only,
  180-minute timeout)

`docs/RELEASE-AUTOMATION.md` already says release acceptance exercises the
produced archive and installed binary. `scripts/check-distribution-acceptance.sh`
does not do that. It `cargo package`s the workspace, extracts `.crate`
trees, rebuilds the CLI from the extract, and runs packaged-crate nextest.
That is crates.io readiness. Crates.io publication is an explicit non-goal
until crate naming is settled. The GitHub archive and MCPB wrap the
compiled binary, not a crate extract.

The all-feature compile exists so a `test-transport` binary cannot sit on
the shared release output path. That is a sequencing workaround for a
second compile, not proof of the artifact. Windows already skips it. CI
`feature-gates` already `cargo check`s the `test-transport` and `hotpath`
graphs.

`tests/release_safety_test.sh` currently string-matches
`Test release distribution` and
`cargo test --workspace --release --target`. That guard is what keeps the
2h Linux x86 suite welded to the release job. The file header already says
the expensive silent failures are identity, attestation, halfway
publication, and mutable packagers. The workspace `--release` suite is not
one of those.

## Redesigned beta job (one target)

Keep the existing four-target matrix and the `validate` / `build` /
`publish` job split. Do not add a dashboard job, a Linux-only test job, or
shards. Dashboard build is 10–20s; leave it inline.

### `validate` (unchanged authority)

Runs once on `ubuntu-22.04`.

1. Checkout the tag.
2. Require a published prerelease, `version.txt` with a hyphen, tag =
   `v${version}`, `GITHUB_REF` / `GITHUB_SHA` = tag SHA.
3. Plan recovery from `.github/release-targets.json`.
4. Attestation-verify any retained assets. Never rebuild an uploaded
   binary to compare bytes.

### `build` (one target, free runner)

1. Checkout the tag SHA. Install Node, Python, the pinned Rust toolchain,
   and the platform linker (mold / lld-link). Restore the existing
   `release-beta-${{ matrix.target }}` cache.
2. `npm ci && npm run build` in `dashboard/`. The CLI build script embeds
   this bundle and fails closed without it.
3. Resolve the production Cargo args from the product manifest only
   (`--no-default-features --features production`). Do not run
   `cargo tree` here.
4. **One compile:**
   `cargo build --package tracedecay-cli --bin tracedecay --release --locked --target <triple> --no-default-features --features production`
5. Record `tracedecay --version`. It must be
   `tracedecay <version>+<source_sha>`.
6. Package the MCPB and the deterministic archive from that binary
   (`scripts/build-mcpb.py`, `scripts/package-release-archive.py`).
7. Extract each archive and run `--version` / `--help` on the packaged
   entry (unix and Windows paths already in the workflow).
8. Unix only: MCP stdio smoke (`scripts/mcp-conformance-smoke.sh` or
   `scripts/check-packaged-mcp-stdio.py`) against the extracted binary.
9. Upload the two artifacts.

No second checkout of `.release-automation` is required once the job
stops treating the tag as a historical source that might lack the
scripts. Current tags already carry them.

### `publish` (unchanged authority)

Assemble retained + new assets, attest new bytes, upload only missing
names, re-download, and re-verify names, checksums, and attestations.
`SHA256SUMS` stays. The release is never marked `latest`.

### Concrete step list (copy target)

```text
checkout tag SHA
setup node 22.23.2 + python 3.12.10
npm ci && npm run build   # dashboard/
rust-toolchain + mold|lld-link + rust-cache
resolve production cargo args from crates/tracedecay/Cargo.toml
cargo build -p tracedecay-cli --bin tracedecay --release --locked \
  --target $TARGET --no-default-features --features production
assert `$binary --version` == tracedecay $VERSION+$SOURCE_SHA
build-mcpb.py build+verify
package-release-archive.py
extract archive; --version / --help
unix: mcp-conformance-smoke.sh on extracted binary
upload-artifact
```

That is the whole per-target critical path.

## What leaves the release critical path

### Delete from beta, stable packaging, and the release-PR copy

These steps do not produce or prove the GitHub asset.

| Step | Why it goes | Where it lives after |
| --- | --- | --- |
| Validate portable distribution harnesses | Python/bash unit tests of packagers | Existing CI `release-version-drift` job (already runs release script tests; add the missing harness tests there) |
| `cargo tree` inside `resolve-release-source-profile.py` | 4–5 minutes of graph identity, not the artifact | Existing CI `feature-gates` job, after the current `cargo check` lines |
| Test release distribution (`nextest` hotpath + `cargo build/test --workspace --release --all-features`) | 2h+ second CI; Linux x86 only; failed beta.40 | Delete. CI already shards the suite. Hotpath parity is its own CI job. |
| Verify all-feature release build compiles | Second full release compile so a contaminated binary cannot occupy the output path | Delete the second compile. The only compile uses `--features production`. |
| `scripts/check-distribution-acceptance.sh` | Third compile plus crate-extract rebuild; crates.io rehearsal | One nightly Linux job |
| `cargo-nextest` install on the release runner | Only the deleted suites need it | Nightly / CI |
| `brew install bash` | Only the deleted acceptance script needed modern Bash | Nightly if that script still needs it |

### Defer to nightly (one Linux job, not a shard farm)

Create `.github/workflows/nightly-distribution.yml` later, not in this
design PR.

Trigger: `schedule` plus `workflow_dispatch`. One `ubuntu-latest` (or
`ubuntu-22.04`) job. Timeout ~180 minutes. Cancel-in-progress true.

Contents:

1. Dashboard build.
2. `scripts/check-production-feature-profile.py` (the `cargo tree` check).
3. `scripts/check-distribution-acceptance.sh` with `CARGO_NET_OFFLINE`
   after `cargo fetch --locked`.
4. Optional compile-safety:
   `cargo build -p tracedecay-cli --bin tracedecay --release --all-features --locked`.
   Do not install that binary. Do not package it.

Nightly red does not unpublish a beta. It blocks the next stable dispatch
until the SHA's nightly is green, or the operator records a typed waiver
on the stable release PR. Empty success is not a waiver.

### Keep, but not on the packaging runner

| Check | Home |
| --- | --- |
| Tag / version / prerelease identity | `validate` |
| Recovery planner + retained attestation | `validate` and `publish` |
| Deterministic archive + MCPB verify | `build` after the one compile |
| Asset name / checksum contract | `publish` via `check-release-artifacts.py` |
| `--deny-self-hosted-runners` on attestations | existing verifier |
| Release PR path integrity | `release-pr-integrity.yml` |
| SDK npm publish | stable only |
| `server.json` rewrite + `latest` | stable only |

### Slim, do not multiply, the release-PR gate

`release-pr-distribution-acceptance.yml` exists because twelve beta tags
published with zero assets: the first `check-distribution-acceptance.sh`
run happened after the tag. After this cutover the thing that can still
produce an empty tag is "the production binary does not compile or the
packager fails."

Replace that workflow's body with the same Linux packaging steps as beta
(`x86_64-linux` only, already one job). Do not keep the 180-minute
crate-extract battery as a merge gate. Do not add macOS/Windows copies.

Once one slim beta has uploaded assets, the PR workflow is optional. Keep
it through the first two migration PRs so a tree that cannot compile in
release mode still fails before the tag.

## Beta vs stable without being unsafe

Shared and required on both channels:

- Immutable tag SHA, version authority, locked production compile.
- `--no-default-features --features production`. `test-transport` is not
  in that feature set (`check-production-feature-profile.py` in CI).
- Dashboard embed from a just-built `dashboard/app-dist`.
- Deterministic archive + MCPB from that binary.
- Packaged `--version` equals `tracedecay <version>+<source_sha>`.
- Attest new bytes. Retain already-attested assets. Never rebuild to
  compare linker output.
- Recovery builds only missing names.

Beta only:

- Trigger: prerelease tag or recovery dispatch.
- Asset prefix `tracedecay-beta-<tag>-<platform>`.
- Never `--latest`.
- No npm. No `server.json` rewrite.
- May publish after a green packaging job even if last nightly is red.
  Nightly red is operator-visible, not a beta admission blocker.

Stable only:

- Trigger: non-prerelease tag or recovery dispatch. Still a deliberate
  `Release Please` dispatch with an exact `X.Y.Z`.
- Asset prefix `tracedecay-<tag>-<platform>`.
- Requires the SHA's latest nightly distribution job to be green, or an
  explicit waiver on the stable release PR.
- `verify-release`, npm trusted publish, `server.json` rewrite, mark
  `latest`.
- Same one-compile packaging path. Do not keep a heavier compile matrix
  "because it is stable."

Unsafe would be: shipping `--all-features`, skipping attestation,
comparing rebuilt bytes, marking beta `latest`, or treating a crate-extract
failure as a packaging success. None of those are part of the slim job.

The remaining accepted risk: a beta can ship a binary whose *crate
whitelist* is wrong. That defect does not enter the GitHub archive or
MCPB. Those wrap the compiled binary and the assets `include_str!` /
`build.rs` already embedded. The whitelist defect is a crates.io /
`cargo package` problem, which nightly still catches and which stable
still waits for.

`docs/RELEASE-AUTOMATION.md` also claims every supported host
install/update/uninstall journey runs at release time. The current
acceptance script does not do that. Host lifecycle stays in CI / plugin
validation. Do not invent a new host-fleet job to replace the deleted
crate-extract battery.

## Estimated wall time

Cold cache, one target, after the cutover. Compile times taken from the
production-binary step on beta.40. That step ran *after* an all-feature
compile on unix, so a true cold production compile can be a few minutes
longer on Linux/macOS. Windows already measured a production-only compile.

| Target | Today (failed before assets) | Slim job (expected) |
| --- | ---: | ---: |
| aarch64-linux | 74m | **25–35m** |
| x86_64-linux | 143m | **25–40m** |
| aarch64-macos | 127m | **40–55m** |
| x86_64-windows | 65m | **55–65m** |

Overhead after deleting the extra compiles is ~5 minutes (checkout,
dashboard, toolchain, cache, package, smoke, upload).

Warm `rust-cache` can cut the compile. Do not treat cache as the design.
The design removes the second and third release-mode graphs.

Windows remains the slowest target because one MSVC release link of this
workspace is ~56 minutes on `windows-latest`. That is the irreducible
packaging compile, not a gate we can delete. Do not raise a timeout to
hide it. Do not add a Windows shard.

End-to-end beta wall clock is the slowest target plus the small
`validate`/`publish` jobs: about **60–70 minutes** today-shaped Windows,
versus **2h+ and no assets** on beta.40.

## What this design refuses

- Paid, larger, or self-hosted runners.
- More jobs or shards to hide one compile.
- A dashboard-assets job on beta (stable already has one; beta's
  dashboard step is 10–20s).
- Keeping `check-distribution-acceptance.sh` on every target "just in
  case."
- A nightly that re-introduces workspace `--release --all-features`
  tests.
- Compatibility shims that run both batteries.
- Weakening `--version` identity, attestation, or lockfile `--locked`.
- Changing asset names the CLI beta channel already resolves.

## Migration (small PRs, in order)

Each PR must leave a publishable beta. Do not land a hole where neither
the old gate nor the new smoke exists.

### PR 0 — this document

`docs/plans/simpler-release-on-free-runners.md` only. Draft. No workflow
edit.

### PR 1 — stop the 2h Linux x86 suite

Delete `Test release distribution` from `release-beta.yml`, `release.yml`,
and `release-pr-distribution-acceptance.yml`.

Rewrite the `tests/release_safety_test.sh` string-match. Keep the header
invariants: identity, recovery planner, retained attestation, deterministic
`package-release-archive.py`, no timestamp-sensitive `tar czf` /
`Compress-Archive`, no rebuilt-byte `cmp`. Require the packaging compile
to pass `--no-default-features --features production` (or the resolver
output that is exactly that). Forbid a workspace `--release` test on the
packaging job.

Verify: `bash tests/release_safety_test.sh`. Do not run the old 2h suite
to "prove" the deletion.

### PR 2 — one compile

Delete `Verify all-feature release build compiles` from the three
workflows. Keep the comment's invariant by construction: the only
`cargo build` writes the production binary to the output path.

Split `scripts/resolve-release-source-profile.py`: TOML +
`--features production` stay on the release runner; `cargo tree` moves
into CI `feature-gates` as `python3 scripts/check-production-feature-profile.py`.

Verify: `scripts/require-exact-test.sh` or the existing Python tests for
the resolver. Confirm `feature-gates` still reports a non-zero check count.

### PR 3 — move crate-extract off the critical path

Remove `Run distribution acceptance` and the macOS `brew install bash`
step from beta, stable packaging, and the release-PR workflow.

Keep packaged archive / MCPB `--version` / `--help`. Add the unix MCP
stdio smoke against the *extracted* binary if it is not already in that
path.

Add `.github/workflows/nightly-distribution.yml` as specified above. One
job.

Update `docs/RELEASE-AUTOMATION.md`: release acceptance is the produced
archive and MCPB; crate-extract is nightly; stable waits for nightly.

Slim `release-pr-distribution-acceptance.yml` to the Linux packaging
smoke, or delete it after PR 4 if a slim beta has already published
assets.

Verify: a dry `workflow_dispatch` of `release-beta.yml` against an
existing prerelease tag that is missing one platform asset, or the next
natural beta. Success is an attested `tracedecay-beta-<tag>-<platform>`
archive on the GitHub Release, not a green crate-extract log.

### PR 4 — harness tests to CI; docs and stable publication gate

Move the portable harness unit tests into the existing
`release-version-drift` job.

Stable `publish-assets` / `verify-release`: require the nightly workflow
run for `github.sha` to be success, or the `release-nightly-waived` label
on the stable release PR. No silent skip.

Delete leftover nextest installs from packaging jobs.

Rewrite the stale "installed host profile for every supported host"
sentence in `docs/RELEASE-AUTOMATION.md` so it names the CI / plugin
surfaces that actually run those journeys.

### Stop condition

A beta tag uploads all four platform archives and MCPBs from the slim
job. Nightly has run once on `master`. Stable still publishes npm /
`server.json` / `latest` and still refuses a red nightly without a
waiver. `tests/release_safety_test.sh` no longer requires the deleted
suite.

## Acceptance

This design is done when the later implementation PRs show, on a real
tag:

1. Each packaging runner performed one `cargo build` of
   `tracedecay-cli`.
2. The uploaded binary `--version` matches the tag SHA.
3. Attestations verify with `--deny-self-hosted-runners`.
4. Wall time per target is in the table above, measured on the
   implementation PR's recovery or beta run, not estimated again from this
   document.
5. No new job or shard was added to `release-beta.yml`.

Until those PRs land, this file is a proposal. It does not change what
`Release (Beta)` runs.

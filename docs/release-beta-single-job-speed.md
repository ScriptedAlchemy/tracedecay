# Release build job speed

Single-job cuts for `Release (Beta)`. Do not add jobs. The matrix stays one
job per target. The same steps are copied in `release.yml` and
`release-pr-distribution-acceptance.yml`; a cut that leaves a copy behind does
not change the wall clock of the next beta.

Evidence is [run 35271070760](https://github.com/ScriptedAlchemy/tracedecay/actions/runs/35271070760)
(`v0.1.0-beta.40`, source `2f6c3621f5ca30b5a655a8288214d604bf406fd6`). Every
build job logged `No cache found` for `Swatinem/rust-cache`, so the minutes
below are cold compiles. A later cache hit shrinks real work and wasted work
together; it does not make the wasted work useful.

## Job wall clock

| Job | Wall | What consumed it | How it died |
| --- | ---: | --- | --- |
| Build x86_64-linux | 2h 29m (until 22:58Z) | `Test release distribution` 143.6m | Hotpath panic in `cargo test --workspace --release` |
| Build aarch64-macos | 2h 07m | all-features build 45.5m, packaging build 35.2m, distribution acceptance 39.9m | `lang-bash` feature check |
| Build aarch64-linux | 1h 14m | all-features build 24.9m, packaging build 19.9m, distribution acceptance 23.7m | `lang-bash` feature check |
| Build x86_64-windows | 1h 05m | packaging build 55.7m (all-features compile is skipped) | fixture mismatch, before any acceptance compile |
| Resolve release source feature profile | 4.0–5.0m on every job | two `cargo tree` runs, before cache restore | succeeded |

`publish` never started. No step downloaded a historical release binary.

## What "Run distribution acceptance" runs

Workflow step (`.github/workflows/release-beta.yml`):

1. `cargo fetch --locked --target <triple>` so the gate can run with
   `CARGO_NET_OFFLINE=true`. On aarch64 this fetch was about 2 seconds. Crates
   were already downloaded by the preceding builds.
2. `scripts/check-distribution-acceptance.sh`.

The script, in order:

| Step | Command | beta.40 time |
| --- | --- | --- |
| Profile, again | `resolve-release-source-profile.py` (two more `cargo tree` runs) | inside the first 2s; index already warm |
| Unused workspace build | `cargo build --workspace --release --no-default-features --features tracedecay/production --lib --bins` | **23m 34s aarch64, 39m 30s macOS.** This is the whole step. |
| Stage | `tar` the checkout (excluding `target`, `.git`, `node_modules`) and copy root assets beside the package manifests | 3s aarch64, 11s macOS |
| Package | `cargo package --workspace --allow-dirty --no-verify --exclude-lockfile` | about 1s |
| Feature wiring | `check-distribution-feature-wiring.py`, then one `cargo check --no-default-features --features <lang>` per `lang-*` feature | failed on the first feature in about 2s |
| Packaged CLI | `cargo build --release` of the extracted `tracedecay-cli` | not reached |
| Rust grammar | `cargo nextest run --all-features --test main -E 'test(/^rust::/)' --no-tests=fail` on extracted `tracedecay-code-extraction` (debug, not release) | not reached |
| Packaged lib check | `cargo check --release --no-default-features --features production --lib` | not reached |
| Query lib | `cargo nextest run --release --all-features --lib --no-tests=fail` on extracted `tracedecay-query` | not reached |
| Root lib | `cargo nextest run --release --no-default-features --features production --lib --no-tests=fail` on extracted `tracedecay` | not reached |
| LSP lib | `cargo nextest run --release --all-features --lib --no-tests=fail` on extracted `tracedecay-lsp` | not reached |
| MCP suite | `cargo nextest run --release --features production --test mcp_suite --no-tests=fail` with `TRACEDECAY_TEST_BIN` set to the packaged CLI | not reached |
| Install | `cargo install --path` the extracted CLI | not reached |
| Consumer | `cargo run --release` a throwaway bin that calls the catalog and host bundles | not reached |
| Test API denial | `cargo check` a probe that must fail to see `has_project_session_retrieval_service_for_test` | not reached |
| Installed smoke | `scripts/mcp-conformance-smoke.sh` (Unix) or `check-packaged-mcp-stdio.py` (Windows), then `tracedecay lsp servers` and `check-packaged-lsp-bridge.py` | not reached |

The opening `cargo build` result is not read. `packaged_cli_bin` is built later
from the extracted crate, into the staged tree's `target/` (the `tar` excludes
`./target`). Workflow builds pass `--target <triple>`, so they write
`target/<triple>/release`. The script does not pass `--target`, so it writes
`target/release`. Three directories, no reuse.

beta.40 died at `lang-bash does not compile in isolation` /
`no matching package named hotpath-macros`. Sorted `lang-*` names start at
`lang-bash`, and there are 37 of them in
`crates/tracedecay-code-extraction/Cargo.toml`. The check runs before any
packaged compile, so none of the nextest filters, the install, or the smoke
ran. That failure was the git-only `[patch]` overlay. Current `master` also
re-applies path patches (including `vendor/hotpath-macros`) and copies
`Cargo.lock` into each extracted crate. The next green resolve will pay the
unmeasured tail below. Do not treat 24 minutes as the cost of that tail.

Windows failed earlier, in about 30 seconds, on
`packaged host-event fixture copy differs from its authority: claude.json`.
It never started the unused workspace build.

## What "Resolve release source feature profile" does

`scripts/resolve-release-source-profile.py` reads
`crates/tracedecay/Cargo.toml`. If `production` exists (it does; `default` is
exactly `["production"]`), it runs `scripts/check-production-feature-profile.py`
and emits `cargo_args=--no-default-features --features production`. There is no
network call of its own and no historical tag checkout.

The checker runs `cargo tree --locked -p tracedecay --edges normal,build` twice,
serially, with stdout captured (the log is silent for the whole step):

1. default features
2. `--no-default-features --features production`

It compares the package set and the feature set, then rejects `test-transport`.
The second tree exists to prove default and `production` resolve the same
graph. The script already rejects a `default` that is not exactly
`["production"]`, so the second walk is redundant for the current manifest.

The step sits before `Cache Rust build`. On beta.40 the cache was empty, so
moving it would not have saved this run. On a warm cache it still blocks the
build by 4 minutes of CPU, unless one of the two walks is removed.

The `legacy-default` branch is only for source tags whose product manifest has
no `production` feature. It does not download anything. Current tags print
`release source profile: production`.

## What the x86_64 job did instead

`Test release distribution` is `if: matrix.name == 'x86_64-linux'`. It is why
that job never reached packaging or acceptance. Log timings:

| Slice | Measured |
| --- | ---: |
| `build-controlled-workload-hotpath-helpers.py --profile release` (feature-off example, then feature-on) | 9m 07s + 6m 11s |
| `cargo nextest run -p tracedecay-search-eval --release --all-features --run-ignored only -E 'test(=controlled_workloads::tests::hotpath_off_vs_on_durable_results_are_identical)' --no-tests=fail` | 24s compile, 0.062s test, 1 passed / 40 skipped |
| `cargo build --workspace --bins --release --all-features --locked` | 33m 17s |
| `cargo test --workspace --release --all-features --locked` | 93m 23s compile, then 735 lib tests, panic at ~1m in `hotpath` `MeasurementGuardSync` |

CI already runs the same nextest filter on the `perf` profile in the
`Hotpath parity` job (`.github/workflows/ci.yml`). The release-job pair exists
only to repeat it under `--release`. The durable-result assertion does not
depend on opt level.

`cargo test --workspace` is libtest, not nextest, with no filter. It rebuilds
every crate's test harness in release mode. CI already runs the suite under
nextest. This compile is the workflow's wall clock.

`Run historical release binary smoke` is `if: profile == 'legacy-default'`.
It was skipped. It only runs `--version` and `--help` on the binary just
built. It does not download a previous release.

Retained-asset downloads live in `scripts/verify-retained-release-assets.sh`,
called from `validate` and `publish`. `validate` finished in about 20 seconds
with nothing to retain. Not a build-job cost.

## Ranked cuts

Minutes are per job, cold, from beta.40 unless marked estimated. None of these
split the matrix.

| Rank | Cut | Save | Confidence |
| --- | --- | ---: | --- |
| 1 | Delete the `cargo test --workspace --release --all-features` invocation, and the `cargo build --workspace --bins --all-features` that only warms it, from `Test release distribution`. Keep the later `Verify all-feature release build compiles` only if a release-profile all-features compile is still required; otherwise delete that too (rank 4). | **127m on x86_64**, which is the workflow wall. 93m 23s is the test harness compile; 33m 17s is the bin compile it repeats. | Measured. This is the only cut that shortens the 2.5h job. |
| 2 | Delete the opening `cargo build --workspace --release … --lib --bins` in `scripts/check-distribution-acceptance.sh`. Nothing reads that binary. The workflow has already built the production CLI. | **24m aarch64, 40m macOS**, and the same class of compile on x86 once rank 1 lets that job reach the step. | Measured. |
| 3 | Drop the Hotpath helper pair and its nextest filter from the release job. `Hotpath parity` in CI already runs `controlled_workloads::tests::hotpath_off_vs_on_durable_results_are_identical` with `--run-ignored only --no-tests=fail`. | **16m on x86_64** (15m 18s of builds + 25s). | Measured. Leaves CI as the parity authority. |
| 4 | Stop proving `--all-features` with a full `cargo build --release` in this job. It exists so a `test-transport` binary is not what gets packaged. `cargo check --release --all-features` still typechecks without writing that binary; or rely on CI and do not compile the test feature set here at all. | **25m aarch64, 46m macOS.** Windows already skips it. On x86 this step did not run (the job died earlier); after rank 1 it would otherwise add another all-features release compile unless the bins build is kept as that proof. | Measured on aarch64 and macOS. |
| 5 | Run one `cargo tree`, not two, in `check-production-feature-profile.py`. `default == ["production"]` is already a hard failure. Check `test-transport` on the production graph only. | **about 2m on every job** (half of the measured 4.0–5.0m). | Estimated split of a measured step. stdout is captured, so the two walks are not separately timed. |
| 6 | In `require_isolated_language_features_compile`, check one feature per dependency set (`medium-grammars`, `large-grammars`, `lang-hlsl`, `lang-markdown`, `lang-wgsl`) instead of all 37 `lang-*` names. The manifest compare already proves the names match. | **not on beta.40's clock** (first check died in 2s). Once path patches resolve, expect on the order of 10–30m of serial `cargo check` process startups plus the first compile of each grammar bundle. Collapsing to five checks removes most of that tail, not the first bundle compile. | Estimated. Do not book a number until a run logs each check. |
| 7 | Install `@modelcontextprotocol/inspector` during the dashboard `npm ci` step (version is `scripts/lib/inspector_version`) so `mcp-conformance-smoke.sh` does not pay `npx -y` on the smoke critical path. The script already tries a 300s warm-up, then spawns a new inspector process per tools/call, with `sleep 1` retries up to `CALL_TIMEOUT_SECS` (60) while the graph warms. | **about 1–5m, unmeasured.** Smoke did not run. | Estimated from the script, not the run. |

Do not spend time on `cargo fetch` (2s), the second checkout of
`.release-automation` (about 9s), or historical binary smoke. They are not the
job.

## After rank 2, what is still serial

Deleting the unused build does not make acceptance cheap. The extracted tree
has its own `target/`, and rustc incremental keys include the source path, so
the packaged CLI release build is another cold production compile. The closest
measured analog is the packaging build in the same job: 20m aarch64, 35m
macOS, 56m Windows. Then, still in one target directory:

- debug nextest of `test(/^rust::/)` (new profile, little reuse)
- release nextest of query, root `--lib`, lsp, and `mcp_suite`

The 93m x86 `cargo test --workspace` number is not this cost. That command
builds every workspace test harness with `--all-features`. Acceptance builds
four specific test targets on the packaged graph. Time them on the next run
before cutting filters. The root `--lib` release harness is the one most
likely to dominate; query and lsp are smaller crates.

`cargo install` and the catalog consumer should reuse that packaged release
directory if the features stay `production`. They are not a second full
workspace build unless the target dir changes again.

No sccache, no extra job, and no `CARGO_TARGET_DIR` pointed at the checkout
`target/` will make the extracted sources cache-hit against the packaging
build. Path-identical fingerprints do not survive `cargo package`.

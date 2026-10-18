# Actions timing census (2026-09-18)

Measurement snapshot for ScriptedAlchemy/tracedecay release and CI wall-clock.
Stock free GitHub-hosted runners only (`ubuntu-22.04`, `ubuntu-22.04-arm`,
`ubuntu-24.04-arm`, `macos-14`, `windows-latest`). No larger/paid/self-hosted
runners. No extra matrix shards.

Pulled from the Actions Jobs API (`started_at` / `completed_at`) and job logs
on 2026-09-18. Durations are wall minutes on one job, not billable minutes
(public-repo timing API reports 0).

## Verdict

The ~1 hour Zack sees, and the 2.5 hour beta.40 failure, are one Linux job
compiling the same workspace several times in release. Packaging, upload,
dashboard, and rust-cache save are not the wall.

`release.yml` is unused on current prereleases: every recent stable Release
run is skipped. The live path is `release-beta.yml`.

## Workflow wall times

### Release (Beta) — last 15 finished runs

| Run | Tag | Conclusion | Wall min |
| --- | --- | --- | ---: |
| [35271070760](https://github.com/ScriptedAlchemy/tracedecay/actions/runs/35271070760) | v0.1.0-beta.40 | failure | 149.8 |
| 35175625203 | v0.1.0-beta.38 | cancelled | 3.7 |
| 32605144188 | v0.1.0-beta.37 | failure | 105.8 |
| 32597856820 | v0.1.0-beta.36 | failure | 85.7 |
| 32552646147 | v0.1.0-beta.35 | failure | 100.9 |
| 32540109408 … 32417378296 | beta.10–34 | cancelled/failure | 49–509 |

Last successful Release (Beta) in history: run 27839751753 (`v0.0.3-beta.1`,
27.4 min). Not comparable to the current workspace.

### Release (stable)

Every listed recent `release.yml` run is skipped (prerelease tags). The
workflow is a better shape (dashboard built once, skip-mode digest) but it is
not on the current release critical path.

### CI (`ci.yml`)

Recent master/PR runs are almost all cancelled by concurrency. Completed
failures that actually scheduled the heavy jobs:

| Run | Conclusion | Wall min | Critical job |
| --- | --- | ---: | --- |
| [35274248710](https://github.com/ScriptedAlchemy/tracedecay/actions/runs/35274248710) | failure | 240.8 | Build Windows tests 114.3 |
| 35271297793 | failure | 210.3 | Build Windows tests 115.3 |
| 35258386278 | failure | 96.1 | Build Windows tests 70.0 |
| 35257227044 | failure | 136.3 | Build Windows tests 112.8 |

Last successful CI in the API window: 34956004104 (41.8 min). That run is the
pre-partition 15-job shape (`Test Linux` 4.4 min). It is not the current
`ci.yml`.

## Run 35271070760 (v0.1.0-beta.40) — 149.8 min wall

Jobs started together after `validate` (0.3 min). Wall clock is the slowest
job: `Build x86_64-linux`.

| Job | Runner | Result | Job min | Dominant step |
| --- | --- | --- | ---: | --- |
| Build x86_64-linux | ubuntu-22.04 | failure | 149.3 | Test release distribution 143.6 |
| Build aarch64-macos | macos-14 | failure | 127.3 | three serial `cargo build`s, 45.5 + 35.2 + 39.9 |
| Build aarch64-linux | ubuntu-22.04-arm | failure | 73.9 | three serial `cargo build`s, 24.9 + 19.9 + 23.7 |
| Build x86_64-windows | windows-latest | failure | 65.1 | Build release binary for packaging 55.7 |
| validate | ubuntu-22.04 | success | 0.3 | checkout 0.3 |
| publish | ubuntu-22.04 | skipped | 0.0 | — |

Packaging, MCPB, archive verify, and artifact upload never ran. The job died
in compile/test or distribution acceptance.

### `Build x86_64-linux` / `Test release distribution` (143.6 min)

Job 105370471904. Rust cache: **No cache found** at 20:35:04. npm cache:
**hit**, 129 MB.

The step is four serial Cargo invocations in one shell:

1. Two controlled-workload helper release builds (feature-off, then
   feature-on): **9.1 min + 6.2 min** (`Finished` at 20:44:12 and 20:50:24).
2. `cargo nextest` for one ignored hotpath test: compile **24.2 s**, run
   **0.063 s**.
3. `cargo build --workspace --bins --release --all-features` then
   `cargo test --workspace --release --all-features`: first `running 735 tests`
   at 22:57:33, SIGABRT at 22:58:38.

Split:

| Phase | Minutes | Evidence |
| --- | ---: | --- |
| helpers + nextest | 15.8 | 20:35:04 → 20:50:49 |
| workspace bins + test compile | 126.7 | 20:50:49 → 22:57:33 |
| first lib suite until abort | 1.1 | 22:57:33 → 22:58:38 |
| **step total** | **143.6** | API step times |

The 2.5 hour wall is almost entirely one cold `cargo test --workspace
--release --all-features` compile on `ubuntu-22.04`. The job never reaches
the later all-feature verify, packaging build, or distribution acceptance
steps.

Older Linux failures of the same step: 103.5 min (beta.37), 80.0 min
(beta.36). Same shape, smaller tree or warmer runner, still the wall.

### Other platforms on the same run — three serial release compiles

After a cache miss, non-Linux (and ARM Linux) jobs do:

1. `Verify all-feature release build compiles` —
   `cargo build -p tracedecay-cli --bin tracedecay --release --all-features`
   (skipped on Windows).
2. `Build release binary for packaging` —
   `cargo build … --no-default-features --features production`.
3. `Run distribution acceptance` —
   `cargo build --workspace --release --lib --bins` with production features
   inside `scripts/check-distribution-acceptance.sh`.

| Platform | All-features CLI | Production CLI | Dist-acceptance workspace | Dist-acceptance outcome |
| --- | ---: | ---: | ---: | --- |
| aarch64-macos | 45.5 | 35.2 | 39.9 | fail: `lang-bash does not compile in isolation` after the workspace compile |
| aarch64-linux | 24.9 | 19.9 | 23.7 | same `lang-bash` fail after 23.6 min of compile |
| x86_64-windows | skipped | 55.7 | 0.5 | fail: packaged `claude.json` fixture mismatch; no workspace compile |

macOS paid **120.6 minutes of serial cargo** in one job. ARM Linux paid
**68.5**. Windows paid **55.7** for the first compile only.

### `Resolve release source feature profile` (4.3–5.0 min, all four platforms)

This step is two `cargo tree --locked` walks
(`scripts/check-production-feature-profile.py`) and it runs **before**
`Cache Rust build`. On a miss it downloads the index and resolves from
scratch:

| Job | Minutes |
| --- | ---: |
| x86_64-windows | 5.0 |
| aarch64-macos | 4.6 |
| x86_64-linux | 4.3 |
| aarch64-linux | 4.0 |

Older betas (cache warmer or smaller tree) spent 0.2–0.4 min here.

## Cache miss cost

### Release (Beta) rust-cache: miss on every platform (beta.40)

Logs: `No cache found.` Restore lookup 3–6 seconds. Keys:

```
v0-rust-release-beta-<target>-<os>-<hash>
```

`Swatinem/rust-cache` hashed vendor fixture `rust-toolchain.toml` files and
the extra `.release-automation` checkout into the key. Prefix restore also
missed. Last successful beta that could have seeded this lineage is
v0.0.3-beta.1.

Save after failure (`cache-on-failure: true`):

| Platform | Uploaded | Save wall |
| --- | ---: | ---: |
| x86_64-linux | 498 MB | 0.25 |
| x86_64-windows | 835 MB | 1.4 |
| aarch64-macos | 1.13 GB | 0.9 |
| aarch64-linux | 1.20 GB | 0.3 |

Save is not the sink. The miss is: every release compile starts from an empty
`target/`.

npm cache hit on Linux and Windows (90–129 MB, restore < 5 s). Dashboard
build 0.1–0.3 min. Not a sink.

### CI rust-cache: hit, workspace still compiles

`Build Windows tests` on run 35274248710:

- rust-cache **hit**, 637 MB, key
  `v0-rust-ci-test-full-windows-msvc-lld-Windows_NT-x64-893866a7-1d50b224`
- `cache-all-crates: false` (dependency artifacts only)
- `cargo build --workspace --bins --tests --profile perf`: **106.5 min**
  (`Finished … in 106m 27s`)
- same key on run 35258386278: **64.8 min** (`Finished … in 64m 33s`)

That 42 minute spread is runner I/O/CPU variance on `windows-latest`, not a
miss. rust-cache is doing what CI comments say: workspace crates compile
fresh every run.

CI comments already measure a cold dependency miss at ~6 min on Linux
partitions. That is small next to the workspace compile.

## Heaviest CI jobs (current `ci.yml`, run 35274248710)

| Job | Result | Job min | Dominant step | Minutes |
| --- | --- | ---: | --- | ---: |
| Build Windows tests | success | 114.3 | Build workspace binaries and tests | 106.5 |
| Test macOS root-suites | failure | 58.4 | Run the group's partitions | 57.1 |
| Test macOS runtime-storage | failure | 52.9 | Run the group's partitions | 51.6 |
| Test macOS contracts-dashboard-api | failure | 47.3 | Run the group's partitions | 46.1 |
| Test macOS root-lib | failure | 28.7 | Run the group's partitions | 27.4 |
| Test Linux runtime | failure | 25.3 | Run tests | 22.4 |
| Test Linux root-journeys | failure | 21.3 | Build executables + Run tests | 13.5 + 6.8 |
| Hotpath parity | success | 15.2 | Build controlled-workload helpers | 14.8 |
| Feature gates | success | 12.1 | cargo check two feature graphs | 11.4 |
| Build debug CLI | success | 11.7 | cargo build -p tracedecay-cli | 11.0 |
| Clippy | success | 6.4 | clippy 3.5 + lean check 2.4 | — |
| Dashboard | success | 2.1 | npm test 1.6 | — |

Windows test shards are 12–15 min after the 114 min archive. They are not the
wall. macOS groups are 3-vCPU `macos-14` compile+test in one step; comments
in `ci.yml` already treat that compile as irreducible on three cores.

## Ranked single-job time sinks

Minutes are one step on one job. Rank is by minutes removed if that step
stops doing redundant work on a free runner.

| Rank | Minutes | Workflow | Run | Job | Step | What the clock is doing |
| ---: | ---: | --- | --- | --- | --- | --- |
| 1 | **126.7** | Release (Beta) | 35271070760 | Build x86_64-linux | Test release distribution | Cold `cargo build --workspace --bins` + `cargo test --workspace --release --all-features`. First suite starts at 126.7 min; aborts 1.1 min later. This is the 2.5 h wall. |
| 2 | **106.5** (64.8 warm) | CI | 35274248710 / 35258386278 | Build Windows tests | Build workspace binaries and tests | `cargo build --workspace --bins --tests --profile perf` on `windows-latest`. Cache **hit**. Workspace crates always rebuild. |
| 3 | **45.5** | Release (Beta) | 35271070760 | Build aarch64-macos | Verify all-feature release build compiles | First of three serial release CLI/workspace graphs. Cache miss. |
| 4 | **39.9** | Release (Beta) | 35271070760 | Build aarch64-macos | Run distribution acceptance | Third graph: `cargo build --workspace --release --lib --bins` production. Then fail immediately on `lang-bash`. |
| 5 | **35.2** | Release (Beta) | 35271070760 | Build aarch64-macos | Build release binary for packaging | Second graph: production CLI after all-features CLI. |
| 6 | **55.7** | Release (Beta) | 35271070760 | Build x86_64-windows | Build release binary for packaging | Only cargo compile on Windows (all-feature verify skipped). Cache miss. |
| 7 | **24.9 / 19.9 / 23.7** | Release (Beta) | 35271070760 | Build aarch64-linux | same three cargo steps | Same serial triple as macOS, faster cores. Dist acceptance still rebuilds the workspace. |
| 8 | **15.3** | Release (Beta) | 35271070760 | Build x86_64-linux | Test release distribution (helpers) | Two serial release helper builds (9.1 + 6.2) before the workspace test compile. CI already has a dedicated `hotpath-parity` job for this. |
| 9 | **57.1** | CI | 35274248710 | Test macOS root-suites | Run the group's partitions | 3-vCPU compile+test. rust-cache is deps-only. |
| 10 | **4.3–5.0** | Release (Beta) | 35271070760 | all four build jobs | Resolve release source feature profile | Two `cargo tree` walks **before** rust-cache restore. |
| — | 0.1–0.3 | Release (Beta) | 35271070760 | all build jobs | Build dashboard | npm cache hit. Not a sink. |
| — | 0.2–1.4 | Release (Beta) | 35271070760 | all build jobs | Post Cache Rust build | Upload 0.5–1.2 GB. Not a sink. |
| — | never ran | Release (Beta) | 35271070760 | all build jobs | Package / upload | Not on the wall. |

## Observed facts vs inferences

Observed:

- Beta.40 wall = `Build x86_64-linux` = `Test release distribution` = 143.6 min.
- That step is a cold cache plus four serial release Cargo graphs. 126.7 of
  the minutes are workspace test compile.
- macOS/ARM Linux each run three serial release compiles (all-features CLI,
  production CLI, production workspace). Dist acceptance then fails on
  `lang-bash` after paying the third compile.
- rust-cache missed on every beta.40 platform. CI Windows rust-cache hit and
  still compiled 65–106 min of workspace crates.
- Dashboard, npm, cache save, packaging, and upload are minutes or less, or
  never reached.

Inferences for single-job cuts on free runners (not implemented here):

1. Stop running `cargo test --workspace --release --all-features` on the
   Linux packaging job. CI already compiles and runs the suite. That is the
   127 minute / 2.5 hour wall.
2. Stop the two extra serial release compiles on every platform: all-features
   CLI verify and the dist-acceptance workspace rebuild after a production
   CLI already exists in `target/`.
3. Restore rust-cache before `cargo tree`. Stop hashing vendor fixture
   toolchains and the `.release-automation` checkout into the release cache
   key so a miss is not guaranteed on every tag.
4. Do not pay two helper release builds inside the Linux packaging job; CI
   `hotpath-parity` already owns that pair.

None of those add jobs, OS matrix rows, or paid runners.

## Sources

- Workflows: `.github/workflows/release-beta.yml`, `release.yml`, `ci.yml`
- Jobs API: `GET /repos/ScriptedAlchemy/tracedecay/actions/runs/<id>/jobs`
- Logs: jobs 105370471904, 105370471855, 105370471889, 105370471932
  (beta.40); 105380892825, 105327687224 (CI Windows)
- `scripts/check-production-feature-profile.py` (`cargo tree` × 2)
- `scripts/check-distribution-acceptance.sh` (workspace `--release --lib --bins`)

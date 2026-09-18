# Release Workflow Simplification and Speed Plan

## Decision

Simplify the release architecture before tuning rustc or caches.

The beta artifact job should do only the work that proves and produces the
artifact for its target:

1. validate the immutable release source centrally;
2. check out that source once;
3. build the dashboard;
4. compile the production `tracedecay` binary once;
5. package that exact binary as the archive and MCPB;
6. run the packaged binary and upload it; and
7. attest, publish, download, and verify the release assets.

All-feature workspace tests, extracted-crate acceptance, feature-graph
equivalence, and release-script unit tests are confidence gates. They do not
inspect the target archive produced by the build job and should not rebuild
every target after a beta tag has already been published.

## Constraints

- Use only free, stock GitHub-hosted runners already available to the public
  repository.
- Reduce work within each job. Do not add release shards.
- Do not use paid, larger, or self-hosted runners.
- Do not trade direct artifact checks for a green proxy.
- Do not merge from this plan.

## Evidence

The baseline is
[Release Beta run 35271070760](https://github.com/ScriptedAlchemy/tracedecay/actions/runs/35271070760)
for beta.40. All four build jobs failed, so successful-step durations are
measurements while savings beyond each failure point are lower-confidence
estimates.

| Target job | Feature profile | All-feature CLI build | Shipping build | Other heavy gate before failure |
| --- | ---: | ---: | ---: | ---: |
| aarch64 Linux | 4.1m | 24.9m | 19.9m | distribution acceptance 23.7m, then failed |
| aarch64 macOS | 4.6m | 45.5m | 35.2m | distribution acceptance 39.9m, then failed |
| x86_64 Windows | 5.1m | skipped | 55.7m | distribution acceptance failed after 0.5m |
| x86_64 Linux | 4.3m | not reached | not reached | release distribution test 143.6m, then failed |

The census totals the serial release compile work at 68.5m on aarch64 Linux
(24.9 + 19.9 + 23.7) and 120.6m on aarch64 macOS
(45.5 + 35.2 + 39.9). Within the x86_64 Linux step, two Hotpath helper builds
took 9.1m and 6.2m before the workspace test spent 126.7m reaching its first
suite and another 1.1m reaching SIGABRT.

The duplicate release-automation checkout took 0.15-0.25m. Rust cache lookup
took 0.03-0.10m, but every target reported `No cache found`. The profile
resolver repeated two `cargo tree` traversals before cache restore in every
target job and took 3.9-5.1m again in the in-progress beta.41 run.
Dashboard work was 0.1-0.3m and Rust cache save was 0.2-1.4m for
498 MiB-1.2 GiB entries. Neither is a primary wall-clock sink.

Two static properties explain the largest opportunities:

- `Test release distribution` runs an all-feature release build and
  `cargo test --workspace --release --all-features` in the x86_64 Linux
  post-tag artifact job.
- `scripts/check-distribution-acceptance.sh` discovers the runner host with
  `rustc -vV` and performs host builds, packaged-crate tests, an install,
  consumer builds, MCP smoke, and LSP smoke. It does not test the matrix target
  archive that the surrounding job eventually uploads.

The repository already has a pre-tag
`Release PR distribution acceptance (x86_64-linux)` workflow. Its stated
purpose is to stop an unshippable release before release-please creates a tag.
That is the correct lifecycle boundary for pre-release confidence work, though
the beta and stable policies should differ.

Sibling evidence:

- Timing census
  [#1583](https://github.com/ScriptedAlchemy/tracedecay/pull/1583) locates
  126.7m of the x86_64 Linux step in the cold all-feature workspace test
  compile and another 15.3m in two Hotpath helper builds. It also confirms
  that packaging, upload, dashboard, and cache save are not the wall.
- Single-compile draft
  [#1582](https://github.com/ScriptedAlchemy/tracedecay/pull/1582) deletes the
  standalone beta all-feature CLI compile and leaves the production packaging
  build as the sole artifact compile. Its dependency comparison found that the
  production build recompiles all 499 unique production crates because the
  all-feature graph intentionally differs.
- Cache/toolchain draft
  [#1585](https://github.com/ScriptedAlchemy/tracedecay/pull/1585) finds that
  all beta.40 release caches missed. Tag cache scope, a full 10 GiB repository
  cache budget, a floating unused stable toolchain, cargo running before
  restore, and the duplicate checkout all prevent reliable reuse.
- Simpler-release draft
  [#1584](https://github.com/ScriptedAlchemy/tracedecay/pull/1584) independently
  arrives at the one-production-compile beta pipeline and estimates slim cold
  jobs at 25-35m for aarch64 Linux, 25-40m for x86_64 Linux, 40-55m for
  aarch64 macOS, and 55-65m for Windows.
- Broader ship-path implementation
  [#1587](https://github.com/ScriptedAlchemy/tracedecay/pull/1587) proves the
  first 23m34s of aarch64 Linux acceptance was a cold production workspace
  rebuild into implicit `target/release`, while the shipping binary already
  existed under the matrix target directory. Staging and packaging then took
  about three seconds before an isolated language check failed. Its current
  head removes distribution acceptance, the x86_64 Linux workspace release
  tests, the all-feature CLI compile, nextest setup, and macOS acceptance Bash
  from beta and stable ship jobs. It replaces the release-PR gate with one
  daily stock `ubuntu-latest` crate-extract battery plus manual dispatch.

The preceding all-feature build does not warm the production graph enough to
justify its 24.9m Linux and 45.5m macOS cost. Compiler-profile savings remain
hypotheses until measured on the simplified single-build path.

## Target architecture

### Beta

Keep one small source-validation job, the existing target matrix, and the
publish job. Make each target build job a straight artifact pipeline:

```text
validate immutable tag/source
  -> checkout source once
  -> setup Node, Python, Rust, and the existing stock linker
  -> build dashboard
  -> restore target cache
  -> cargo build tracedecay-cli --release --target <target>
       --no-default-features --features production --locked
  -> package the resulting binary into archive and MCPB
  -> extract and run --version and --help from both package forms
  -> upload
  -> attest, publish, download, checksum, and verify
```

Delete from the beta target jobs:

- the `.release-automation` checkout, because it checks out the same immutable
  source SHA a second time;
- portable harness unit tests, which belong in ordinary CI;
- cargo-nextest installation;
- per-target feature-profile graph resolution;
- `Test release distribution`;
- `Verify all-feature release build compiles`;
- `Run distribution acceptance`; and
- the legacy-default historical smoke branch.

Install only the toolchain pinned by `rust-toolchain.toml`, plus the matrix
target. Build the dashboard once in the job, validate its digest, and pass the
existing skip-build digest contract to the CLI build so `build.rs` embeds
those bytes without running the dashboard build again.

The ordinary beta path should accept only the current production release
contract. If recovery of a legacy tag is still required, retain that policy in
an explicitly invoked recovery path rather than charging every current target
job for historical profile discovery.

### Stable

Use the same one-compile artifact pipeline so stable artifacts do not pay for
duplicate compiles either. Stable adds its real channel responsibilities:
npm publication, `server.json`, the `latest` designation, and stable
provenance. It does not regain a heavy compile matrix or block on the
crate-extract battery, because that battery does not stamp the binary,
archive, or MCPB.

This keeps one packaging implementation. Beta remains lighter through its
smaller publication surface, not by making stable repeat unrelated Rust
builds.

### Periodic

Run the full crate-extract battery as one daily stock `ubuntu-latest` job and
retain manual dispatch. Rename and reuse the current release-PR acceptance
implementation; this is deferred work, not another release shard.

Periodic work owns:

- packaging every workspace crate;
- extracted library, CLI, query, LSP, MCP, install, and consumer tests; and
- production/default feature-graph equivalence.

The periodic run is visibility, not evidence that a later tagged SHA passed.
Existing required CI owns workspace tests and Hotpath parity. Beta and stable
ship paths rely on that CI plus direct checks of every produced artifact; they
do not wait for the periodic crate-extract result.

Expected cold target-job envelopes after A, before compiler or cache wins:

| Target | Beta.40 failed job | Slim artifact job |
| --- | ---: | ---: |
| aarch64 Linux | 74m | 25-35m |
| x86_64 Linux | 149m | 25-40m |
| aarch64 macOS | 127m | 40-55m |
| x86_64 Windows | 65m | 55-65m |

Windows remains dominated by the one shipping compile. It is the first target
for B, not a reason to keep general acceptance in the artifact job.

## Ranked cuts

Savings are per affected job on the existing stock runners. They are not all
additive: removing an all-feature build can make the remaining production
build colder.

### A. Simplify workflow shape

#### A1. Remove general release testing from post-tag beta jobs

Delete the duplicate workspace release test from the ship path and move deep
crate-extract distribution acceptance to the one periodic battery. Existing CI
already owns workspace tests and Hotpath parity.

Estimated cut:

- x86_64 Linux: 100-120m net. The removed test step consumed 143.6m before
  failing: 15.3m in Hotpath helper builds and 126.7m before the first workspace
  test suite started. A cold production artifact build will replace part of
  that time.
- aarch64 Linux: at least 24m from distribution acceptance.
- aarch64 macOS: at least 40m from distribution acceptance.
- x86_64 Windows: unquantified. The observed 0.5m was only an early failure,
  not a healthy acceptance run.

PR #1587 is the preferred A implementation because it makes this cut
coherently across beta, stable, and release-PR workflows while preserving one
periodic crate-extract authority. Its nightly has not yet run, so the move is
implemented but the periodic production journey remains unverified.

#### A2. Compile exactly one shipping feature set per target

Delete the all-feature CLI release build from artifact jobs. Compile
`tracedecay-cli` once with production features and package that exact output.
All-feature compile coverage remains in CI and the heavy battery.

Estimated cut, supported by the beta.40 timings and the sibling dependency
comparison:

- aarch64 Linux: 24.9m;
- aarch64 macOS: 45.5m;
- x86_64 Windows: 0m because beta.40 already performed one binary build; and
- x86_64 Linux: unquantified until A1 lets the shipping build complete.

PR #1587 subsumes PR #1582's beta all-feature deletion and applies the same
rule to stable. Do not land #1582 separately if #1587 is selected.

#### A3. Remove release-time feature-graph resolution

For current releases, pass the reviewed production arguments directly. Run the
two-graph equivalence proof in ordinary CI and the heavy battery, once per
source rather than once per target.

Estimated cut: 4.0-5.1m from every target job.

This should land in the architecture change, not as a faster custom resolver.
The target argument is currently ignored by
`production_release_features`, so repeating the proof per target provides no
target-specific evidence.

#### A4. Remove duplicate source checkout and repeated harness self-tests

Invoke release scripts from the immutable source checkout. Keep script unit
tests in CI; release jobs should execute the reviewed scripts, not retest their
parsers four times.

Estimated cut: 0.4-1.0m per target. The direct checkout saving is only
0.15-0.25m; the larger benefit is fewer serial steps and fewer checkout/path
authorities.

Keep the dashboard build in beta for now. It costs seconds and its bytes are
embedded into the binary. Centralizing it would add artifact ceremony for
little critical-path gain.

#### A5. Pin one Rust toolchain and embed the already-built dashboard

Install the `rust-toolchain.toml` channel rather than an additional floating
`stable`, and pass the validated dashboard digest to the CLI build's existing
skip-build contract.

Estimated cut: 0.5-1.0m per target from avoiding a second dashboard build.
Toolchain installation itself saves approximately 0m, but removing unused
rustc 1.98.1 from a rustc 1.97.1 build stops avoidable cache-key drift.

### B. Compile and link cuts

#### B1. Measure the single production build before changing release profile

After A lands, capture Cargo timings for the one production build on Windows,
macOS, and both Linux targets. Rank crates by code generation and link time,
then remove production features or dependencies that the shipped CLI does not
call.

Estimated cut to pursue: 5-15m on the 55.7m Windows build and 3-10m on Unix.
These are targets for measured dependency/feature pruning, not established
savings. The census also observed the ordinary Windows workspace build at
106.5m and 64.8m on a warmer runner despite a 637 MiB Rust cache hit, while the
macOS root-suites job took 57.1m. Those CI measurements reinforce that compile
graph size and stock-runner variance remain B concerns after release
duplication is removed; they are not additive release savings.

Do not begin by changing optimization semantics. The workspace has no custom
release LTO setting, release debuginfo is already disabled, Linux already uses
mold, and Windows already uses `lld-link`. Disabling nonexistent LTO or
installing another linker cannot explain the baseline.

#### B2. Trial higher release codegen parallelism only with product evidence

The default release profile uses Cargo's default 16 codegen units. A stock
runner trial may compare 16 with a higher value if B1 shows LLVM codegen, not
dependency compilation, dominates.

Estimated cut to pursue: 3-10m on Windows and macOS; no claimed cut until the
same binary-size, startup, representative query, and indexing checks pass.
Reject the change if runtime regression buys CI speed.

Land B only after the architecture baseline. Otherwise duplicate builds and
acceptance noise will obscure the treatment.

### C. Cache

#### C1. Make the existing cache eligible to hit

Apply the key hygiene from PR #1585: use only the pinned toolchain, remove the
duplicate checkout from the hash inputs, and restore before the first Cargo
command. Keep one target/profile cache key and reassess it after feature-set
consolidation. Do not cache final release binaries across tags; the binary
embeds and reports the source SHA and must be produced from the tagged source.

Estimated direct cut: 0-1m after A removes the 4m pre-cache `cargo tree`
operation. A compatible default-branch dependency cache could avoid an
estimated 6-20m on Linux, and potentially more on macOS, but beta.40 provides
no hit measurement. GitHub cannot restore a previous tag's cache into a new
tag, and the current repository cache inventory already exceeds the 10 GiB
budget before eviction.

Reuse a compatible cache written by existing required master work only if its
profile and key already match. Do not add a cache-warming compile or another
job merely to manufacture a hit.

Do not add sccache, remote cache infrastructure, self-hosted runners, or paid
cache services.

## What must stay

- Immutable tag, source SHA, version, release-channel, and retained-asset
  validation.
- The four required distribution targets on their existing free stock runners.
- One production release compile for each target.
- Existing mold and `lld-link` verification.
- Dashboard bytes embedded by the canonical CLI build.
- Source-SHA and version checks on the produced binary.
- Archive and MCPB construction from the exact shipping binary.
- Extraction and runtime smoke of both package forms.
- Artifact upload with `if-no-files-found: error`.
- Attestation, checksum generation, release upload, download, and remote
  artifact verification.
- Fail-closed recovery planning and byte verification of retained assets.

## What not to do

- Do not add shards or split crates across more release jobs.
- Do not use paid, larger, or self-hosted runners.
- Do not move compilation to an unverified binary cache.
- Do not package an all-feature binary containing test-only transport.
- Do not preserve a second checkout as a shadow release-script authority.
- Do not raise timeouts to conceal the current workload.
- Do not weaken artifact extraction, execution, checksum, provenance, or
  retained-asset checks.
- Do not tune cache before deleting duplicate work.

## Landing order

1. Prefer PR #1587 as the coherent A slice. It subsumes #1582, removes the
   largest measured sinks from beta and stable, and moves the crate-extract
   journey to one daily free runner without adding release shards.
2. Do not land PR #1582 separately after #1587; both delete the beta
   all-feature verify step.
3. Reconcile the non-overlapping no-second-checkout, pinned-toolchain,
   dashboard-digest, and cache-order parts of PR #1585 onto the simplified
   target shape. Treat cache hits as unverified until a compatible
   default-branch writer and a release restore are both observed.
4. Do not land the sibling timing and architecture docs as parallel plan
   authorities. This document consolidates #1583 and #1584.
5. Run the next Release Beta and establish successful single-build timings for
   all four targets.
6. Land measured B changes one at a time, starting with the Windows production
   graph because its single build was 55.7m.
7. Re-evaluate C only after two comparable successful runs show persistent
   dependency recompilation.

## Verification on the next Release Beta

Use the next successful `Release (Beta)` run on the same stock runner labels.
Compare it with run 35271070760 by target and step, not only by whole-workflow
duration.

Record for each build job:

- queue time, job start, and job completion;
- Rust cache key, restore result, and duration;
- rustc versions included in the cache environment;
- start and completion of the sole production `cargo build`;
- archive and MCPB package/verification duration;
- artifact upload duration; and
- total build-job duration.

Acceptance criteria:

1. Each target log contains one production CLI release build and no
   all-feature CLI build, workspace release test, or deep distribution
   acceptance.
2. The archive and MCPB both contain the binary from that build.
3. Extracted package runtime checks report the release version and source SHA.
4. All four target artifacts upload, the publish job attests them, and the
   downloaded release assets pass checksum and manifest verification.
5. The pre-rust profile-resolution delay falls from 3.9-5.1m to zero.
6. aarch64 Linux and macOS no longer spend the measured 23.7m and 39.9m in
   host distribution acceptance.
7. x86_64 Linux no longer spends 143.6m in the all-feature workspace release
   test.
8. Any compile-profile follow-up reports its own before/after production-build
   timing and product runtime evidence; architecture and compiler treatments
   are not combined in one timing claim.

Beta.40 was cold on every target. If the next run restores a cache, report that
as a separate treatment and do not attribute its whole delta to the workflow
cut. A successful end-to-end release with slower target compilation is still
valid evidence for the architecture; compile/link and cache savings require
their own comparable treatments.

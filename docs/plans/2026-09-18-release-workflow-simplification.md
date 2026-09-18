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

The duplicate release-automation checkout took 0.15-0.25m. Rust cache restore
took 0.03-0.10m. The profile resolver repeated two `cargo tree` traversals in
every target job and took 3.9-5.1m again in the in-progress beta.41 run.

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

Draft PR
[#1582](https://github.com/ScriptedAlchemy/tracedecay/pull/1582) supplies the
first sibling result. It deletes the standalone beta all-feature CLI compile
and leaves the production packaging build as the sole artifact compile. Its
dependency comparison found that the production build recompiles all 499
unique production crates because the all-feature graph intentionally differs.
The preceding all-feature build therefore does not warm the production graph
enough to justify its 24.9m Linux and 45.5m macOS cost. No other open PR title
matched `CI speed` or `release speed` at the last refresh, so the remaining
compiler and cache recommendations distinguish measurements from hypotheses.

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

The ordinary beta path should accept only the current production release
contract. If recovery of a legacy tag is still required, retain that policy in
an explicitly invoked recovery path rather than charging every current target
job for historical profile discovery.

### Stable

Use the same one-compile artifact pipeline so stable artifacts do not pay for
duplicate compiles either. Stable remains stricter before tagging:

- a stable release PR must pass the heavy x86_64 Linux release battery on a
  free stock runner;
- the battery may include all-feature workspace tests and the full
  extracted-crate distribution acceptance journey; and
- the post-tag stable target jobs still perform only direct target-artifact
  work.

This makes beta lighter without making stable artifacts follow a separate
packaging implementation.

### Periodic

Run the full release battery as one periodic x86_64 Linux job and retain manual
dispatch. Reuse the current release-PR acceptance implementation instead of
creating a second battery.

Periodic or stable-pre-tag work owns:

- all-feature workspace release build and tests;
- controlled-workload Hotpath parity;
- packaging every workspace crate;
- extracted library, CLI, query, LSP, MCP, install, and consumer tests; and
- production/default feature-graph equivalence.

The periodic run is visibility, not evidence that a later tagged SHA passed.
Stable still requires its pre-tag battery. Beta relies on ordinary required CI,
the most recent periodic signal, and direct checks of every produced artifact.

## Ranked cuts

Savings are per affected job on the existing stock runners. They are not all
additive: removing an all-feature build can make the remaining production
build colder.

### A. Simplify workflow shape

#### A1. Remove general release testing from post-tag beta jobs

Move `Test release distribution` and deep distribution acceptance to the
periodic/stable-pre-tag battery.

Estimated cut:

- x86_64 Linux: 100-120m net. The removed test step consumed 143.6m before
  failing; a cold production artifact build will replace part of that time.
- aarch64 Linux: at least 24m from distribution acceptance.
- aarch64 macOS: at least 40m from distribution acceptance.
- x86_64 Windows: unquantified. The observed 0.5m was only an early failure,
  not a healthy acceptance run.

Land this first, together with the explicit beta/stable/periodic ownership
above. It removes the most work and places failures before tagging or directly
against the artifact they concern.

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

PR #1582 is the ready beta implementation slice. Apply the same one-compile
rule to stable after its beta release evidence is green.

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

### B. Compile and link cuts

#### B1. Measure the single production build before changing release profile

After A lands, capture Cargo timings for the one production build on Windows,
macOS, and both Linux targets. Rank crates by code generation and link time,
then remove production features or dependencies that the shipped CLI does not
call.

Estimated cut to pursue: 5-15m on the 55.7m Windows build and 3-10m on Unix.
These are targets for measured dependency/feature pruning, not established
savings.

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

#### C1. Align the existing Rust cache with the one production build

Keep one target/profile cache key and reassess it after feature-set
consolidation. Do not cache final release binaries across tags; the binary
embeds and reports the source SHA and must be produced from the tagged source.

Estimated cut: 0-5m on a warm run. Beta.40 restored each Rust cache in 2-6
seconds yet still spent 20-56m compiling, so cache work is not the first lever.

Do not add remote cache infrastructure, self-hosted runners, or paid cache
services.

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

1. Land the focused one-compile beta deletion from PR #1582. It is the
   smallest ready part of A and removes 24.9-45.5m from affected target jobs.
2. Land the rest of A as a coherent workflow cut: define the light beta path,
   keep the heavy battery periodic and stable-pre-tag, and remove post-tag
   general testing and repeated release-time validation.
3. Run the next Release Beta and establish successful single-build timings for
   all four targets.
4. Land measured B changes one at a time, starting with the Windows production
   graph because its single build was 55.7m.
5. Re-evaluate C only after two comparable successful runs show persistent
   dependency recompilation.

## Verification on the next Release Beta

Use the next successful `Release (Beta)` run on the same stock runner labels.
Compare it with run 35271070760 by target and step, not only by whole-workflow
duration.

Record for each build job:

- queue time, job start, and job completion;
- Rust cache restore result and duration;
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

If the next run is cold while beta.40 was warm, report that difference and do
not attribute the whole delta to the workflow cut. A successful end-to-end
release with slower target compilation is still valid evidence for the
architecture; compile/link savings require their own comparable treatment.

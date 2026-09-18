# Release build profile measurement

Answers items B1 and B2 of
[`docs/plans/2026-09-18-release-workflow-simplification.md`](plans/2026-09-18-release-workflow-simplification.md):
capture Cargo timings for the one production build that remains on the beta
ship path after #1587, rank the units, and decide whether the release profile
is worth changing.

**Outcome: no Cargo profile setting is worth changing for wall clock.** The
build is bound by the volume of code rustc must monomorphize and lower, not by
optimization level, LTO, or codegen parallelism. Every profile knob was
measured across its useful range and none of them moved the build by more than
noise.

## What the release profile is today

The workspace declares no `[profile.release]`. The shipping build therefore
runs on Cargo's stock release defaults:

| Setting | Effective value |
| --- | --- |
| `opt-level` | `3` |
| `lto` | `false` (thin-local LTO, per codegen unit) |
| `codegen-units` | `16` |
| `incremental` | `false` |
| `debug` | `0` |
| `panic` | `"unwind"` |
| `strip` | `"none"` |

There is no fat LTO and no `codegen-units = 1` to remove. `[profile.bench]`
does exist, but it does not apply here: `cargo test --release` selects
`profile.release` for test targets, verified with `-C opt-level` on a probe
crate whose `bench` and `release` opt-levels differ.

## Method

Measured on a 4 vCPU / 16 GiB Linux host, the same shape as a free
GitHub-hosted `ubuntu-latest` runner, with mold 2.41.0 as the default `ld` (as
`.github/actions` configures on Linux) and a cold `target/`. The command is the
one the beta job runs, from `scripts/resolve-release-source-profile.py`:

```
cargo build --package tracedecay-cli --bin tracedecay --release \
  --target x86_64-unknown-linux-gnu --no-default-features --features production --locked
```

The measured 18m12s baseline is consistent with beta.40's aarch64 packaging
build (19.9m, [run 35271070760](https://github.com/ScriptedAlchemy/tracedecay/actions/runs/35271070760)).

## Results

| Configuration | Wall | Delta |
| --- | ---: | ---: |
| Baseline (stock release defaults, `opt-level = 3`) | **18m 12s** | — |
| `opt-level = 2` for every unit | **18m 09s** | −0.3% |

The serial `tracedecay` → `tracedecay-cli` tail, re-measured on its own by
touching `crates/tracedecay/src/lib.rs` so only those two units rebuild:

| Configuration | Tail wall | Delta |
| --- | ---: | ---: |
| `tracedecay` at `opt-level = 3`, on the baseline tree | **4m 36s** | — |
| `tracedecay` at `opt-level = 1`, on the `opt-level = 2` tree | **4m 30s** | −2% |

The two tail runs sit on different bases, because each full tree had to be built
to measure it, so `tracedecay-cli` is at 3 in the first row and 2 in the second.
That is immaterial at the resolution being argued: the global 3-to-2 comparison
above is already a wash, so the row-to-row difference is `tracedecay` itself
going from 3 to 1, and it returns six seconds of 276. The binary also grew, from
503.0 MiB for the whole tree at 2 to 516.8 MiB with the root dropped to 1.
Optimization level is not the cost.

## Why the profile cannot help

**The build is codegen-bound, and codegen volume barely tracks opt-level.**
Across the 783 units, `--timings` sections report 47.6 minutes of codegen
against 7.8 minutes of frontend: 86% of sectioned unit time is LLVM. LLVM's
work here is lowering 380,869 symbols, and `opt-level` changes how hard it
optimizes each one, not how many there are.

**The cost is in workspace crates, so dependency overrides have almost no
headroom.** 48.5 of 60.1 unit-minutes are TraceDecay's own 61 units; all 722
dependency units together are 11.6 unit-minutes. A
`[profile.release.package."*"]` override can only ever reach that 11.6.

**The tail already saturates the runner, so `codegen-units` cannot help.**
Sampling `/proc/stat` every 5s through the 4m36s tail gives a mean of 77% busy,
3.08 of 4 cores. The stretches that sit at 25% are single-threaded rustc
frontend and the final link, neither of which responds to more codegen units.
Raising `codegen-units` above the default 16 has nothing to reclaim on a
four-core runner and would cost cross-unit inlining. **B2 is rejected on this
evidence; do not run the trial.**

**`panic = "abort"` is unavailable.** Production code recovers through
`catch_unwind` / `resume_unwind` in the rusqlite runtime (transaction guards and
the writer worker), the lexical projection artifact builder, daemon shutdown,
and the git watcher. Aborting would convert recoverable states into process
death.

**Grammar features are not a build-time lever.** All 148 tree-sitter and C
units together are under 5 unit-minutes. Narrowing the `full` language tier
would shrink the artifact substantially (see below) and save roughly nothing on
the clock.

## Where the bytes are

`x86_64-unknown-linux-gnu` release binary, 503.0 MiB:

| Section | Size |
| --- | ---: |
| `.text` | 218.1 MiB |
| `.rodata` | 133.8 MiB |
| `.strtab` + `.symtab` | 107.4 MiB |
| `.eh_frame` + `.gcc_except_table` | 31.4 MiB |

Code bytes by instantiating crate (224.8 MiB over 380,869 symbols):

| Instantiating crate | Size | Symbols |
| --- | ---: | ---: |
| `core` | 40.1 MiB | 131,854 |
| `serde_json` | 30.4 MiB | 35,738 |
| `serde` | 29.7 MiB | 13,882 |
| `alloc` | 15.9 MiB | 36,130 |
| `tracedecay-domain` | 12.0 MiB | 29,558 |
| `tracedecay-contracts` | 8.8 MiB | 17,806 |
| all `tracedecay-*` | 72.3 MiB | — |

`serde` and `serde_json` alone are 60.1 MiB, 27% of all code, and the `core` and
`alloc` instantiations above them are largely driven by the same generic
surface. This is the same effect the `[profile.dev.package.tracedecay-daemon-protocol]`
comment already records at debug scale ("serde+DTO glue", 2.2M instructions),
now measured for the whole graph at release.

`.rodata` is a separate story: 112.9 MiB of it is tree-sitter C parse tables
across 3,630 symbols. Those tables cost under 5 unit-minutes to compile but
dominate the shipped file.

The published archive grew from 38 MiB at `v0.0.74` to 112 MiB at
`v0.1.0-beta.14` through `beta.17`, which matches a local `tar.gz` of the
current binary at 117.4 MiB.

## Ranked proposals

Minutes are for one `Build x86_64-linux` job on a free `ubuntu-latest` runner,
after #1587.

| Rank | Change | Build minutes saved | Other effect | Confidence |
| --- | --- | ---: | --- | --- |
| 1 | `[profile.release] strip = "symbols"` | **~0** (strip itself costs 0.8s) | Binary 503.0 → 395.7 MiB, archive 117.4 → 105.3 MiB. Cuts upload, attest, download, and verify transfer across 4 targets × 2 assets. Costs legible symbol names in production backtraces. | Measured. Needs a call on backtrace legibility for a beta. |
| 2 | Nothing else in the profile | 0 | — | Measured; see rejections above. |

There is no rank 3. `opt-level`, `codegen-units`, `lto`, `panic`, dependency
overrides, and grammar feature pruning were each measured or ruled out above and
none of them returns minutes.

## What would actually cut the build

Not a profile change. The 18 minutes are 380,869 monomorphized symbols, and the
largest single contributor is the serde/DTO surface. Reducing it is a code
change with its own design cost, and none of it should be attempted as a
release-lane tweak:

- Collapse generic serde instantiations on the widest DTO boundaries
  (`tracedecay-domain`, `tracedecay-contracts`, `tracedecay-daemon-protocol`)
  so one monomorphization serves many call sites.
- Narrow the `full` language tier if 112.9 MiB of parse tables is not worth its
  download weight. This is an artifact-size decision, not a build-time one.

Treat the beta ship path as finished for profile tuning. After #1587 the job is
one production compile plus packaging; the compile is irreducible without
changing what is compiled.

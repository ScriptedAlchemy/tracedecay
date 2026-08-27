# Vendored dependency provenance — grafeo

TraceDecay's graph boundary (`crates/tracedecay-graph-db`) sits on grafeo. Three
of grafeo's crates are vendored here and wired in through `[patch.crates-io]` in
the workspace root `Cargo.toml`, because the published 0.5.42 release cannot
serve TraceDecay's tiered-storage path without local fixes.

`grafeo-adapters` and `grafeo-storage` are **not** vendored. They stay on
crates.io and pick the patched crates up transitively — `cargo tree -p
tracedecay-graph-db` shows `grafeo-adapters v0.5.42` depending on
`grafeo-core v0.5.42 (…/vendor/grafeo/grafeo-core)`.

## Source

| field | value |
| --- | --- |
| upstream project | Grafeo |
| upstream URL | <https://github.com/GrafeoDB/grafeo> |
| upstream revision | `3bc891129ddec91b97a0613c9032b06d489695c4` (from each crate's `.cargo_vcs_info.json`) |
| version | 0.5.42 |
| retrieved from | the crates.io registry cache, i.e. the published `.crate` payloads |
| retrieved | 2026-08-27 |
| licence | Apache-2.0 — see `LICENSE-APACHE` |

The published `.crate` payloads carry no licence file of their own; the manifests
declare `license = "Apache-2.0"`. `LICENSE-APACHE` here is the canonical
Apache License 2.0 text, added to satisfy the redistribution term.

Integrity anchors — the crates.io checksums of the exact payloads these trees
were expanded from. They are also the `checksum` lines that this vendoring
removed from `Cargo.lock`, so the lockfile diff is itself a second witness:

| crate | sha256 of the published `.crate` |
| --- | --- |
| `grafeo-common` | `b5f446c25eeedab9cccaabc85060dfa08dac2d1041974244864f36436b81ca5a` |
| `grafeo-core` | `9e185dd750843637a2d99e56100c08ef4cc34ce4a7b5f9cb9566f873bbf54c90` |
| `grafeo-engine` | `ede967e6b0a16396c91752febdf037ab04ca69b851f6fb43565ee5f30586ee0d` |

The first commit in this directory is the pristine expansion of those three
payloads, with only three per-crate files dropped: `.cargo-ok` (a cargo cache
marker), and `Cargo.lock` (a packaged lock that a path dependency never reads).
`Cargo.toml.orig` and `.cargo_vcs_info.json` are kept — the first shows what the
upstream workspace manifest looked like before cargo normalised it, the second
pins the revision. **Every local edit therefore shows up as a diff against that
first commit**, which is what makes this tree auditable without re-downloading.

## Why a fork rather than an upstream fix

The owner's call: patch in-repo, do not upstream. The defects sit in a feature
(`tiered-storage`) that TraceDecay is the one exercising, the fixes are small and
local, and waiting on a release would block the memory work.

## Not a workspace member

These crates are listed in the root `[workspace] exclude`, not in `members`.
They are reached only through `[patch.crates-io]`, matching how
`vendor/tree-sitter-rust` is wired. Consequences worth knowing:

* `cargo test -p grafeo-common` from the repo root does **not** work — a patched
  package is in the dependency graph but is not a member.
* To run a vendored crate's own suite, run cargo from inside its directory. The
  `exclude` entry is what lets cargo treat it as a standalone package there
  instead of refusing to build a non-member.
* Their dev-dependencies never enter the workspace lockfile, so `--workspace`
  builds, clippy runs, and CI surface are unchanged by the vendoring.

## Local patches

None yet in this commit — this is the pristine 0.5.42 expansion. The patch
commits that follow each append their entry here.

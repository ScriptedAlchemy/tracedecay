# `tracedecay-large-treesitters` — Rename Constraints

> Finding for Kanban task `t_4070b0b0`: why the grammar bundle dependency must keep
> its legacy `tracedecay-*` name after the tracedecay rebrand, what would have to
> change before a rename is safe, and the wording to record in the compatibility
> policy. Every claim below is backed by a `file:line` reference.

## TL;DR

**Do not rename `tracedecay-large-treesitters` (or its `-medium` / `-lite` siblings)
for the foreseeable future.** The name is owned and published by an **external
upstream** (`aovestdipaperino`), the three crates form a hard internal dependency
chain whose names are baked into the upstream's *own* manifests, and the package is
public on crates.io with external consumers. Renaming would require forking and
permanently maintaining the entire three-tier grammar pipeline — which directly
contradicts the project's "never push to / fork the aovestdipaperino upstream"
policy. The real long-term exit is dropping these deps entirely via the Rust-parser
migration (`docs/RUST-PARSER-MIGRATION.md`), not renaming them.

## What the dependency actually is

| Aspect | Value | Source |
|---|---|---|
| Declared in tracedecay | `tracedecay-large-treesitters = { version = "0.5.0", git = "https://github.com/aovestdipaperino/tracedecay-large-treesitters" }` | `Cargo.toml:108` |
| Source | **git** (upstream), pinned to rev `0fc87352…` | `Cargo.lock:4472-4475` |
| Rust crate / lib name | `tracedecay_large_treesitters` | imports at `src/extraction/ts_provider.rs:27`, `src/extraction/markdown_extractor.rs:70,139` |
| Internal dep chain | `large` → depends on `tracedecay-medium-treesitters` → depends on `tracedecay-lite-treesitters` | `Cargo.lock:4480`, `Cargo.lock:4531` |
| Source of `medium` / `lite` | **crates.io registry** (0.2.0 each), not git | `Cargo.lock:4509-4512`, `Cargo.lock:4525-4529` |
| Public registry history | Published as `0.3.2`, `0.4.0` on crates.io before the switch to a git pin | `CHANGELOG.md:530,552` |
| Build-script coupling | Upstream's build script compiles vendored C/C++ grammars under `.cargo/config.toml` `CFLAGS=-DNDEBUG` | `CHANGELOG.md:582-583` |
| API surface tracedecay consumes | `all_languages()`, `markdown::LANGUAGE`, `markdown::inline::LANGUAGE` | `ts_provider.rs:27`, `markdown_extractor.rs:70,139` |

The project *itself* rebranded tracedecay → tracedecay (lib/bin name is `tracedecay`,
`Cargo.toml:97,100`; version reset at `CHANGELOG.md:12`) and carries extensive
pre-rebrand compatibility shims (legacy `.tracedecay/` dir at `src/config.rs:16`,
legacy archive names at `src/cloud.rs:140`, legacy `TRACEDECAY_*` env at
`src/config.rs:152`). **Only the grammar-bundle dependency retains the legacy
`tracedecay-` prefix — and that is intentional** (`AGENTS.md:20`).

## Why it must keep the name (constraints)

1. **External upstream ownership.** The crate name, the git repo URL, the crates.io
   package, the build script, and the vendored grammars are all owned by
   `aovestdipaperino`. `AGENTS.md:20` is explicit: *"never push or open PRs to the
   aovestdipaperino upstream; only the tracedecay-large-treesitters dependency
   intentionally stays pointed at upstream."* We cannot unilaterally change the
   crate's published name.

2. **Three-tier name coupling in the upstream's own manifests.** The `large` crate
   declares a dependency named `tracedecay-medium-treesitters` (`Cargo.lock:4480`),
   which in turn declares `tracedecay-lite-treesitters` (`Cargo.lock:4531`). You
   cannot rename `large` in isolation — the names are part of the upstream's
   internal `Cargo.toml`, not ours.

3. **Public registry package with external consumers.** `tracedecay-large-treesitters`
   is a public crates.io crate (`CHANGELOG.md:530,552`). Other downstream users
   depend on the exact name; a tracedecay-published rename would be a *different*
   package.

4. **`package = "..."` aliasing does not solve this.** Cargo's dependency-rename
   field only changes how *tracedecay* refers to the crate locally. The crate
   identity on crates.io and the git repo remain `tracedecay-large-treesitters`, and
   the `large → medium → lite` registry names are still fixed. It is cosmetic, not a
   rename.

5. **Forking cost is high and policy-violating.** To rename we would have to fork
   the entire three-tier set, rename all three crates coherently, republish to
   crates.io, and permanently maintain: the grammar build scripts, the vendored
   C/C++ grammars, the `.cargo/config.toml` `NDEBUG` build-flag interplay
   (`CHANGELOG.md:582-583`), and grammar-set updates on every language addition.
   This contradicts the no-fork policy in (1).

6. **API is defined upstream.** tracedecay consumes `all_languages()`,
   `markdown::LANGUAGE`, and `markdown::inline::LANGUAGE` — names and shapes we do
   not control (`ts_provider.rs:27`, `markdown_extractor.rs:70,139`).

## What would have to change before a rename is safe

A rename becomes viable only under one of these two paths:

- **Upstream-driven rename (preferred).** `aovestdipaperino` renames all three crates
  consistently — git repo, crates.io packages, and internal `Cargo.toml`
  references — and publishes new versions. tracedecay then bumps its dep line and
  the three import sites. Zero fork maintenance.
- **Controlled fork.** ScriptedAlchemy forks the full `large`/`medium`/`lite` set,
  renames all three coherently, republishes under `tracedecay-*-treesitters`, and
  permanently owns the grammar build pipeline (build scripts, vendored grammars,
  `NDEBUG` interplay, grammar additions). This must be approved as an explicit
  policy change to `AGENTS.md:20` *before* starting.

In **either** path, the mechanical changes in tracedecay itself are the same:
update `Cargo.toml:108` (and any sibling `-medium`/`-lite` references that may
appear if we ever declare them directly), the `Cargo.lock` pin, the three import
sites (`ts_provider.rs:27`, `markdown_extractor.rs:70,139`), and any CI/release
scripts that hardcode the `aovestdipaperino/tracedecay-large-treesitters` git URL.

**Real long-term exit:** `docs/RUST-PARSER-MIGRATION.md:219` plans to *drop* the
`tracedecay-large/medium/lite` dependencies entirely in favour of in-tree Rust
parsers. Eliminating the dependency removes the naming question outright and is the
recommended end state rather than a rename.

## Recommended compatibility-policy wording

Add the following (verbatim or adapted) to the compatibility policy / `AGENTS.md`
`Learned Workspace Facts`, expanding on the existing one-liner at `AGENTS.md:20`:

> **`tracedecay-large-treesitters` is an external upstream dependency and must not be
> renamed, vendored, or forked.** The crate name, git repo
> (`aovestdipaperino/tracedecay-large-treesitters`), crates.io publication, build
> script, and vendored grammars are owned by the upstream and intentionally kept on
> the legacy `tracedecay-*` name despite the tracedecay rebrand. The `large → medium
> → lite` tier names are coupled inside the upstream's own manifests, so the three
> crates rise and fall together. Renaming is only safe if (a) the upstream renames
> all three crates and republishes, or (b) the project explicitly adopts a
> maintained fork as a policy change. Until then, keep the exact name in
> `Cargo.toml`, keep import sites as `tracedecay_large_treesitters::*`, and prefer the
> Rust-parser migration (`docs/RUST-PARSER-MIGRATION.md`) as the path to eventually
> removing the dependency rather than renaming it.

## Verification trail

- `Cargo.toml:108` — git dep declaration (current, `0.5.0`, aovestdipaperino).
- `Cargo.lock:4472-4506` — `large` 0.5.0 git source + deps incl. `tracedecay-medium-treesitters`.
- `Cargo.lock:4509-4523` — `lite` 0.2.0 from crates.io.
- `Cargo.lock:4525-4531` — `medium` 0.2.0 from crates.io, depends on `lite`.
- `src/extraction/ts_provider.rs:27` — `tracedecay_large_treesitters::all_languages()`.
- `src/extraction/markdown_extractor.rs:70,139` — `markdown::inline::LANGUAGE` / `markdown::LANGUAGE`.
- `CHANGELOG.md:530,533,552` — prior crates.io publication history (0.3.2 / 0.4.0).
- `CHANGELOG.md:905` — confirms `large` includes `medium` + `lite`.
- `CHANGELOG.md:582-583` — upstream build-script / `NDEBUG` coupling.
- `Cargo.toml:97,100` + `CHANGELOG.md:12` — project rebrand to `tracedecay`.
- `AGENTS.md:20` — existing upstream / no-fork policy statement.
- `docs/RUST-PARSER-MIGRATION.md:69,219` — vendored-grammar ownership + planned drop of the deps.

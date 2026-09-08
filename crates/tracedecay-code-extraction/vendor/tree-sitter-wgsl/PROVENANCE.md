# Vendored grammar provenance — tree-sitter-wgsl

`src/parser.c`, `src/scanner.c`, and `src/tree_sitter/parser.h` are the
generated WGSL grammar sources that `crates/tracedecay-code-extraction/build.rs`
compiles under the `lang-wgsl` feature and that `src/ts_provider.rs` binds
through `tree_sitter_language::LanguageFn`.

## Source

| field | value |
| --- | --- |
| upstream project | tree-sitter-wgsl (Mehmet Oguz Derin) |
| upstream repository | <https://github.com/mehmetoguzderin/tree-sitter-wgsl> |
| imported from | crates.io package `tree-sitter-wgsl` 0.0.6 (published 2022-06-13; latest release, the repository has no tags) |
| package sha256 | `3e6ab6ce5c5817f198d0e2b36ad501daa48798db5b1602187ac3fd6aaac938e3` |
| package VCS commit (`.cargo_vcs_info.json`) | `27ae4db1458d5ea7555300c9e93c6273723d420c` |
| generator | tree-sitter CLI 0.20 line (`LANGUAGE_VERSION 13` in `parser.c`); the generated files are imported as published, nothing is regenerated locally |
| licence | CC0-1.0 — `LICENSE` is the repository file at the VCS commit (the crate tarball's `include` list omits it) |

| imported path | bytes | sha256 |
| --- | ---: | --- |
| `src/parser.c` | 830,121 | `84a7d48f6344d81728a37538a2a6bfc127746e2e6943d08571129b372291ef4d` |
| `src/scanner.c` | 1,904 | `d1a087b7f333efc50275c6a3e0c83c86414d4213503ec5e8c27bafa2b0189762` |
| `src/tree_sitter/parser.h` | 5,378 | `ab104936984904469572a4e868149f7a22fb2929347f837ae6a1f9b790f1b173` |

All three files are byte-for-byte identical to the crate tarball and to the
repository at the VCS commit, at the same relative paths. Nothing under `src/`
is edited; TraceDecay's build glue lives in `build.rs` and `ts_provider.rs`.
`src/tree_sitter/parser.h` is the tree-sitter runtime header the tree-sitter
CLI writes into every generated grammar (tree-sitter/tree-sitter, MIT); it
ships as part of the upstream package and is imported unchanged.

## Why a copied grammar rather than the crate

The crate's Rust binding depends on `tree-sitter ~0.20.6`, whose `Language`
type is not compatible with the `tree-sitter` 0.26 runtime used here, and no
newer release exists. The generated C sources themselves are still accepted by
the 0.26 runtime, so `build.rs` compiles `src/parser.c` and `src/scanner.c`
directly and `ts_provider.rs` declares the `tree_sitter_wgsl` entry point.
Only `src/` is imported; `grammar.js`, `queries/`, and the Rust binding are not
needed to build.

## Refresh procedure

The published package is the import authority. To re-import or verify the
snapshot:

```sh
cd crates/tracedecay-code-extraction/vendor/tree-sitter-wgsl
curl -fsSL -o /tmp/tree-sitter-wgsl-0.0.6.crate \
  https://static.crates.io/crates/tree-sitter-wgsl/tree-sitter-wgsl-0.0.6.crate
echo '3e6ab6ce5c5817f198d0e2b36ad501daa48798db5b1602187ac3fd6aaac938e3  /tmp/tree-sitter-wgsl-0.0.6.crate' \
  | sha256sum -c
tar -xzf /tmp/tree-sitter-wgsl-0.0.6.crate --strip-components=1 \
  tree-sitter-wgsl-0.0.6/src/parser.c \
  tree-sitter-wgsl-0.0.6/src/scanner.c \
  tree-sitter-wgsl-0.0.6/src/tree_sitter/parser.h
curl -fsSL -o LICENSE \
  https://raw.githubusercontent.com/mehmetoguzderin/tree-sitter-wgsl/27ae4db1458d5ea7555300c9e93c6273723d420c/LICENSE
git status --short .   # expected: no changes
```

Moving to a different grammar revision is a behaviour change, not a refresh:
update every field above (package version, checksum, VCS commit, per-file
hashes), re-run the WGSL extraction fixtures, and commit the import on its own.

Offline workspace for the cargo rerun tests in
`tests/core_cli_suite/source_provenance_test.rs`.

The nested build mounts `build-support/source_provenance.rs`, which names
the `serde_json` crate. `provenance-json` is that crate: a path member with
no registry dependency. The `[workspace]` table keeps this package out of
the TraceDecay workspace. `Cargo.lock` is the authority for `--locked`
builds.

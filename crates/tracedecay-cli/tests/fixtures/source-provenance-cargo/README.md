clean

Regenerate the vendor snapshot from the checked-in lockfile:

```sh
cd crates/tracedecay-cli/tests/fixtures/source-provenance-cargo
cargo vendor --locked vendor
```

`Cargo.lock` is the exact dependency authority; update it deliberately before
running the command when bumping the fixture. Keep `Cargo.toml` pinned to
`serde_json = "=1.0.151"` with the standalone `[workspace]` table.
Restore the first line of this file to `clean` after regeneration.

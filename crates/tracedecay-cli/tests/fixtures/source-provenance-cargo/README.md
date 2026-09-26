clean

Regenerate the vendor snapshot from the checked-in lockfile. Run Cargo from
outside the checkout, because the repository `.cargo/config.toml` replaces
crates.io with the crates pnpm vendored for the root workspace, and those do
not cover this fixture's lock.

```sh
fixture="$PWD/crates/tracedecay-cli/tests/fixtures/source-provenance-cargo"
(cd / && cargo vendor --locked --manifest-path "$fixture/Cargo.toml" "$fixture/vendor")
```

`Cargo.lock` is the exact dependency authority; update it deliberately before
running the command when bumping the fixture. Keep `Cargo.toml` pinned to
`serde_json = "=1.0.151"` with the standalone `[workspace]` table.
Restore the first line of this file to `clean` after regeneration.

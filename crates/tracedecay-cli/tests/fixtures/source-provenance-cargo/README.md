clean

Regenerate the pinned `serde_json` vendor snapshot from this directory:

```sh
cd crates/tracedecay-cli/tests/fixtures/source-provenance-cargo
cargo generate-lockfile
cargo vendor vendor
```

Keep `Cargo.toml` pinned to `serde_json = "=1.0.151"` with the standalone `[workspace]` table.
Restore the first line of this file to `clean` after regeneration.

# Temporary files always go to the system temporary directory

Turso rev `2829ee1662bd01d79f60c2c170af684a2b50af86`.

`TempFile::new` (`core/io/mod.rs:302`) creates its directory with
`tempfile::tempdir()`, the process-wide system temporary directory, and only
then asks the `IO` backend to open a file inside it. A backend that confines
the engine to known paths, as TraceDecay's `ConfinedIo` does, has to refuse
that open, so every sorter spill (`core/vdbe/sorter.rs:536`) and ephemeral
table fails unless the connection sets `TempStore::Memory`. With `Memory` the
sorter's spill chunks live in a `MemoryIO` file, so a finalization sort holds
its whole input in memory and the build memory ledger cannot bound it
(#3491).

Repro: open a database through an `IO` whose `open_file` admits only the
database and WAL paths, set `PRAGMA temp_store = FILE`, and run an `ORDER BY`
over more than the sorter's 2 MB buffer: the statement fails when the sorter
opens `/tmp/.tmpXXXXXX/tursodb_temp_file`.

Fix carried in `scripts/bazel/patches/turso_core-io-temp-dir.patch`: an
`IO::temp_dir()` hook, `None` by default, that `TempFile::new` creates its
directory under. `ConfinedIo` answers with the store's private `<store>-tmp`
directory, which it clears once the engine holds the store lock and removes on
close. The native test
`file_sorts_spill_inside_the_store_and_leave_nothing_behind` holds this.

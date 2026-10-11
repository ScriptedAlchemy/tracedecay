# The WAL restart cannot be kept without the auto-checkpoint

Turso rev `2829ee1662bd01d79f60c2c170af684a2b50af86`.

`turso_core::Connection` stores a `WalAutoActions` set with two flags:
`Checkpoint` (checkpoint after commit and on close) and `Restart` (start the WAL
over before a write once every frame is backfilled). The only public setter is
`wal_auto_actions_disable()`, which clears both. The field is `pub(super)`.

TraceDecay must disable the automatic checkpoint, because the writer actor's
checkpoint lane is the only scheduler. Clearing `Restart` with it means a
fully backfilled WAL is never reused: every write appends, and the file only
shrinks at a maintenance or shutdown TRUNCATE.

Repro:

```rust
let conn = db.connect()?;
conn.wal_auto_actions_disable();
// write N pages, then PRAGMA wal_checkpoint(PASSIVE) copies all of them back
// write 1 page: the WAL now holds N + 1 frames; SQLite would hold 1
```

Workaround in `crates/tracedecay-sql/src/native/driver.rs`: after a PASSIVE or
FULL checkpoint that copied back every frame, the driver runs a RESTART
checkpoint, which is what SQLite's next writer does. A reader still using the
WAL makes the restart report busy; the next checkpoint tries again. The
conformance case `a_copied_back_wal_starts_over_on_the_next_write` holds both
engines to this.

Engine fix to upstream: a `set_wal_auto_actions(WalAutoActions)` setter.

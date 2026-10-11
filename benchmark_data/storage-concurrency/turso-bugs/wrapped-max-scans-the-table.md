# A wrapped `MAX(col)` scans the whole table (Turso 0.8.0)

Turso answers a bare `SELECT MAX(rowid) FROM t` from the end of the B-tree.
Once the aggregate sits inside another expression (`COALESCE(MAX(x), 0)`,
`IFNULL(MAX(x), 0)`, `MAX(x) + 0`), it loses that shortcut and reads every
row. SQLite keeps the shortcut for these forms.

## Repro (4000 rows)

| statement | VM steps |
|---|---|
| `SELECT MAX(rowid) FROM t` | 13 |
| `SELECT COALESCE(MAX(rowid), 0) FROM t` | 12011 |
| `SELECT IFNULL(MAX(rowid), 0) FROM t` | 12012 |
| `SELECT MAX(rowid) + 0 FROM t` | 12012 |
| `SELECT COALESCE((SELECT MAX(rowid) FROM t), 0)` | 22 |

## Workaround

The aggregate moves into a scalar subquery and the wrapper stays outside:
`SELECT COALESCE((SELECT MAX(x) FROM t ...), 0)`. Both engines return the
same value. TraceDecay's projection cache reads the provenance high-water mark
this way on every writer operation
(`tracedecay-global-db` `observation_projection/state.rs`
`provenance_high_water`), as do the observation frontier, payload GC and
summary-retention watermarks.

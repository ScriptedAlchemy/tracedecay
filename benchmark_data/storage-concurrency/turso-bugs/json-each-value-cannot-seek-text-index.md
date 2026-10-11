# A `json_each` value cannot drive a seek on a TEXT index (Turso 0.8.0)

Turso declares the `json_each` columns `value ANY` and `atom ANY`
(`core/json/vtab.rs`). `ANY` has NUMERIC affinity, so an equality between a
TEXT index column and `requested.value` has NUMERIC comparison affinity, and
`Constraint::can_drive_index_seek` (`core/translate/optimizer/constraints.rs`)
refuses the seek. SQLite declares the `json_each` columns without a type, so
the comparison takes the column's TEXT affinity and seeks. The rows returned
are the same; Turso scans the whole index range for every requested value.

`json_extract(value, '$.x')` carries no affinity and seeks normally. A rowid
or INTEGER column also seeks, because NUMERIC affinity suits it.

## Repro

```sql
CREATE TABLE prov (v TEXT NOT NULL, observation_id TEXT NOT NULL,
                   PRIMARY KEY (v, observation_id));
-- 4000 rows for v = 'v1'
SELECT p.observation_id
FROM json_each('["obs-1","obs-2"]') AS requested
CROSS JOIN prov AS p
WHERE p.v = 'v1' AND p.observation_id = requested.value;
```

| form | VM steps over 4000 rows |
|---|---|
| `= requested.value` | 40018 (plan: `SEARCH p USING ... (v=?)`) |
| `= CAST(requested.value AS TEXT)` | 30 (plan: `(v=? AND observation_id=?)`) |
| `anchor_id IN (SELECT value FROM json_each(..))` | 68014 |
| `anchor_id IN (SELECT CAST(value AS TEXT) FROM json_each(..))` | 48 |

## Workaround

Every TEXT join or `IN` key read from `json_each` is spelled
`CAST(<alias>.value AS TEXT)`. The cast is a no-op on SQLite, where the
requested ids are already text.

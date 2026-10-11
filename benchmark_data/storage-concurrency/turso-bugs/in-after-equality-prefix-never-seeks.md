# `IN` on a later index column never seeks (Turso 0.8.0)

Turso seeks an `IN` list or `IN (subquery)` only when the `IN` column leads
the index. When the index starts with equality columns and the `IN` column
follows them, Turso seeks the equality prefix and filters every row under it.
SQLite seeks each `IN` value under the prefix.

## Repro

```sql
CREATE TABLE r (store_id INTEGER PRIMARY KEY, session_id TEXT NOT NULL,
                summary_id TEXT NOT NULL);
CREATE INDEX r_sess ON r (session_id, summary_id);
-- 4000 rows, all session_id = 's'
SELECT store_id FROM r WHERE session_id = 's' AND summary_id IN ('sum-1', 'sum-2');
```

| form | VM steps |
|---|---|
| `summary_id IN ('sum-1', 'sum-2')` | 20012 |
| `summary_id IN (SELECT CAST(value AS TEXT) FROM json_each(..))` | 68017 |
| `FROM (SELECT DISTINCT CAST(value AS TEXT) AS v FROM json_each(..)) AS w CROSS JOIN r WHERE r.session_id = 's' AND r.summary_id = w.v` | 50 |

## Workaround

A prefixed `IN` over requested keys is written as a join against the distinct
requested keys, as in the last row. `DISTINCT` keeps the `IN` semantics when a
caller repeats a key.

# A wider secondary index loses to a one-column primary-key prefix (Turso 0.8.0)

When a table's primary-key index and a secondary index both match a
statement's leading equality, Turso picks the primary-key index even if the
secondary index matches more equality columns. The statement then filters
every row under the shared one-column prefix. `ANALYZE` does not change the
choice. SQLite picks the index that matches the most equality columns.

## Repro

```sql
CREATE TABLE prov (
  projector_version TEXT NOT NULL, observation_id TEXT NOT NULL,
  output_ordinal INTEGER NOT NULL, output_provider TEXT NOT NULL,
  output_message_id TEXT NOT NULL,
  PRIMARY KEY (projector_version, observation_id, output_ordinal));
CREATE INDEX prov_output ON prov (projector_version, output_provider, output_message_id);
-- 4000 rows for projector_version = 'v1'
SELECT observation_id FROM prov
WHERE projector_version = 'v1' AND output_provider = 'codex' AND output_message_id = 'msg-7';
```

- Turso: `SEARCH prov USING INDEX sqlite_autoindex_prov_1 (projector_version=?)`,
  28012 VM steps.
- With `INDEXED BY prov_output`: 18 VM steps.

The same choice is made for a correlated subquery whose keys come from the
outer row: 72,078,006 VM steps for 4000 outer rows, against 72,006 with
`INDEXED BY`.

Two secondary indexes that share an equality prefix behave the same way. For
`tracedecay-sessions` `SESSION_MESSAGES_AFTER_SQL` (`provider = ? AND
session_id = ? AND timestamp >= ? ORDER BY timestamp, ordinal, message_id
LIMIT ?`), Turso chose `idx_lcm_raw_session_ordinal (provider=? AND
session_id=?)` plus a sorter over the whole session instead of
`idx_lcm_raw_session_activity`, which serves the range and the order.

## Workaround

Statements whose bound depends on a secondary index name it with
`INDEXED BY`. SQLite already chose that index, so the hint only pins the plan
both engines should run.

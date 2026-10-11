# A NOT NULL virtual column declared before its source fails self-referencing UPDATEs (Turso 0.8.0, 2829ee1662)

When a `NOT NULL` virtual generated column is declared before the column its
expression reads, an `UPDATE` whose `SET` expression reads that source column
fails the virtual column's `NOT NULL` check. The same `UPDATE` with a literal
or bound value succeeds, and a nullable virtual column ends up with the right
value, so only the constraint check sees the column before it is computed.
SQLite accepts every form.

## Repro

```sql
CREATE TABLE t (
  id TEXT PRIMARY KEY,
  snippet_text TEXT NOT NULL GENERATED ALWAYS AS (index_text) VIRTUAL,
  index_text TEXT NOT NULL
);
INSERT INTO t (id, index_text) VALUES ('a', 'hello');
UPDATE t SET index_text = 'lit' WHERE id = 'a';           -- ok on both engines
UPDATE t SET index_text = upper(index_text) WHERE id = 'a';
```

- SQLite: the second `UPDATE` succeeds.
- Turso: `NOT NULL constraint failed: t.snippet_text`.

Declaring `snippet_text` after `index_text`, or dropping its `NOT NULL`, makes
the second `UPDATE` succeed on Turso.

## Where it shows

`session_occurrences` (`tracedecay-global-db` `session_temporal_schema.rs`)
declares `snippet_text ... GENERATED ALWAYS AS (index_text) VIRTUAL` before
`index_text`. No production statement updates `index_text`, so only the
`tracedecay-session-temporal-store` test
`projection::tests::occurrence_replay_accepts_existing_full_text_and_rejects_changed_content`
hit it; it now binds the changed text instead of computing it in SQL.

## Workaround

Bind the new value. The schema fix is to declare `snippet_text` after
`index_text`.

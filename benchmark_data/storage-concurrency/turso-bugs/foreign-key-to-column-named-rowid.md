# Foreign key to a declared column named `rowid` is a mismatch (Turso 0.8.0, 2829ee1662)

`ForeignKey::validate` in `core/schema.rs` refuses any foreign key whose parent
column is spelled `rowid`, `_rowid_` or `oid`, even when the parent table
declares a real column with that name. SQLite resolves the declared column.
Turso enables foreign keys on every connection, so the refusal fires on every
DML statement that touches either side.

## Repro

```sql
PRAGMA foreign_keys = ON;
CREATE TABLE parent (rowid INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT);
CREATE TABLE child (
  parent_rowid INTEGER PRIMARY KEY,
  FOREIGN KEY(parent_rowid) REFERENCES parent(rowid)
);
INSERT INTO parent(body) VALUES ('x');
DELETE FROM parent;
```

- SQLite: the DELETE succeeds.
- Turso: `foreign key mismatch referencing "parent"`.

Seen through the census rows for `memory_v2_assertion_payloads` (every purge
`DELETE FROM memory_v2_assertion_payloads ...` and the digest insert).

## Workaround

`memory_v2_assertion_payload_digests` references its payload through the
payload's `UNIQUE(assertion_id, fact_id, owner_kind, project_id)` key, which the
digest row carries too. The `AFTER DELETE` trigger still drops the digest by
`payload_rowid`; a digest whose rowid disagrees with its key would block the
payload delete through the key, so a purge fails closed instead of leaving a
fingerprint.

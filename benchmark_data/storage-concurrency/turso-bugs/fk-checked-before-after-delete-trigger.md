# A parent delete fails its foreign key before an AFTER DELETE trigger removes the child (Turso 0.8.0, 2829ee1662)

SQLite counts immediate foreign-key violations across the whole statement,
including the triggers it fires, and checks the count when the statement
ends. A parent delete whose `AFTER DELETE` trigger deletes the referencing
child therefore succeeds. Turso checks the child reference when the parent
row is deleted, before the `AFTER DELETE` trigger runs, and fails the
statement.

## Repro

```sql
PRAGMA foreign_keys = ON;
CREATE TABLE p (rowid INTEGER PRIMARY KEY AUTOINCREMENT, a TEXT NOT NULL, UNIQUE(a));
CREATE TABLE c (prow INTEGER PRIMARY KEY, a TEXT NOT NULL, UNIQUE(a),
                FOREIGN KEY(a) REFERENCES p(a));
CREATE TRIGGER p_del AFTER DELETE ON p BEGIN
  DELETE FROM c WHERE prow = OLD.rowid;
END;
INSERT INTO p (a) VALUES ('x');
INSERT INTO c VALUES (1, 'x');
DELETE FROM p WHERE a = 'x';
```

- SQLite: the DELETE succeeds and removes both rows.
- Turso: `FOREIGN KEY constraint failed`.

The same cascade as a `BEFORE DELETE` trigger succeeds on both engines.

## Where it showed

`tracedecay-runtime-core` `memory_v2_payloads_digest_delete` removes the
`memory_v2_assertion_payload_digests` row of a deleted
`memory_v2_assertion_payloads` row. Every memory-fact removal, merge and
payload purge failed on native with `FOREIGN KEY constraint failed`
(`tracedecay-session-memory` crud, curation, merge, search and graph
reconciliation tests).

## Workaround

The trigger is `BEFORE DELETE`. The native final-shape golden was regenerated.

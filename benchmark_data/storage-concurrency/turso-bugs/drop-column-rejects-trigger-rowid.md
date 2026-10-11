# DROP COLUMN rejects any trigger that reads `NEW.rowid` (Turso 0.8.0)

`ALTER TABLE ... DROP COLUMN` re-validates every trigger in the schema
(`core/translate/alter.rs`, `validate_trigger_columns_after_drop`). That check
resolves `NEW.<column>` against the declared columns only, so the implicit
`rowid` reads as a missing column. The DROP fails even when the trigger sits on
an unrelated table. SQLite accepts the same statement.

## Repro

```sql
CREATE TABLE receipts (receipt_id TEXT NOT NULL PRIMARY KEY);
CREATE TABLE marks (high INTEGER NOT NULL);
CREATE TRIGGER receipts_mark AFTER INSERT ON receipts
  WHEN NEW.rowid <= (SELECT high FROM marks)
  BEGIN DELETE FROM marks; END;
CREATE TABLE other (a TEXT, b TEXT);
ALTER TABLE other DROP COLUMN b;
```

- SQLite: the DROP succeeds.
- Turso: `Parse error: error in trigger receipts_mark: no such column: NEW.rowid`.

The trigger itself installs and fires correctly on Turso; only the DROP COLUMN
re-validation is wrong.

## Workaround

TraceDecay never drops a column in production: native stores start fresh and
install their final shape. The one caller is the drift fixture in
`tracedecay-global-db` `schema_stages::tests::registry_with_non_final_code_project_shape_requires_typed_reset`.
It lifts `receipt_audit_invalidate_nonappend_insert_v1`, applies the drift, and
restores the trigger from its stored `sqlite_schema` text, so admission still
sees the exact registered trigger.

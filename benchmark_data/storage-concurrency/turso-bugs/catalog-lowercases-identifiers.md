# The schema catalog stores table names lowercased (Turso 0.8.0)

SQLite keeps a table's name in `sqlite_schema.name` exactly as the CREATE
statement spelled it. Turso stores the normalized, lowercased identifier.
Identifier lookup is case-insensitive on both engines, so queries behave the
same; only text that reports the catalog name differs.

## Repro

```sql
CREATE TABLE MiXeD_Case (a INTEGER NOT NULL);
SELECT name FROM sqlite_schema WHERE type = 'table';
```

- SQLite: `MiXeD_Case`.
- Turso: `mixed_case`.

## Workaround

None needed in product code: admission refuses the unknown object on both
engines and only its message carries the catalog spelling. The admission
tests in `tracedecay-global-db`
`tests::lcm_schema::temporal_catalog::admission::final_schema_admission_rejects_mixed_case_extra_*`
compare the refusal's table name case-insensitively.

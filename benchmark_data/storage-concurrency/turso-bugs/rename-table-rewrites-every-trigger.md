# RENAME TO rewrites the stored text of every trigger (Turso 0.8.0)

`ALTER TABLE ... RENAME TO` runs `AlterTableFunc::RenameTable` over every
`sqlite_schema` row (`core/vdbe/execute.rs`). Its `CreateTrigger` arm always
returns `Some(stmt.to_string())`, so every trigger in the schema is stored
again as Turso's re-rendered SQL, including triggers on unrelated tables that
never mention the renamed table. SQLite rewrites only the references to the
renamed table and leaves every other trigger's text byte-identical. The
`CreateTable` and `CreateIndex` arms already return `None` for unrelated rows;
only triggers are affected.

## Repro

```sql
CREATE TABLE audit (id INTEGER PRIMARY KEY);
CREATE TRIGGER audit_immutable
BEFORE UPDATE ON audit
BEGIN SELECT RAISE(ABORT, 'immutable'); END;
CREATE TABLE scratch (id INTEGER PRIMARY KEY);
SELECT sql FROM sqlite_schema WHERE name = 'audit_immutable';
ALTER TABLE scratch RENAME TO renamed;
SELECT sql FROM sqlite_schema WHERE name = 'audit_immutable';
```

- SQLite: both reads return the trigger text exactly as written.
- Turso: the second read returns the re-rendered statement, so the text
  differs from the first read although `audit` was not touched.

## Effect

Any check that pins stored DDL text sees drift after an unrelated rename. The
configuration authority digests the stored `sql` of every `configuration_*`
object, so a rename anywhere in the profile store makes it report
`persisted schema is not the exact final configuration shape`.

Found by `//crates/tracedecay:transport_acceptance_suite`
`typed_terminal_restart_acceptance::stale_profile_authority_reset` and the
two `stale_project_sessions_census` tests that share its fixture: they expected
the project registry refusal and got the configuration refusal instead.

## Workaround

Production stores never rename a table: native stores start fresh and install
their final shape. The fixture
`give_registry_the_released_graph_scope_shape` now copies the rows to a
scratch table, drops `graph_scopes`, creates it in the released shape and
copies the rows back, without a rename.

The lexical projection artifact builder in `tracedecay-query` still renames
`ngram_postings` (`artifact/builder.rs`). An artifact whose verification pins
trigger text would see the same drift there.

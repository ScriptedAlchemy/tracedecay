# CAST of invalid UTF-8 bytes to TEXT is lossy

Turso rev `2829ee1662bd01d79f60c2c170af684a2b50af86`.

```sql
CREATE TABLE t (value TEXT NOT NULL);
INSERT INTO t(value) VALUES (CAST(x'80' AS TEXT));
SELECT hex(value) FROM t;
```

SQLite stores the byte as written and returns `80`. Turso converts the blob with
`String::from_utf8_lossy` (`core/vdbe/value.rs`, `core/numeric/mod.rs`) and
returns `EFBFBD`, the replacement character.

Effect on TraceDecay: none for owned data. Every text value the runtime binds is
a Rust `String`, so only a SQL-side `CAST` can produce invalid text, and on
native the engine has already replaced it. The exact SQL reader's refusal of
invalid UTF-8 (`invalid_sqlite_text_is_rejected_without_lossy_conversion`)
applies to SQLite only; under `--config=native` the test asserts the stored
replacement character instead.

Workaround: none needed.

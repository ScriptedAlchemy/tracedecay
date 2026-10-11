-- Turso rejects a foreign key whose parent column is a declared column named
-- rowid, oid or _rowid_ (turso_core 0.8.0 at 2829ee1662, core/schema.rs
-- ForeignKey::validate). SQLite resolves the name to the declared column, so
-- the statements below run there.
--
-- Native Turso fails the INSERT with:
--   foreign key mismatch referencing "parent"
--
-- Workaround (#3402 U12c, memory_v2/schema/payload_digests.rs): reference the
-- parent's composite UNIQUE key instead of its rowid-named column.
PRAGMA foreign_keys = ON;
CREATE TABLE parent (rowid INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT NOT NULL);
CREATE TABLE child (
    parent_rowid INTEGER PRIMARY KEY,
    FOREIGN KEY(parent_rowid) REFERENCES parent(rowid)
);
INSERT INTO parent(body) VALUES ('a');

//! Deterministic parity and rollback evidence for the pre-cutover graph adapter.
//!
//! These APIs are explicit fixtures. Production attachment does not call them,
//! and the schema helper must never be used as a exact SQL authority.

use std::{collections::BTreeMap, path::Path};

use rusqlite::{Connection, params};
use tracedecay_store::GraphNodeV1;

use super::{GraphMutationExecutor, GraphMutationPayloadV1, read::row_to_graph_node};

const GRAPH_FIXTURE_SCHEMA_V1: &str = r#"
CREATE TABLE nodes (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    qualified_name TEXT NOT NULL,
    file_path TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    start_column INTEGER NOT NULL,
    end_column INTEGER NOT NULL,
    docstring TEXT,
    signature TEXT,
    visibility TEXT NOT NULL DEFAULT 'private',
    is_async INTEGER NOT NULL DEFAULT 0,
    branches INTEGER NOT NULL DEFAULT 0,
    loops INTEGER NOT NULL DEFAULT 0,
    returns INTEGER NOT NULL DEFAULT 0,
    max_nesting INTEGER NOT NULL DEFAULT 0,
    unsafe_blocks INTEGER NOT NULL DEFAULT 0,
    unchecked_calls INTEGER NOT NULL DEFAULT 0,
    assertions INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL,
    attrs_start_line INTEGER,
    parent_id TEXT
);
CREATE TABLE edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source TEXT NOT NULL,
    target TEXT NOT NULL,
    kind TEXT NOT NULL,
    line INTEGER,
    FOREIGN KEY(source) REFERENCES nodes(id) ON DELETE CASCADE,
    FOREIGN KEY(target) REFERENCES nodes(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX idx_edges_unique
    ON edges(source, target, kind, COALESCE(line, -1));
CREATE TABLE files (
    path TEXT PRIMARY KEY,
    content_hash TEXT NOT NULL,
    size INTEGER NOT NULL,
    modified_at INTEGER NOT NULL,
    indexed_at INTEGER NOT NULL,
    node_count INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE unresolved_refs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    from_node_id TEXT NOT NULL,
    reference_name TEXT NOT NULL,
    reference_kind TEXT NOT NULL,
    line INTEGER NOT NULL,
    col INTEGER NOT NULL,
    file_path TEXT NOT NULL
);
CREATE TABLE vectors (
    node_id TEXT PRIMARY KEY,
    embedding BLOB NOT NULL,
    model TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE VIRTUAL TABLE nodes_fts USING fts5(
    name, qualified_name, docstring, signature,
    content='nodes', content_rowid='rowid'
);
CREATE TRIGGER nodes_fts_insert AFTER INSERT ON nodes BEGIN
    INSERT INTO nodes_fts(rowid, name, qualified_name, docstring, signature)
    VALUES(NEW.rowid, NEW.name, NEW.qualified_name, NEW.docstring, NEW.signature);
END;
CREATE TRIGGER nodes_fts_delete AFTER DELETE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, docstring, signature)
    VALUES('delete', OLD.rowid, OLD.name, OLD.qualified_name, OLD.docstring, OLD.signature);
END;
CREATE TRIGGER nodes_fts_update AFTER UPDATE ON nodes BEGIN
    INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, docstring, signature)
    VALUES('delete', OLD.rowid, OLD.name, OLD.qualified_name, OLD.docstring, OLD.signature);
    INSERT INTO nodes_fts(rowid, name, qualified_name, docstring, signature)
    VALUES(NEW.rowid, NEW.name, NEW.qualified_name, NEW.docstring, NEW.signature);
END;
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphParityEdgeV1 {
    pub source: String,
    pub target: String,
    pub kind: String,
    pub line: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphParityFileV1 {
    pub path: String,
    pub content_hash: String,
    pub size: u64,
    pub modified_at: i64,
    pub indexed_at: i64,
    pub node_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphParityDerivedCountsV1 {
    pub unresolved_refs: u64,
    pub vectors: u64,
    pub nodes_fts: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphParitySnapshotV1 {
    pub nodes: Vec<GraphNodeV1>,
    pub edges: Vec<GraphParityEdgeV1>,
    pub files: Vec<GraphParityFileV1>,
    pub metadata: BTreeMap<String, String>,
    pub derived_counts: GraphParityDerivedCountsV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphRollbackEvidenceV1 {
    pub before: GraphParitySnapshotV1,
    pub applied: GraphParitySnapshotV1,
    pub after_rollback: GraphParitySnapshotV1,
}

impl GraphRollbackEvidenceV1 {
    pub fn restored_exactly(&self) -> bool {
        self.before == self.after_rollback
    }
}

pub fn install_graph_fixture_schema_v1(connection: &Connection) -> rusqlite::Result<()> {
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.execute_batch(GRAPH_FIXTURE_SCHEMA_V1)
}

pub fn create_graph_fixture_database_v1(path: &Path) -> rusqlite::Result<()> {
    let connection = Connection::open(path)?;
    install_graph_fixture_schema_v1(&connection)
}

pub fn capture_graph_parity_fixture_v1(
    connection: &Connection,
) -> rusqlite::Result<GraphParitySnapshotV1> {
    Ok(GraphParitySnapshotV1 {
        nodes: capture_nodes(connection)?,
        edges: capture_edges(connection)?,
        files: capture_files(connection)?,
        metadata: capture_metadata(connection)?,
        derived_counts: GraphParityDerivedCountsV1 {
            unresolved_refs: table_count(connection, "unresolved_refs")?,
            vectors: table_count(connection, "vectors")?,
            nodes_fts: table_count(connection, "nodes_fts")?,
        },
    })
}

pub fn exercise_graph_rollback_fixture_v1(
    connection: &mut Connection,
    executor: &mut GraphMutationExecutor,
    payload: &GraphMutationPayloadV1,
) -> rusqlite::Result<GraphRollbackEvidenceV1> {
    let before = capture_graph_parity_fixture_v1(connection)?;
    let applied = {
        let mut transaction = connection.transaction()?;
        let mut savepoint = transaction.savepoint()?;
        executor.execute(&savepoint, payload)?;
        let applied = capture_graph_parity_fixture_v1(&savepoint)?;
        savepoint.rollback()?;
        drop(savepoint);
        transaction.rollback()?;
        applied
    };
    let after_rollback = capture_graph_parity_fixture_v1(connection)?;
    Ok(GraphRollbackEvidenceV1 {
        before,
        applied,
        after_rollback,
    })
}

fn capture_nodes(connection: &Connection) -> rusqlite::Result<Vec<GraphNodeV1>> {
    let mut statement = connection.prepare(
        "SELECT id, kind, name, qualified_name, file_path,
                start_line, end_line, start_column, end_column,
                docstring, signature, visibility, is_async, branches, loops, returns,
                max_nesting, unsafe_blocks, unchecked_calls, assertions, updated_at,
                attrs_start_line, parent_id
         FROM nodes ORDER BY id",
    )?;
    statement
        .query_map([], row_to_graph_node)?
        .collect::<rusqlite::Result<Vec<_>>>()
}

fn capture_edges(connection: &Connection) -> rusqlite::Result<Vec<GraphParityEdgeV1>> {
    let mut statement = connection.prepare(
        "SELECT source, target, kind, line
         FROM edges ORDER BY source, target, kind, COALESCE(line, -1)",
    )?;
    statement
        .query_map([], |row| {
            Ok(GraphParityEdgeV1 {
                source: row.get(0)?,
                target: row.get(1)?,
                kind: row.get(2)?,
                line: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
}

fn capture_files(connection: &Connection) -> rusqlite::Result<Vec<GraphParityFileV1>> {
    let mut statement = connection.prepare(
        "SELECT path, content_hash, size, modified_at, indexed_at, node_count
         FROM files ORDER BY path",
    )?;
    statement
        .query_map([], |row| {
            let size = row.get::<_, i64>(2)?;
            let node_count = row.get::<_, i64>(5)?;
            Ok(GraphParityFileV1 {
                path: row.get(0)?,
                content_hash: row.get(1)?,
                size: u64::try_from(size).unwrap_or(0),
                modified_at: row.get(3)?,
                indexed_at: row.get(4)?,
                node_count: u32::try_from(node_count).unwrap_or(0),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
}

fn capture_metadata(connection: &Connection) -> rusqlite::Result<BTreeMap<String, String>> {
    let mut statement = connection.prepare("SELECT key, value FROM metadata ORDER BY key")?;
    statement
        .query_map(params![], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()
}

fn table_count(connection: &Connection, table: &'static str) -> rusqlite::Result<u64> {
    let sql = match table {
        "unresolved_refs" => "SELECT COUNT(*) FROM unresolved_refs",
        "vectors" => "SELECT COUNT(*) FROM vectors",
        "nodes_fts" => "SELECT COUNT(*) FROM nodes_fts",
        _ => unreachable!("closed graph fixture table"),
    };
    connection
        .query_row(sql, [], |row| row.get::<_, i64>(0))
        .map(|count| u64::try_from(count).unwrap_or(0))
}

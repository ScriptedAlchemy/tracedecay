use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};

use rusqlite::{Savepoint, params};
use tracedecay_store::GraphNodeV1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphEdgeMutationV1 {
    pub source: String,
    pub target: String,
    pub kind: String,
    pub line: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphFileMutationV1 {
    pub path: String,
    pub content_hash: String,
    pub size: u64,
    pub modified_at: i64,
    pub indexed_at: i64,
    pub node_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphFileReplacementV1 {
    pub file: GraphFileMutationV1,
    pub nodes: Vec<GraphNodeV1>,
    pub edges: Vec<GraphEdgeMutationV1>,
}

/// Closed graph mutation vocabulary used by pre-cutover native fixtures.
///
/// There is intentionally no SQL, callback, JSON blob, path locator, schema
/// change, FTS rebuild, or maintenance variant. Publishing one of these
/// operations through the root writer still requires a later store-contract
/// cutover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphMutationPayloadV1 {
    ReplaceFile(GraphFileReplacementV1),
    DeleteFile { file_path: String },
    SetMetadata { entries: BTreeMap<String, String> },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GraphMutationExecutor;

impl GraphMutationExecutor {
    pub fn execute(
        &mut self,
        savepoint: &Savepoint<'_>,
        payload: &GraphMutationPayloadV1,
    ) -> rusqlite::Result<()> {
        validate_payload(payload)?;
        match payload {
            GraphMutationPayloadV1::ReplaceFile(replacement) => {
                delete_file_projection(savepoint, &replacement.file.path)?;
                upsert_nodes(savepoint, &replacement.nodes)?;
                upsert_edges(savepoint, &replacement.edges)?;
                upsert_file(savepoint, &replacement.file)
            }
            GraphMutationPayloadV1::DeleteFile { file_path } => {
                delete_file_projection(savepoint, file_path)?;
                savepoint
                    .execute("DELETE FROM files WHERE path = ?1", params![file_path])
                    .map(|_| ())
            }
            GraphMutationPayloadV1::SetMetadata { entries } => {
                let mut statement = savepoint.prepare(
                    "INSERT INTO metadata(key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                )?;
                for (key, value) in entries {
                    statement.execute(params![key, value])?;
                }
                Ok(())
            }
        }
    }
}

fn validate_payload(payload: &GraphMutationPayloadV1) -> rusqlite::Result<()> {
    match payload {
        GraphMutationPayloadV1::ReplaceFile(replacement) => {
            validate_file(&replacement.file)?;
            let mut node_ids = BTreeSet::new();
            for node in &replacement.nodes {
                node.validate().map_err(invalid_contract)?;
                if node.file_path != replacement.file.path {
                    return Err(invalid("replacement node belongs to a different file"));
                }
                if !node_ids.insert(node.id.as_str()) {
                    return Err(invalid("replacement contains duplicate node ids"));
                }
            }
            if replacement.file.node_count as usize != replacement.nodes.len() {
                return Err(invalid("file node count does not match replacement nodes"));
            }
            for edge in &replacement.edges {
                validate_text(&edge.source, "edge source", 4_096)?;
                validate_text(&edge.target, "edge target", 4_096)?;
                validate_text(&edge.kind, "edge kind", 128)?;
                if !node_ids.contains(edge.source.as_str()) {
                    return Err(invalid(
                        "replacement edge source is outside the replaced file",
                    ));
                }
            }
            Ok(())
        }
        GraphMutationPayloadV1::DeleteFile { file_path } => validate_relative_path(file_path),
        GraphMutationPayloadV1::SetMetadata { entries } => {
            if entries.is_empty() {
                return Err(invalid("metadata mutation is empty"));
            }
            for (key, value) in entries {
                validate_text(key, "metadata key", 4_096)?;
                if value.len() > 1024 * 1024 {
                    return Err(invalid("metadata value is too large"));
                }
            }
            Ok(())
        }
    }
}

fn validate_file(file: &GraphFileMutationV1) -> rusqlite::Result<()> {
    validate_relative_path(&file.path)?;
    validate_text(&file.content_hash, "file content hash", 4_096)?;
    sqlite_u64(file.size, "file size")?;
    Ok(())
}

fn validate_relative_path(value: &str) -> rusqlite::Result<()> {
    validate_text(value, "file path", 65_536)?;
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid("graph file path must be normalized and relative"));
    }
    Ok(())
}

fn validate_text(value: &str, field: &str, max: usize) -> rusqlite::Result<()> {
    if value.is_empty()
        || value.len() > max
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(invalid(format!("invalid {field}")));
    }
    Ok(())
}

fn invalid_contract(error: impl std::fmt::Display) -> rusqlite::Error {
    invalid(format!("invalid graph contract: {error}"))
}

fn invalid(message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName(message.into())
}

fn sqlite_u64(value: u64, field: &str) -> rusqlite::Result<i64> {
    i64::try_from(value).map_err(|_| invalid(format!("{field} exceeds SQLite INTEGER")))
}

fn delete_file_projection(savepoint: &Savepoint<'_>, file_path: &str) -> rusqlite::Result<()> {
    for table in ["edges", "unresolved_refs", "vectors"] {
        let sql = match table {
            "edges" => {
                "DELETE FROM edges
                 WHERE source IN (SELECT id FROM nodes WHERE file_path = ?1)
                    OR target IN (SELECT id FROM nodes WHERE file_path = ?1)"
            }
            "unresolved_refs" => {
                "DELETE FROM unresolved_refs
                 WHERE from_node_id IN (SELECT id FROM nodes WHERE file_path = ?1)"
            }
            "vectors" => {
                "DELETE FROM vectors
                 WHERE node_id IN (SELECT id FROM nodes WHERE file_path = ?1)"
            }
            _ => unreachable!("closed graph projection table"),
        };
        savepoint.execute(sql, params![file_path])?;
    }
    savepoint.execute("DELETE FROM nodes WHERE file_path = ?1", params![file_path])?;
    Ok(())
}

fn upsert_nodes(savepoint: &Savepoint<'_>, nodes: &[GraphNodeV1]) -> rusqlite::Result<()> {
    let mut statement = savepoint.prepare(
        "INSERT OR REPLACE INTO nodes
         (id, kind, name, qualified_name, file_path,
          start_line, end_line, start_column, end_column,
          docstring, signature, visibility, is_async,
          branches, loops, returns, max_nesting,
          unsafe_blocks, unchecked_calls, assertions, updated_at, attrs_start_line, parent_id)
         VALUES
         (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
          ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)",
    )?;
    for node in nodes {
        statement.execute(params![
            node.id,
            node.kind,
            node.name,
            node.qualified_name,
            node.file_path,
            i64::from(node.start_line),
            i64::from(node.end_line),
            i64::from(node.start_column),
            i64::from(node.end_column),
            node.docstring,
            node.signature,
            node.visibility,
            i64::from(node.is_async),
            i64::from(node.branches),
            i64::from(node.loops),
            i64::from(node.returns),
            i64::from(node.max_nesting),
            i64::from(node.unsafe_blocks),
            i64::from(node.unchecked_calls),
            i64::from(node.assertions),
            sqlite_u64(node.updated_at, "node updated_at")?,
            i64::from(node.attrs_start_line),
            node.parent_id,
        ])?;
    }
    Ok(())
}

fn upsert_edges(savepoint: &Savepoint<'_>, edges: &[GraphEdgeMutationV1]) -> rusqlite::Result<()> {
    let mut edge_statement = savepoint.prepare(
        "INSERT OR IGNORE INTO edges(source, target, kind, line)
         SELECT ?1, ?2, ?3, ?4
         WHERE EXISTS (SELECT 1 FROM nodes WHERE id = ?1)
           AND EXISTS (SELECT 1 FROM nodes WHERE id = ?2)",
    )?;
    let mut parent_statement =
        savepoint.prepare("UPDATE nodes SET parent_id = ?1 WHERE id = ?2")?;
    for edge in edges {
        if edge.kind == "contains" {
            parent_statement.execute(params![edge.source, edge.target])?;
        } else {
            edge_statement.execute(params![
                edge.source,
                edge.target,
                edge.kind,
                edge.line.map(i64::from),
            ])?;
        }
    }
    Ok(())
}

fn upsert_file(savepoint: &Savepoint<'_>, file: &GraphFileMutationV1) -> rusqlite::Result<()> {
    savepoint
        .execute(
            "INSERT OR REPLACE INTO files
             (path, content_hash, size, modified_at, indexed_at, node_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                file.path,
                file.content_hash,
                sqlite_u64(file.size, "file size")?,
                file.modified_at,
                file.indexed_at,
                i64::from(file.node_count),
            ],
        )
        .map(|_| ())
}

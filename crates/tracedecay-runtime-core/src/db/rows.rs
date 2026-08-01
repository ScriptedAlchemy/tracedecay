// Rust guideline compliant 2025-10-17
use crate::db::engine::{Error, Row, Value};
use crate::types::*;

// ---------------------------------------------------------------------------
// Helper: map an engine row to domain types (by column index).
// ---------------------------------------------------------------------------

/// The `nodes` projection [`row_to_node`] expects, in index order.
///
/// `row_to_node` maps **by column index**, so every `SELECT` feeding it must
/// request exactly these columns in exactly this order. Use this macro (via
/// `concat!` for static SQL, or the [`NODE_SELECT_COLUMNS`] const inside
/// `format!`) rather than re-spelling the list — a hand-written copy that
/// drifts by one column silently mis-maps every field after the drift.
///
/// Callers needing extra columns append them *after* this list, so their
/// indices start at 23 and the mapped prefix stays valid.
macro_rules! node_select_columns {
    () => {
        "id, kind, name, qualified_name, file_path, \
         start_line, end_line, start_column, end_column, \
         docstring, signature, visibility, is_async, branches, loops, returns, max_nesting, \
         unsafe_blocks, unchecked_calls, assertions, updated_at, attrs_start_line, parent_id"
    };
}

pub(super) use node_select_columns;

/// [`node_select_columns!`] as a value, for `format!`-built SQL.
pub(super) const NODE_SELECT_COLUMNS: &str = node_select_columns!();

/// Columns [`row_to_node`] reads, and therefore the index of the trailing
/// `rowid` cursor column in a paged node scan.
pub(super) const NODE_COLUMNS: i32 = 23;

/// Maps a row from the `nodes` table to a `Node`.
///
/// Expected column order: id(0), kind(1), name(2), `qualified_name(3)`,
/// `file_path(4)`, `start_line(5)`, `end_line(6)`, `start_column(7)`, `end_column(8)`,
/// docstring(9), signature(10), visibility(11), `is_async(12)`,
/// branches(13), loops(14), returns(15), `max_nesting(16)`,
/// `unsafe_blocks(17)`, `unchecked_calls(18)`, assertions(19), `updated_at(20)`,
/// `attrs_start_line(21)`.
pub(super) fn row_to_node(row: &Row) -> std::result::Result<Node, Error> {
    let kind_str = get_string_lossy(row, 1)?;
    let vis_str = get_string_lossy(row, 11)?;
    let is_async_int = row.get::<i64>(12)?;
    let start_line = row.get::<u32>(5)?;
    // `attrs_start_line` is the first line of an item's leading doc-comment /
    // attribute block. A stored `0` is a *legitimate* value — an item documented
    // or attributed at the very top of a file (e.g. `/// doc\nfn foo() {}` yields
    // attrs_start_line=0, start_line=1). We therefore trust the stored integer
    // verbatim, including 0, and never conflate it with "unset". We only fall
    // back to `start_line` when the column is genuinely absent: a SQL NULL (a
    // legacy row that predates the column) or an older SELECT list that does not
    // request column 21. `Option<u32>` distinguishes those cases (NULL / missing
    // column => `None`) from a real zero (`Some(0)`).
    let attrs_start_line = row
        .get::<Option<u32>>(21)
        .ok()
        .flatten()
        .unwrap_or(start_line);
    // `parent_id` is column 22 in v9+ SELECT lists. Older SELECTs in this
    // file don't request it; the .ok().flatten() chain swallows the missing-
    // column error and yields None.
    let parent_id = get_opt_string_lossy(row, 22).ok().flatten();

    Ok(Node {
        id: get_string_lossy(row, 0)?,
        kind: NodeKind::from_str(&kind_str).unwrap_or(NodeKind::Function),
        name: get_string_lossy(row, 2)?,
        qualified_name: get_string_lossy(row, 3)?,
        file_path: get_string_lossy(row, 4)?,
        start_line,
        attrs_start_line,
        end_line: row.get::<u32>(6)?,
        start_column: row.get::<u32>(7)?,
        end_column: row.get::<u32>(8)?,
        signature: get_opt_string_lossy(row, 10)?,
        docstring: get_opt_string_lossy(row, 9)?,
        visibility: Visibility::from_str(&vis_str).unwrap_or_default(),
        is_async: is_async_int != 0,
        branches: row.get::<u32>(13)?,
        loops: row.get::<u32>(14)?,
        returns: row.get::<u32>(15)?,
        max_nesting: row.get::<u32>(16)?,
        unsafe_blocks: row.get::<u32>(17)?,
        unchecked_calls: row.get::<u32>(18)?,
        assertions: row.get::<u32>(19)?,
        updated_at: row.get::<u64>(20)?,
        parent_id,
    })
}

/// Reads a text column as String, replacing invalid UTF-8 bytes with U+FFFD.
/// This prevents crashes when source files with non-UTF-8 encoding (e.g. Latin-1)
/// have their signatures or docstrings stored in the database.
///
/// The underlying `SQLite` text decoder rejects blob values, so we
/// must read as `Value` first and convert.
fn get_string_lossy(row: &Row, idx: i32) -> std::result::Result<String, Error> {
    let val = row.get::<Value>(idx)?;
    match val {
        Value::Text(s) => Ok(s),
        Value::Blob(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Value::Null => Ok(String::new()),
        Value::Integer(i) => Ok(i.to_string()),
        Value::Real(f) => Ok(f.to_string()),
    }
}

/// Like `get_string_lossy` but for nullable columns.
fn get_opt_string_lossy(row: &Row, idx: i32) -> std::result::Result<Option<String>, Error> {
    let val = row.get::<Value>(idx)?;
    match val {
        Value::Null => Ok(None),
        Value::Text(s) => Ok(Some(s)),
        Value::Blob(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
        Value::Integer(i) => Ok(Some(i.to_string())),
        Value::Real(f) => Ok(Some(f.to_string())),
    }
}

/// Maps a row from the `edges` table to an `Edge`.
///
/// Expected column order: source(0), target(1), kind(2), line(3).
pub(super) fn row_to_edge(row: &Row) -> std::result::Result<Edge, Error> {
    let kind_str = row.get::<String>(2)?;
    let line = row.get::<Option<u32>>(3)?;

    Ok(Edge {
        source: row.get::<String>(0)?,
        target: row.get::<String>(1)?,
        kind: EdgeKind::from_str(&kind_str).unwrap_or(EdgeKind::Uses),
        line,
    })
}

/// Maps a row from the `files` table to a `FileRecord`.
///
/// Expected column order: path(0), `content_hash(1)`, size(2), `modified_at(3)`,
/// `indexed_at(4)`, `node_count(5)`.
pub(super) fn row_to_file(row: &Row) -> std::result::Result<FileRecord, Error> {
    Ok(FileRecord {
        path: row.get::<String>(0)?,
        content_hash: row.get::<String>(1)?,
        size: row.get::<u64>(2)?,
        modified_at: row.get::<i64>(3)?,
        indexed_at: row.get::<i64>(4)?,
        node_count: row.get::<u32>(5)?,
    })
}

/// Maps a row from the `unresolved_refs` table to an `UnresolvedRef`.
///
/// Expected column order: `from_node_id(0)`, `reference_name(1)`,
/// `reference_kind(2)`, line(3), col(4), `file_path(5)`.
pub(super) fn row_to_unresolved_ref(row: &Row) -> std::result::Result<UnresolvedRef, Error> {
    let kind_str = row.get::<String>(2)?;

    Ok(UnresolvedRef {
        from_node_id: row.get::<String>(0)?,
        reference_name: row.get::<String>(1)?,
        reference_kind: EdgeKind::from_str(&kind_str).unwrap_or(EdgeKind::Uses),
        line: row.get::<u32>(3)?,
        column: row.get::<u32>(4)?,
        file_path: row.get::<String>(5)?,
    })
}

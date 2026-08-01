//! Every graph-scale read must page past the runtime's per-query
//! materialization limit.
//!
//! The `SQLite` runtime admits a bounded number of rows per query and refuses
//! anything larger outright rather than truncating it. `tracedecay_health`
//! failed on a real 296 K-line index with "failed to query test marker ids: …
//! migration SQL query materialization exceeded its limit", and the same class
//! of defect applied to every other unbounded whole-table or whole-partition
//! read.
//!
//! Each test asserts both directions against one oversized fixture:
//!
//! 1. the statement the read *used to* issue is still refused — otherwise the
//!    fixture no longer reproduces the failure and the next assertion proves
//!    nothing; and
//! 2. the `rowid` keyset statement the read issues *now* returns every row.
//!
//! The tests bind the production SQL constants and builders directly, so a
//! page statement cannot drift away from what the fixture exercises.

use tempfile::TempDir;

use super::analytics::{
    CALL_EDGE_LINE_PAGE_SQL, CALL_EDGE_LINE_PREFIXED_PAGE_SQL, CALL_EDGE_PAGE_SQL,
    CALL_EDGE_PREFIXED_PAGE_SQL, nodes_by_dir_page_sql,
};
use super::coverage::{
    SKIP_TEST_COVERAGE_PAGE_SQL, TEST_ANNOTATION_FILE_PAGE_SQL, TEST_MARKER_PAGE_SQL,
};
use super::edges::edges_by_endpoint_page_sql;
use super::engine::{TestConnection, Value};
use super::files::FILE_PATH_PAGE_SQL;
use super::nodes::NODES_BY_KIND_PAGE_SQL;
use super::sql::{collect_rowid_pages, collect_rowid_pages_with};

/// The `SQLite` runtime refuses a single query that materializes more than this
/// many rows.
const RUNTIME_QUERY_ROW_LIMIT: i64 = 10_000;

/// Rows seeded per scaled table. One past the limit is the smallest fixture
/// that separates a refused statement from a paged one.
const ROWS: i64 = RUNTIME_QUERY_ROW_LIMIT + 1;

/// The one hub symbol every seeded function calls, so a single edge endpoint
/// carries more rows than one query may materialize.
const HUB_ID: &str = "hub::sink";

/// Directory prefix holding every seeded function, one file each.
const FUNCTION_DIR: &str = "lib/";

/// Builds a graph whose `nodes`, `edges`, and `files` tables each hold more
/// rows than one query may materialize:
///
/// - `ROWS` `function` nodes under `lib/`, one file each, every docstring
///   carrying the `skip-test-coverage` marker;
/// - `ROWS` `annotation_usage` nodes named `test`, one per function;
/// - `ROWS` `annotates` edges (marker → function) and `ROWS` `calls` edges
///   (function → hub), so both the `calls` partition and the hub endpoint
///   exceed the limit;
/// - `ROWS` file records under `lib/`.
async fn seed_oversized_graph(directory: &TempDir) -> TestConnection {
    let conn = TestConnection::open(&directory.path().join("oversized.db"));
    conn.execute_batch(
        "CREATE TABLE nodes (
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
             visibility TEXT NOT NULL,
             is_async INTEGER NOT NULL,
             branches INTEGER NOT NULL,
             loops INTEGER NOT NULL,
             returns INTEGER NOT NULL,
             max_nesting INTEGER NOT NULL,
             unsafe_blocks INTEGER NOT NULL,
             unchecked_calls INTEGER NOT NULL,
             assertions INTEGER NOT NULL,
             updated_at INTEGER NOT NULL,
             attrs_start_line INTEGER NOT NULL,
             parent_id TEXT
         );
         CREATE TABLE edges (
             source TEXT NOT NULL,
             target TEXT NOT NULL,
             kind TEXT NOT NULL,
             line INTEGER
         );
         CREATE TABLE files (
             path TEXT PRIMARY KEY,
             content_hash TEXT NOT NULL,
             size INTEGER NOT NULL,
             modified_at INTEGER NOT NULL,
             indexed_at INTEGER NOT NULL,
             node_count INTEGER NOT NULL
         );
         -- The real schema indexes both edge endpoints and the node kind. The
         -- fixture carries them too, so a page's plan here matches production's
         -- rather than degenerating into a per-row table scan.
         CREATE INDEX idx_edges_source ON edges(source);
         CREATE INDEX idx_edges_target ON edges(target);
         CREATE INDEX idx_nodes_kind ON nodes(kind);
         CREATE INDEX idx_nodes_file_path ON nodes(file_path);",
    )
    .await
    .expect("create oversized graph schema");

    let fixture = format!(
        "WITH RECURSIVE fixture(value) AS (
             SELECT 0 UNION ALL SELECT value + 1 FROM fixture WHERE value < {}
         )",
        ROWS - 1
    );

    conn.execute(
        &format!(
            "{fixture}
             INSERT INTO nodes
             SELECT printf('fn::%05d', value), 'function', printf('f%05d', value),
                    printf('crate::f%05d', value), printf('{FUNCTION_DIR}m%05d.rs', value),
                    1, 3, 0, 1, 'skip-test-coverage', NULL, 'pub',
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 1, NULL
             FROM fixture"
        ),
        (),
    )
    .await
    .expect("seed function nodes");

    conn.execute(
        &format!(
            "{fixture}
             INSERT INTO nodes
             SELECT printf('marker::%05d', value), 'annotation_usage', 'test',
                    'test', 'src/hub.rs',
                    1, 3, 0, 1, NULL, NULL, 'pub',
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 1, NULL
             FROM fixture"
        ),
        (),
    )
    .await
    .expect("seed marker nodes");

    conn.execute(
        &format!(
            "INSERT INTO nodes VALUES
             ('{HUB_ID}', 'function', 'sink', 'crate::sink', 'src/hub.rs',
              1, 3, 0, 1, 'plain hub', NULL, 'pub', 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, NULL)"
        ),
        (),
    )
    .await
    .expect("seed hub node");

    conn.execute(
        &format!(
            "{fixture}
             INSERT INTO edges
             SELECT printf('marker::%05d', value), printf('fn::%05d', value), 'annotates', NULL
             FROM fixture"
        ),
        (),
    )
    .await
    .expect("seed annotates edges");

    conn.execute(
        &format!(
            "{fixture}
             INSERT INTO edges
             SELECT printf('fn::%05d', value), '{HUB_ID}', 'calls', value
             FROM fixture"
        ),
        (),
    )
    .await
    .expect("seed calls edges");

    conn.execute(
        &format!(
            "{fixture}
             INSERT INTO files
             SELECT printf('{FUNCTION_DIR}m%05d.rs', value), printf('hash%05d', value),
                    1, 1, 1, 1
             FROM fixture"
        ),
        (),
    )
    .await
    .expect("seed file records");

    conn
}

/// Asserts the pre-fix statement is still refused against this fixture. A
/// statement that suddenly succeeds means the fixture stopped reproducing the
/// failure, and the paged assertion beside it would prove nothing.
async fn assert_unpaged_statement_refused(conn: &TestConnection, sql: &str) {
    assert!(
        conn.query(sql, ()).await.is_err(),
        "the unpaged statement must still be refused, or this fixture no longer \
         reproduces the materialization failure: {sql}"
    );
}

/// Reads a paged scan of single-column string rows and asserts nothing was lost.
async fn paged_ids(
    conn: &TestConnection,
    page_sql: &str,
    leading: &[Value],
    cursor_index: i32,
    operation: &str,
) -> Vec<String> {
    collect_rowid_pages_with(
        &**conn,
        page_sql,
        leading,
        cursor_index,
        |row| row.get::<String>(0),
        operation,
    )
    .await
    .expect("a paged scan must not exceed the runtime materialization limit")
}

/// The live `tracedecay_health` defect: `collect_test_marker_ids` issued one
/// unbounded `SELECT id FROM nodes WHERE kind = 'annotation_usage' …`.
#[tokio::test]
async fn test_marker_ids_page_past_the_runtime_query_limit() {
    let directory = TempDir::new().expect("marker scan tempdir");
    let conn = seed_oversized_graph(&directory).await;

    assert_unpaged_statement_refused(
        &conn,
        "SELECT id FROM nodes
         WHERE kind = 'annotation_usage'
           AND (
               name = 'test'
               OR name LIKE '%::test'
               OR name = 'wasm_bindgen_test'
               OR name LIKE '%::wasm_bindgen_test'
           )",
    )
    .await;

    let markers = paged_ids(
        &conn,
        TEST_MARKER_PAGE_SQL,
        &[],
        1,
        "collect_test_marker_ids",
    )
    .await;
    assert_eq!(i64::try_from(markers.len()).expect("marker count"), ROWS);
    assert_eq!(markers.first().map(String::as_str), Some("marker::00000"));
    assert_eq!(markers.last().map(String::as_str), Some("marker::10000"));
}

/// `get_skip_test_coverage_node_ids` issued one unbounded leading-wildcard
/// `LIKE` scan over `nodes`.
#[tokio::test]
async fn skip_test_coverage_ids_page_past_the_runtime_query_limit() {
    let directory = TempDir::new().expect("skip marker tempdir");
    let conn = seed_oversized_graph(&directory).await;

    assert_unpaged_statement_refused(
        &conn,
        "SELECT id FROM nodes WHERE docstring LIKE '%skip-test-coverage%'",
    )
    .await;

    let skipped = paged_ids(
        &conn,
        SKIP_TEST_COVERAGE_PAGE_SQL,
        &[],
        1,
        "get_skip_test_coverage_node_ids",
    )
    .await;
    assert_eq!(i64::try_from(skipped.len()).expect("skip count"), ROWS);
}

/// `get_files_with_test_annotations` issued one unbounded `DISTINCT` over the
/// annotates join.
#[tokio::test]
async fn test_annotation_files_page_past_the_runtime_query_limit() {
    let directory = TempDir::new().expect("annotation file tempdir");
    let conn = seed_oversized_graph(&directory).await;

    assert_unpaged_statement_refused(
        &conn,
        "SELECT DISTINCT t.file_path \
         FROM edges e \
         JOIN nodes n ON e.source = n.id \
         JOIN nodes t ON e.target = t.id \
         WHERE n.kind = 'annotation_usage' \
           AND n.name = 'test' \
           AND e.kind = 'annotates' \
           AND t.kind IN ('function', 'method')",
    )
    .await;

    let paths = paged_ids(
        &conn,
        TEST_ANNOTATION_FILE_PAGE_SQL,
        &[],
        1,
        "get_files_with_test_annotations",
    )
    .await;
    assert_eq!(i64::try_from(paths.len()).expect("path count"), ROWS);
    // The SQL `DISTINCT` cannot survive a page cursor; the read dedupes into a
    // `HashSet` instead, and every path must still be present exactly once.
    let unique: std::collections::HashSet<&String> = paths.iter().collect();
    assert_eq!(unique.len(), paths.len());
}

/// `get_nodes_by_kind` issued one unbounded read of a whole node partition.
#[tokio::test]
async fn nodes_by_kind_page_past_the_runtime_query_limit() {
    let directory = TempDir::new().expect("kind scan tempdir");
    let conn = seed_oversized_graph(&directory).await;

    assert_unpaged_statement_refused(
        &conn,
        "SELECT id FROM nodes WHERE kind = 'annotation_usage'",
    )
    .await;

    let markers = paged_ids(
        &conn,
        NODES_BY_KIND_PAGE_SQL,
        &[Value::Text("annotation_usage".to_string())],
        super::rows::NODE_COLUMNS,
        "get_nodes_by_kind",
    )
    .await;
    assert_eq!(i64::try_from(markers.len()).expect("node count"), ROWS);
}

/// `get_nodes_by_dir` issued one unbounded read of a path prefix, which at the
/// repository root is most of the `nodes` table.
#[tokio::test]
async fn nodes_by_dir_page_past_the_runtime_query_limit() {
    let directory = TempDir::new().expect("dir scan tempdir");
    let conn = seed_oversized_graph(&directory).await;

    assert_unpaged_statement_refused(
        &conn,
        "SELECT id FROM nodes WHERE file_path LIKE 'lib/' || '%' AND kind IN ('function')",
    )
    .await;

    let nodes = paged_ids(
        &conn,
        &nodes_by_dir_page_sql(1),
        &[
            Value::Text(FUNCTION_DIR.to_string()),
            Value::Text("function".to_string()),
        ],
        super::rows::NODE_COLUMNS,
        "get_nodes_by_dir",
    )
    .await;
    assert_eq!(i64::try_from(nodes.len()).expect("node count"), ROWS);
}

/// `get_all_file_paths` and `get_stats`' language breakdown each issued one
/// unbounded whole-`files` read.
#[tokio::test]
async fn file_paths_page_past_the_runtime_query_limit() {
    let directory = TempDir::new().expect("file path tempdir");
    let conn = seed_oversized_graph(&directory).await;

    assert_unpaged_statement_refused(&conn, "SELECT path FROM files ORDER BY path").await;
    assert_unpaged_statement_refused(&conn, "SELECT path FROM files").await;

    let paths = paged_ids(&conn, FILE_PATH_PAGE_SQL, &[], 1, "get_all_file_paths").await;
    assert_eq!(i64::try_from(paths.len()).expect("file count"), ROWS);
}

/// `get_call_edges` and `get_call_edges_with_lines` read the whole `calls`
/// partition — the largest partition of the largest table — unbounded.
#[tokio::test]
async fn call_edges_page_past_the_runtime_query_limit() {
    let directory = TempDir::new().expect("call edge tempdir");
    let conn = seed_oversized_graph(&directory).await;

    assert_unpaged_statement_refused(
        &conn,
        "SELECT source, target FROM edges WHERE kind = 'calls'",
    )
    .await;
    assert_unpaged_statement_refused(
        &conn,
        "SELECT e.source, e.target, e.line FROM edges e
         JOIN nodes n ON e.source = n.id
         WHERE e.kind = 'calls' AND n.file_path LIKE 'lib/%'",
    )
    .await;

    let prefix = &[Value::Text(format!("{FUNCTION_DIR}%"))];
    for (sql, leading, cursor, operation) in [
        (CALL_EDGE_PAGE_SQL, &[][..], 2, "get_call_edges"),
        (
            CALL_EDGE_PREFIXED_PAGE_SQL,
            &prefix[..],
            2,
            "get_call_edges",
        ),
        (
            CALL_EDGE_LINE_PAGE_SQL,
            &[][..],
            3,
            "get_call_edges_with_lines",
        ),
        (
            CALL_EDGE_LINE_PREFIXED_PAGE_SQL,
            &prefix[..],
            3,
            "get_call_edges_with_lines",
        ),
    ] {
        let edges = paged_ids(&conn, sql, leading, cursor, operation).await;
        assert_eq!(
            i64::try_from(edges.len()).expect("edge count"),
            ROWS,
            "{operation} lost rows reading through {sql}"
        );
    }
}

/// `get_incoming_edges` / `get_outgoing_edges` read one endpoint unbounded. A
/// single endpoint is not a bound: a hub symbol carries more edges than one
/// query may materialize.
#[tokio::test]
async fn hub_endpoint_edges_page_past_the_runtime_query_limit() {
    let directory = TempDir::new().expect("hub edge tempdir");
    let conn = seed_oversized_graph(&directory).await;

    assert_unpaged_statement_refused(
        &conn,
        &format!("SELECT source, target, kind, line FROM edges WHERE target = '{HUB_ID}'"),
    )
    .await;

    let hub = Value::Text(HUB_ID.to_string());
    let unfiltered = paged_ids(
        &conn,
        &edges_by_endpoint_page_sql("target", 1, 0),
        std::slice::from_ref(&hub),
        super::edges::EDGE_COLUMNS,
        "get_incoming_edges",
    )
    .await;
    assert_eq!(i64::try_from(unfiltered.len()).expect("edge count"), ROWS);

    let filtered = paged_ids(
        &conn,
        &edges_by_endpoint_page_sql("target", 1, 1),
        &[hub.clone(), Value::Text("calls".to_string())],
        super::edges::EDGE_COLUMNS,
        "get_incoming_edges",
    )
    .await;
    assert_eq!(i64::try_from(filtered.len()).expect("edge count"), ROWS);

    // The mirror direction shares the builder; assert it still keys off
    // `source` rather than silently reading the same endpoint.
    let outgoing = paged_ids(
        &conn,
        &edges_by_endpoint_page_sql("source", 1, 0),
        std::slice::from_ref(&hub),
        super::edges::EDGE_COLUMNS,
        "get_outgoing_edges",
    )
    .await;
    assert!(outgoing.is_empty(), "the hub calls nothing");
}

/// `get_incoming_edges_bulk` / `get_outgoing_edges_bulk` read an id list
/// unbounded. An id list is not a bound either: a bulk frontier multiplies one
/// node's fan-out by the number of ids.
#[tokio::test]
async fn bulk_endpoint_edges_page_past_the_runtime_query_limit() {
    let directory = TempDir::new().expect("bulk edge tempdir");
    let conn = seed_oversized_graph(&directory).await;

    // Two ids whose combined fan-in exceeds one query's materialization: the
    // hub takes every `calls` edge, and one marker takes its `annotates` edge.
    assert_unpaged_statement_refused(
        &conn,
        &format!(
            "SELECT source, target, kind, line FROM edges \
             WHERE target IN ('{HUB_ID}', 'fn::00000')"
        ),
    )
    .await;

    let edges = paged_ids(
        &conn,
        &edges_by_endpoint_page_sql("target", 2, 0),
        &[
            Value::Text(HUB_ID.to_string()),
            Value::Text("fn::00000".to_string()),
        ],
        super::edges::EDGE_COLUMNS,
        "get_incoming_edges_bulk",
    )
    .await;
    assert_eq!(i64::try_from(edges.len()).expect("edge count"), ROWS + 1);
}

/// `get_nodes_by_file` and the id gather in `delete_nodes_by_file` read one
/// file's symbols unbounded. A generated or vendored file can declare more
/// symbols than one query may materialize.
#[tokio::test]
async fn nodes_by_file_page_past_the_runtime_query_limit() {
    let directory = TempDir::new().expect("file node tempdir");
    let conn = seed_oversized_graph(&directory).await;

    // Every marker node is declared by the one hub file.
    assert_unpaged_statement_refused(
        &conn,
        "SELECT id FROM nodes WHERE file_path = 'src/hub.rs' ORDER BY start_line",
    )
    .await;

    let hub_file = &[Value::Text("src/hub.rs".to_string())];
    let nodes = paged_ids(
        &conn,
        super::nodes::NODES_BY_FILE_PAGE_SQL,
        hub_file,
        super::rows::NODE_COLUMNS,
        "get_nodes_by_file",
    )
    .await;
    // Every marker plus the hub symbol itself.
    assert_eq!(i64::try_from(nodes.len()).expect("node count"), ROWS + 1);

    let ids = paged_ids(
        &conn,
        super::nodes::NODE_IDS_BY_FILE_PAGE_SQL,
        hub_file,
        1,
        "delete_nodes_by_file",
    )
    .await;
    assert_eq!(i64::try_from(ids.len()).expect("id count"), ROWS + 1);
}

/// `find_dead_code` without a limit read every candidate in one query. Its
/// cursor is the result ordering itself rather than `rowid`, so the paged
/// statement has to be exercised in that form.
#[tokio::test]
async fn dead_code_candidates_page_past_the_runtime_query_limit() {
    let directory = TempDir::new().expect("dead code tempdir");
    let conn = seed_oversized_graph(&directory).await;

    // Every seeded function is unreferenced, so the candidate set is the whole
    // `lib/` tree — more rows than one query may materialize.
    assert_unpaged_statement_refused(
        &conn,
        "SELECT id FROM nodes
         WHERE name != 'main' AND name NOT LIKE 'test%'
         AND kind = 'function'
         AND NOT EXISTS (
             SELECT 1 FROM edges WHERE target = nodes.id
             AND kind IN ('calls', 'implements', 'extends', 'type_of', 'returns', 'receives', 'uses')
         )
         ORDER BY file_path ASC, start_line ASC, id ASC",
    )
    .await;

    let page_sql = "SELECT id, file_path, start_line FROM nodes
         WHERE name != 'main' AND name NOT LIKE 'test%'
         AND kind = 'function'
         AND NOT EXISTS (
             SELECT 1 FROM edges WHERE target = nodes.id
             AND kind IN ('calls', 'implements', 'extends', 'type_of', 'returns', 'receives', 'uses')
         )
         AND (
             file_path > ?1
             OR (file_path = ?1 AND (start_line > ?2 OR (start_line = ?2 AND id > ?3)))
         )
         ORDER BY file_path ASC, start_line ASC, id ASC
         LIMIT ?4";

    // Mirrors `find_dead_code_inner`: a composite keyset on the result ordering,
    // so a small limit still stops early and an unlimited call still completes.
    const PAGE_ROWS: i64 = 2_000;
    let mut seen = Vec::new();
    let mut cursor = (String::new(), 0_i64, String::new());
    loop {
        let mut rows = conn
            .query(
                page_sql,
                (cursor.0.clone(), cursor.1, cursor.2.clone(), PAGE_ROWS),
            )
            .await
            .expect("a paged dead-code scan must not exceed the runtime limit");
        let mut page_rows = 0_i64;
        while let Some(row) = rows.next().await.expect("read dead-code row") {
            let id: String = row.get(0).expect("id");
            let file_path: String = row.get(1).expect("file_path");
            let start_line: i64 = row.get(2).expect("start_line");
            cursor = (file_path, start_line, id.clone());
            page_rows += 1;
            seen.push(id);
        }
        drop(rows);
        if page_rows < PAGE_ROWS {
            break;
        }
    }

    assert_eq!(i64::try_from(seen.len()).expect("candidate count"), ROWS);
    let unique: std::collections::HashSet<&String> = seen.iter().collect();
    assert_eq!(unique.len(), seen.len(), "a page repeated a candidate");
}

/// The shared helper's unparameterized entry point still has to page — the
/// converted whole-table reads route through it.
#[tokio::test]
async fn unparameterized_helper_pages_past_the_runtime_query_limit() {
    let directory = TempDir::new().expect("helper tempdir");
    let conn = seed_oversized_graph(&directory).await;

    let paths = collect_rowid_pages(
        &*conn,
        FILE_PATH_PAGE_SQL,
        1,
        |row| row.get::<String>(0),
        "get_all_file_paths",
    )
    .await
    .expect("a paged scan must not exceed the runtime materialization limit");
    assert_eq!(i64::try_from(paths.len()).expect("file count"), ROWS);
}

use super::super::engine::{QueryExecutor, params_from_iter};
use super::super::rows::row_to_edge;
use super::*;

async fn explain_query_plan(
    conn: &TestConnection,
    sql: &str,
    parameters: Vec<Value>,
) -> Vec<String> {
    let mut rows = conn
        .query(
            &format!("EXPLAIN QUERY PLAN {sql}"),
            params_from_iter(parameters),
        )
        .await
        .expect("explain bulk edge query");
    let mut details = Vec::new();
    while let Some(row) = rows.next().await.expect("read bulk edge query plan") {
        details.push(row.get::<String>(3).expect("query plan detail"));
    }
    details
}

fn has_endpoint_kind_seek(details: &[String], endpoint: &str, index: &str) -> bool {
    let index_seek = format!("SEARCH edges USING INDEX {index}");
    let endpoint_predicate = format!("{endpoint}=?");
    details.iter().any(|detail| {
        detail.contains(&index_seek)
            && detail.contains(&endpoint_predicate)
            && detail.contains("kind=?")
    })
}

/// A depth-two graph frontier must seek by each endpoint before sorting its
/// matched rows. Without that constraint, SQLite may scan the entire `kind`
/// partition only to discard nearly every row after testing the JSON frontier.
#[tokio::test]
async fn bulk_target_frontier_seeks_the_target_kind_index_before_sorting() {
    let directory = TempDir::new().expect("bulk target plan tempdir");
    let conn = seed_oversized_graph(&directory).await;
    let endpoints = serde_json::to_string(&[HUB_ID, "fn::00000"]).expect("encode target frontier");
    let sql = bulk_edges_by_endpoint_page_sql(EdgeEndpoint::Target, 1);

    let details = explain_query_plan(
        &conn,
        &sql,
        vec![
            Value::Text(endpoints),
            Value::Text("calls".to_owned()),
            Value::Integer(i64::MIN),
            Value::Integer(2_000),
        ],
    )
    .await;

    assert!(
        has_endpoint_kind_seek(&details, "target", "idx_edges_target_kind"),
        "bulk target traversal must seek idx_edges_target_kind before sorting: {details:?}"
    );
}

/// `get_callees` follows the same bulk cursor in the opposite direction.
/// It must seek each source endpoint before applying the global rowid ordering.
#[tokio::test]
async fn bulk_source_frontier_seeks_the_source_kind_index_before_sorting() {
    let directory = TempDir::new().expect("bulk source plan tempdir");
    let conn = seed_oversized_graph(&directory).await;
    let endpoints =
        serde_json::to_string(&["fn::00000", "fn::00001"]).expect("encode source frontier");
    let sql = bulk_edges_by_endpoint_page_sql(EdgeEndpoint::Source, 1);

    let details = explain_query_plan(
        &conn,
        &sql,
        vec![
            Value::Text(endpoints),
            Value::Text("calls".to_owned()),
            Value::Integer(i64::MIN),
            Value::Integer(2_000),
        ],
    )
    .await;

    assert!(
        has_endpoint_kind_seek(&details, "source", "idx_edges_source_kind"),
        "bulk source traversal must seek idx_edges_source_kind before sorting: {details:?}"
    );
}

#[tokio::test]
async fn bulk_target_frontier_returns_the_explicit_rowid_ordered_edge_set() {
    let directory = TempDir::new().expect("bulk target result tempdir");
    let conn = seed_oversized_graph(&directory).await;
    let endpoints = vec![HUB_ID.to_owned(), "fn::00000".to_owned()];

    let actual = read_edges_by_endpoint_controlled(
        &conn,
        EdgeEndpoint::Target,
        &endpoints,
        &[crate::types::EdgeKind::Calls],
        "bulk target frontier",
        || Ok(()),
    )
    .await
    .expect("read bulk target frontier");
    let expected = collect_rowid_pages_with(
        &conn,
        "SELECT source, target, kind, line, rowid FROM edges
         WHERE target IN (?1, ?2) AND kind = ?3 AND rowid > ?4
         ORDER BY rowid LIMIT ?5",
        &[
            Value::Text(HUB_ID.to_owned()),
            Value::Text("fn::00000".to_owned()),
            Value::Text("calls".to_owned()),
        ],
        super::super::edges::EDGE_COLUMNS,
        row_to_edge,
        "explicit bulk target frontier",
    )
    .await
    .expect("read explicit bulk target frontier");

    assert_eq!(actual, expected);
}

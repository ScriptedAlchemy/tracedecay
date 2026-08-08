//! The `nodes.symbol_occurrence_id` identity bridge (ruling DR-C as amended
//! by A1', 2026-08-08).
//!
//! Wire identity stays `nodes.id`. The occurrence is an internal join key that
//! lets a read resolve a projected symbol to its canonical relational row
//! without the ambiguous `(file, qualified_name, kind)` name key. Every
//! property that makes that safe is asserted here against the **canonical**
//! schema (`migrations::create_schema_connection`) and the **production** SQL
//! builders, so neither can drift away from the other unnoticed:
//!
//! 1. a binding belongs to exactly one generation, and a rebind clears the
//!    previous generation's bindings rather than leaving a mixed set;
//! 2. re-indexing a node clears its binding, because `insert_nodes` omits the
//!    column from its `INSERT OR REPLACE` list;
//! 3. two nodes can never claim one occurrence — the store refuses the second
//!    write instead of making the join pick a winner;
//! 4. a binding write does not churn the full-text index, which is what makes
//!    rewrite-per-publish affordable.

use tempfile::TempDir;

use super::engine::{QueryExecutor, TestConnection, Value, params_from_iter};
use super::migrations::create_schema_connection;
use super::nodes::{
    CLEAR_SYMBOL_OCCURRENCE_BINDINGS_SQL, SymbolOccurrenceBinding,
    node_ids_by_symbol_occurrence_sql, symbol_occurrence_bind_params, symbol_occurrence_bind_sql,
    validate_symbol_occurrence_bindings,
};

/// The production `INSERT OR REPLACE` column list from `insert_nodes`.
/// `symbol_occurrence_id` is deliberately absent.
const INSERT_NODE_SQL: &str = "INSERT OR REPLACE INTO nodes \
     (id,kind,name,qualified_name,file_path,\
      start_line,end_line,start_column,end_column,\
      docstring,signature,visibility,is_async,\
      branches,loops,returns,max_nesting,\
      unsafe_blocks,unchecked_calls,assertions,updated_at,attrs_start_line,parent_id) \
     VALUES (?1,'function',?2,?2,'src/lib.rs',1,3,0,1,NULL,NULL,'public',0,0,0,0,0,0,0,0,0,1,NULL)";

async fn seeded_store(directory: &TempDir) -> TestConnection {
    let conn = TestConnection::open(&directory.path().join("bridge.db"));
    create_schema_connection(&conn)
        .await
        .expect("create canonical schema");
    for (id, name) in [
        ("function:aaa", "alpha"),
        ("function:bbb", "beta"),
        ("function:ccc", "gamma"),
    ] {
        conn.execute(
            INSERT_NODE_SQL,
            params_from_iter(vec![
                Value::Text(id.to_owned()),
                Value::Text(name.to_owned()),
            ]),
        )
        .await
        .expect("seed node");
    }
    conn
}

async fn bind(conn: &TestConnection, bindings: &[SymbolOccurrenceBinding]) -> u64 {
    validate_symbol_occurrence_bindings(bindings).expect("bindings are unambiguous");
    conn.execute(CLEAR_SYMBOL_OCCURRENCE_BINDINGS_SQL, ())
        .await
        .expect("clear prior bindings");
    conn.execute(
        &symbol_occurrence_bind_sql(bindings.len()),
        params_from_iter(symbol_occurrence_bind_params(bindings)),
    )
    .await
    .expect("apply bindings")
}

async fn resolve(conn: &TestConnection, occurrences: &[&str]) -> Vec<(String, String)> {
    let values: Vec<Value> = occurrences
        .iter()
        .map(|occurrence| Value::Text((*occurrence).to_owned()))
        .collect();
    let mut rows = conn
        .query(
            &node_ids_by_symbol_occurrence_sql(occurrences.len()),
            params_from_iter(values),
        )
        .await
        .expect("resolve occurrences");
    let mut resolved = Vec::new();
    while let Some(row) = rows.next().await.expect("read resolution row") {
        resolved.push((
            row.get::<String>(0).expect("occurrence"),
            row.get::<String>(1).expect("node id"),
        ));
    }
    resolved.sort();
    resolved
}

fn binding(node_id: &str, occurrence: &str) -> SymbolOccurrenceBinding {
    SymbolOccurrenceBinding {
        node_id: node_id.to_owned(),
        symbol_occurrence_id: occurrence.to_owned(),
    }
}

#[tokio::test]
async fn a_rebind_replaces_the_previous_generation_rather_than_merging_with_it() {
    let directory = TempDir::new().expect("temp dir");
    let conn = seeded_store(&directory).await;

    let first = [
        binding("function:aaa", "symbol.v1.gen1-alpha"),
        binding("function:bbb", "symbol.v1.gen1-beta"),
    ];
    assert_eq!(bind(&conn, &first).await, 2);
    assert_eq!(
        resolve(&conn, &["symbol.v1.gen1-alpha", "symbol.v1.gen1-beta"]).await,
        vec![
            ("symbol.v1.gen1-alpha".to_owned(), "function:aaa".to_owned()),
            ("symbol.v1.gen1-beta".to_owned(), "function:bbb".to_owned()),
        ]
    );

    // The next generation mints fresh occurrence ids (the digest takes the
    // generation id as an input) and no longer mentions `function:bbb`.
    let second = [binding("function:aaa", "symbol.v1.gen2-alpha")];
    assert_eq!(bind(&conn, &second).await, 1);
    assert_eq!(
        resolve(&conn, &["symbol.v1.gen2-alpha"]).await,
        vec![("symbol.v1.gen2-alpha".to_owned(), "function:aaa".to_owned())]
    );
    assert!(
        resolve(&conn, &["symbol.v1.gen1-alpha", "symbol.v1.gen1-beta"])
            .await
            .is_empty(),
        "a superseded generation's bindings must not survive the rebind"
    );
}

#[tokio::test]
async fn re_indexing_a_node_clears_the_binding_the_publish_path_wrote() {
    let directory = TempDir::new().expect("temp dir");
    let conn = seeded_store(&directory).await;
    bind(&conn, &[binding("function:aaa", "symbol.v1.alpha")]).await;

    conn.execute(
        INSERT_NODE_SQL,
        params_from_iter(vec![
            Value::Text("function:aaa".to_owned()),
            Value::Text("alpha".to_owned()),
        ]),
    )
    .await
    .expect("re-index the node");

    assert!(
        resolve(&conn, &["symbol.v1.alpha"]).await.is_empty(),
        "a re-indexed row must not keep an occurrence minted for a generation it left"
    );
}

#[tokio::test]
async fn two_nodes_cannot_claim_one_symbol_occurrence() {
    let directory = TempDir::new().expect("temp dir");
    let conn = seeded_store(&directory).await;
    bind(&conn, &[binding("function:aaa", "symbol.v1.shared")]).await;

    let refusal = conn
        .execute(
            "UPDATE nodes SET symbol_occurrence_id = 'symbol.v1.shared' WHERE id = 'function:bbb'",
            (),
        )
        .await;
    assert!(
        refusal.is_err(),
        "the store must refuse a duplicate occurrence claim instead of leaving the join ambiguous"
    );
    assert_eq!(
        resolve(&conn, &["symbol.v1.shared"]).await,
        vec![("symbol.v1.shared".to_owned(), "function:aaa".to_owned())]
    );
}

#[test]
fn an_ambiguous_binding_set_is_refused_before_any_write() {
    assert!(
        validate_symbol_occurrence_bindings(&[
            binding("function:aaa", "symbol.v1.one"),
            binding("function:aaa", "symbol.v1.two"),
        ])
        .is_err(),
        "one node claimed by two occurrences must be refused"
    );
    assert!(
        validate_symbol_occurrence_bindings(&[
            binding("function:aaa", "symbol.v1.one"),
            binding("function:bbb", "symbol.v1.one"),
        ])
        .is_err(),
        "one occurrence claimed by two nodes must be refused"
    );
    assert!(
        validate_symbol_occurrence_bindings(&[binding("", "symbol.v1.one")]).is_err(),
        "an empty identity must be refused"
    );
    assert!(
        validate_symbol_occurrence_bindings(&[
            binding("function:aaa", "symbol.v1.one"),
            binding("function:bbb", "symbol.v1.two"),
        ])
        .is_ok()
    );
}

#[tokio::test]
async fn binding_writes_do_not_reindex_the_full_text_table() {
    let directory = TempDir::new().expect("temp dir");
    let conn = seeded_store(&directory).await;

    async fn fts_matches(conn: &TestConnection, term: &str) -> i64 {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM nodes_fts WHERE nodes_fts MATCH ?1",
                params_from_iter(vec![Value::Text(term.to_owned())]),
            )
            .await
            .expect("query full-text index");
        rows.next()
            .await
            .expect("read count row")
            .expect("count row present")
            .get::<i64>(0)
            .expect("count")
    }

    assert_eq!(fts_matches(&conn, "alpha").await, 1);
    bind(&conn, &[binding("function:aaa", "symbol.v1.alpha")]).await;
    // The trigger is scoped to the four indexed columns, so the rebind wrote no
    // full-text rows — and the index still answers for the unchanged content.
    assert_eq!(fts_matches(&conn, "alpha").await, 1);

    // A write that does name a searchable column still reindexes the row.
    conn.execute(
        "UPDATE nodes SET name = 'renamed', qualified_name = 'renamed' WHERE id = 'function:aaa'",
        (),
    )
    .await
    .expect("rename the node");
    assert_eq!(fts_matches(&conn, "alpha").await, 0);
    assert_eq!(fts_matches(&conn, "renamed").await, 1);
}

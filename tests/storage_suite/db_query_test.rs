use std::ops::Deref;

use tempfile::TempDir;
use tracedecay::db::{Database, StoredFingerprint};
use tracedecay::redundancy::Fingerprint;
use tracedecay::types::*;

use crate::support;

#[path = "db_query_test/analytics_queries.rs"]
mod analytics_queries;
#[path = "db_query_test/bootstrap.rs"]
mod bootstrap;
#[path = "db_query_test/edge_queries.rs"]
mod edge_queries;
#[path = "db_query_test/files_metadata.rs"]
mod files_metadata;
#[path = "db_query_test/insert_content.rs"]
mod insert_content;
#[path = "db_query_test/insert_edges.rs"]
mod insert_edges;
#[path = "db_query_test/node_queries.rs"]
mod node_queries;
#[path = "db_query_test/transactions.rs"]
mod transactions;

struct TestDb {
    db: Database,
    _dir: TempDir,
}

impl Deref for TestDb {
    type Target = Database;

    fn deref(&self) -> &Self::Target {
        &self.db
    }
}

async fn setup_db() -> TestDb {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("test.db");
    support::seed_latest_graph_db(&db_path).await;
    let (db, migrated) = crate::common::open_test_database(&db_path)
        .await
        .expect("failed to open template database");
    assert!(
        !migrated,
        "fresh test database should not require migration"
    );
    TestDb { db, _dir: dir }
}

/// Helper: create a sample node with reasonable defaults.
fn sample_node(id: &str, name: &str, file_path: &str) -> Node {
    Node {
        id: id.to_string(),
        kind: NodeKind::Function,
        name: name.to_string(),
        qualified_name: format!("crate::{name}"),
        file_path: file_path.to_string(),
        start_line: 1,
        attrs_start_line: 1,
        end_line: 10,
        start_column: 0,
        end_column: 1,
        signature: Some(format!("fn {name}()")),
        docstring: Some(format!("Documentation for {name}")),
        visibility: Visibility::Pub,
        is_async: false,
        branches: 0,
        loops: 0,
        returns: 0,
        max_nesting: 0,
        unsafe_blocks: 0,
        unchecked_calls: 0,
        assertions: 0,
        updated_at: 1000,
        parent_id: None,
    }
}

fn sample_edge(source: &str, target: &str, kind: EdgeKind) -> Edge {
    Edge {
        source: source.to_string(),
        target: target.to_string(),
        kind,
        line: Some(5),
    }
}

fn sample_file(path: &str) -> FileRecord {
    FileRecord {
        path: path.to_string(),
        content_hash: format!("hash_{path}"),
        size: 1024,
        modified_at: 1000,
        indexed_at: 2000,
        node_count: 3,
    }
}

async fn assert_can_start_new_transaction(db: &Database) {
    const PROBE_KEY: &str = "test.transaction_probe";
    const PROBE_VALUE: &str = "committed";

    db.set_metadata(PROBE_KEY, PROBE_VALUE)
        .await
        .expect("writer should not be left inside a transaction");
    let stored = db
        .get_metadata(PROBE_KEY)
        .await
        .expect("transaction probe should be readable");
    assert_eq!(stored.as_deref(), Some(PROBE_VALUE));
}

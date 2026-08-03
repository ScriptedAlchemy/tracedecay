use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability,
    GraphFormatVersion, NeverCancelled,
};

fn persistent_options(path: std::path::PathBuf) -> GraphDbOpenOptions {
    GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(path),
        expected_format: GraphFormatVersion::new(2).unwrap(),
        durability: GraphDurability::Sync,
        cancellation: Arc::new(NeverCancelled),
    }
}

#[test]
fn malformed_persistent_file_is_corrupt() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("graph.grafeo");
    std::fs::write(&path, b"not a grafeo database").unwrap();
    let error = GraphDb::open(persistent_options(path)).unwrap_err();
    assert!(
        matches!(error, GraphDbError::Corrupt { .. }),
        "unexpected error: {error:?}"
    );
}

#[test]
fn lock_contention_is_unavailable_not_corrupt() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("graph.grafeo");
    let first = GraphDb::open(persistent_options(path.clone())).unwrap();
    let error = GraphDb::open(persistent_options(path)).unwrap_err();
    assert!(matches!(error, GraphDbError::Unavailable { .. }));
    first.close().unwrap();
}

#[test]
fn persisted_payload_with_invalid_opaque_identity_is_corrupt() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("graph.grafeo");
    let raw = grafeo_engine::GrafeoDB::with_config(
        grafeo_engine::Config::persistent(&path)
            .with_storage_format(grafeo_engine::config::StorageFormat::SingleFile),
    )
    .unwrap();
    let mut session = raw.session();
    session.begin_transaction().unwrap();
    session
        .create_node_with_props(
            &["__tracedecay_graph_db_format"],
            [
                ("__tracedecay_graph_db_version", 2_i64.into()),
                ("__tracedecay_graph_db_sequence", 0_i64.into()),
            ],
        )
        .unwrap();
    session
        .create_node_with_props(
            &["__tracedecay_graph_db_entity"],
            [(
                "__tracedecay_graph_db_payload",
                r#"{"namespace":"","projection":"code","entity":{"identity":"entity","labels":[],"properties":{}}}"#
                    .into(),
            )],
        )
        .unwrap();
    session.commit().unwrap();
    raw.close().unwrap();
    assert!(matches!(
        GraphDb::open(persistent_options(path)),
        Err(GraphDbError::Corrupt { .. })
    ));
}

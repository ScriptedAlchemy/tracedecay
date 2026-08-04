use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphEntityId,
    GraphFormatVersion, GraphNamespace, NeverCancelled,
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
fn persisted_scalar_identity_mismatch_is_corrupt_on_point_read() {
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
                ("__tracedecay_graph_db_schema", "native-scalars-v1".into()),
                ("__tracedecay_graph_db_sequence", 0_i64.into()),
            ],
        )
        .unwrap();
    let stable_key = "776f726b7370616365:656e74697479";
    let locator = format!(
        "__tracedecay_graph_db_entity_key_{}",
        hex::encode(stable_key.as_bytes())
    );
    session
        .create_node_with_props(
            &["__tracedecay_graph_db_entity", locator.as_str()],
            [
                ("__tracedecay_graph_db_entity_key", stable_key.into()),
                ("__tracedecay_graph_db_namespace", "workspace".into()),
                ("__tracedecay_graph_db_projection", "code".into()),
                ("__tracedecay_graph_db_entity_id", "different".into()),
            ],
        )
        .unwrap();
    session.commit().unwrap();
    raw.close().unwrap();
    let db = GraphDb::open(persistent_options(path)).unwrap();
    assert!(matches!(
        db.entity(
            &GraphNamespace::new("workspace").unwrap(),
            &GraphEntityId::new("entity").unwrap(),
            Arc::new(NeverCancelled),
        ),
        Err(GraphDbError::Corrupt { .. })
    ));
}

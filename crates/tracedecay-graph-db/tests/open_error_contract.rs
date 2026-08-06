use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_graph_db::{GraphDbError, GraphEntityId, GraphNamespace, NeverCancelled};

mod support;

use support::{RegisteredGraph, graph_path};

#[test]
fn foreign_grafeo_store_without_marker_is_reset_required() {
    let temp = TempDir::new().unwrap();
    let path = graph_path(temp.path());
    let raw = grafeo_engine::GrafeoDB::with_config(
        grafeo_engine::Config::persistent(&path)
            .with_storage_format(grafeo_engine::config::StorageFormat::WalDirectory),
    )
    .unwrap();
    raw.close().unwrap();
    let error = RegisteredGraph::open(temp.path()).err().unwrap();
    assert!(
        matches!(error, GraphDbError::ResetRequired { .. }),
        "unexpected error: {error:?}"
    );
}

#[test]
fn persisted_scalar_identity_mismatch_is_corrupt_on_point_read() {
    let temp = TempDir::new().unwrap();
    let path = graph_path(temp.path());
    let raw = grafeo_engine::GrafeoDB::with_config(
        grafeo_engine::Config::persistent(&path)
            .with_storage_format(grafeo_engine::config::StorageFormat::WalDirectory),
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
    let (_, db) = RegisteredGraph::open(temp.path()).unwrap();
    assert!(matches!(
        db.entity(
            &GraphNamespace::new("workspace").unwrap(),
            &GraphEntityId::new("entity").unwrap(),
            Arc::new(NeverCancelled),
        ),
        Err(GraphDbError::Corrupt { .. })
    ));
}

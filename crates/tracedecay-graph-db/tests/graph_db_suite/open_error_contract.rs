use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use grafeo_common::types::Value;
use tempfile::TempDir;
use tracedecay_graph_db::{
    GraphDbError, GraphEntityId, GraphNamespace, GraphRelation, GraphRelationId, GraphRelationKind,
    NeverCancelled,
};

use crate::support;

use support::{RegisteredGraph, graph_path};

#[test]
fn foreign_grafeo_store_without_marker_is_reset_required() {
    let temp = TempDir::new().unwrap();
    let path = graph_path(temp.path());
    let raw = grafeo_engine::GrafeoDB::with_config(
        grafeo_engine::Config::persistent(&path)
            .with_storage_format(grafeo_engine::config::StorageFormat::SingleFile),
    )
    .unwrap();
    raw.close().unwrap();
    let error = RegisteredGraph::open_lease(temp.path()).err().unwrap();
    assert!(
        matches!(error, GraphDbError::ResetRequired { .. }),
        "unexpected error: {error:?}"
    );
}

fn raw_store(temp: &TempDir) -> grafeo_engine::GrafeoDB {
    grafeo_engine::GrafeoDB::with_config(
        grafeo_engine::Config::persistent(graph_path(temp.path()))
            .with_storage_format(grafeo_engine::config::StorageFormat::SingleFile),
    )
    .unwrap()
}

const FORMAT_MARKER: [(&str, i64); 2] = [
    ("__tracedecay_graph_db_version", 4),
    ("__tracedecay_graph_db_sequence", 0),
];

fn format_marker() -> Vec<(&'static str, Value)> {
    let mut marker: Vec<(&str, Value)> = FORMAT_MARKER
        .iter()
        .map(|(name, value)| (*name, Value::from(*value)))
        .collect();
    marker.push(("__tracedecay_graph_db_schema", "native-scalars-v1".into()));
    marker
}

#[test]
fn persisted_scalar_identity_mismatch_is_corrupt_on_point_read() {
    let temp = TempDir::new().unwrap();
    let raw = raw_store(&temp);
    let mut session = raw.session();
    session.begin_transaction().unwrap();
    session
        .create_node_with_props(&["__tracedecay_graph_db_format"], format_marker())
        .unwrap();
    session
        .create_node_with_props(
            &["__tracedecay_graph_db_entity"],
            [
                // base64url(sha256("workspace")[..8] ‖ raw-identity tag ‖ "entity").
                (
                    "__tracedecay_graph_db_entity_key",
                    "IaMjDgN3KlgAZW50aXR5".into(),
                ),
                ("__tracedecay_graph_db_namespace", "workspace".into()),
                ("__tracedecay_graph_db_projection", "code".into()),
                ("__tracedecay_graph_db_entity_id", "different".into()),
            ],
        )
        .unwrap();
    session.commit().unwrap();
    raw.close().unwrap();
    let (_, db) = RegisteredGraph::open_lease(temp.path()).unwrap();
    assert!(matches!(
        db.entity(
            &GraphNamespace::new("workspace").unwrap(),
            &GraphEntityId::new("entity").unwrap(),
            Arc::new(NeverCancelled),
        ),
        Err(GraphDbError::Corrupt { .. })
    ));
}

/// A format-4 relation as it lies on disk. Keys are base64url of
/// `sha256("workspace")[..8] ‖ digest-identity tag ‖ kind ‖ digest`. The
/// locator owns the identity, source, and target, each stored as U+0001,
/// kind, and base64url digest, and the native edge carries none of them.
/// Keyed reads and edge fan-outs both resolve the same relation.
#[test]
fn compact_relation_identities_read_back_through_keys_and_edges() {
    let temp = TempDir::new().unwrap();
    let raw = raw_store(&temp);
    let session = raw.session();
    session
        .create_node_with_props(&["__tracedecay_graph_db_format"], format_marker())
        .unwrap();
    let symbol = |key: &str, identity: String| {
        session
            .create_node_with_props(
                &["__tracedecay_graph_db_entity"],
                [
                    ("__tracedecay_graph_db_entity_key", Value::from(key)),
                    ("__tracedecay_graph_db_namespace", "workspace".into()),
                    ("__tracedecay_graph_db_projection", "code".into()),
                    ("__tracedecay_graph_db_entity_id", identity.into()),
                ],
            )
            .unwrap()
    };
    let caller = symbol(
        "IaMjDgN3KlgBc3ltYm9sERERERERERERERERERERERERERERERERERERERERERE",
        format!("symbol:{}", "11".repeat(32)),
    );
    let callee = symbol(
        "IaMjDgN3KlgBc3ltYm9sIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiI",
        format!("symbol:{}", "22".repeat(32)),
    );
    let edge = session
        .create_edge_with_props(
            caller,
            callee,
            // `__tracedecay_graph_db_relation_` + hex("calls").
            "__tracedecay_graph_db_relation_63616c6c73",
            [
                ("__tracedecay_graph_db_namespace", Value::from("workspace")),
                ("__tracedecay_graph_db_projection", Value::from("code")),
                ("__tracedecay_graph_db_relation_kind", Value::from("calls")),
            ],
        )
        .unwrap();
    session
        .create_node_with_props(
            &["__tracedecay_graph_db_relation_locator"],
            [
                (
                    "__tracedecay_graph_db_relation_key",
                    "IaMjDgN3KlgBZWRnZTMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMz".into(),
                ),
                ("__tracedecay_graph_db_namespace", "workspace".into()),
                ("__tracedecay_graph_db_projection", "code".into()),
                (
                    "__tracedecay_graph_db_relation_id",
                    "\u{1}edgeMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzM".into(),
                ),
                (
                    "__tracedecay_graph_db_relation_from",
                    "\u{1}symbolERERERERERERERERERERERERERERERERERERERERERE".into(),
                ),
                (
                    "__tracedecay_graph_db_relation_to",
                    "\u{1}symbolIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiI".into(),
                ),
                ("__tracedecay_graph_db_relation_kind", "calls".into()),
                (
                    "__tracedecay_graph_db_relation_edge",
                    i64::try_from(edge.as_u64()).unwrap().into(),
                ),
            ],
        )
        .unwrap();
    raw.close().unwrap();

    let (_, db) = RegisteredGraph::open_lease(temp.path()).unwrap();
    let workspace = GraphNamespace::new("workspace").unwrap();
    let relation_id = GraphRelationId::new(format!("edge:{}", "33".repeat(32))).unwrap();
    let expected = GraphRelation::new(
        relation_id.clone(),
        GraphEntityId::new(format!("symbol:{}", "11".repeat(32))).unwrap(),
        GraphEntityId::new(format!("symbol:{}", "22".repeat(32))).unwrap(),
        GraphRelationKind::new("calls").unwrap(),
        BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        db.relation(&workspace, &relation_id, Arc::new(NeverCancelled))
            .unwrap(),
        Some(expected.clone())
    );
    let starts = [expected.from.clone()];
    assert_eq!(
        db.outgoing_relations(
            &workspace,
            &starts,
            &BTreeSet::new(),
            16,
            Arc::new(NeverCancelled)
        )
        .unwrap(),
        vec![vec![expected]]
    );
    assert_eq!(
        db.outgoing_relation_ids(
            &workspace,
            &starts,
            &BTreeSet::new(),
            16,
            Arc::new(NeverCancelled)
        )
        .unwrap(),
        vec![vec![relation_id]]
    );
}

use tracedecay_code_index::graph_projection::code_graph_generation_id;
use tracedecay_domain::CodeGenerationId;
use tracedecay_graph_db::GraphProjectorRevision;

#[test]
fn projector_revision_changes_the_immutable_graph_generation_identity() {
    let code_generation = CodeGenerationId::new("generation:sealed-source").unwrap();
    let first = GraphProjectorRevision::try_from("code-graph-projector.v1".to_owned()).unwrap();
    let changed = GraphProjectorRevision::try_from("code-graph-projector.v2".to_owned()).unwrap();

    let first_identity = code_graph_generation_id(&code_generation, &first).unwrap();
    let replay_identity = code_graph_generation_id(&code_generation, &first).unwrap();
    let changed_identity = code_graph_generation_id(&code_generation, &changed).unwrap();

    assert_eq!(first_identity, replay_identity);
    assert_ne!(first_identity, changed_identity);
}

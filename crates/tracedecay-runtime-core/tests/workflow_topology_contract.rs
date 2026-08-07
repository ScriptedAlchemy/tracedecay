use std::collections::BTreeSet;
use std::sync::Arc;

use tracedecay_domain::{
    ManifestDigest, ProjectId, WorkflowDefinition, WorkflowDefinitionId, WorkflowOperationRef,
    WorkflowStep, WorkflowStepId,
};
use tracedecay_graph_db::{
    GraphIdempotencyKey, GraphNamespace, GraphProjectorRevision, NeverCancelled,
    VerifiedGraphSnapshot,
};
use tracedecay_runtime_core::workflow_topology::{
    WORKFLOW_TOPOLOGY_PROJECTOR_REVISION_V1, WorkflowTopologyError, WorkflowTopologyStore,
    build_workflow_topology_manifest_checked, workflow_topology_idempotency_key,
    workflow_topology_namespace, workflow_topology_projection_identity,
};

fn digest(label: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", label.to_string().repeat(64))).expect("digest")
}

fn step_id(label: &str) -> WorkflowStepId {
    WorkflowStepId::new(format!("step.{label}")).expect("step ID")
}

fn step(label: &str, predecessors: &[&str]) -> WorkflowStep {
    WorkflowStep {
        step_id: step_id(label),
        operation: WorkflowOperationRef::new(format!("operation.{label}")).expect("operation"),
        predecessors: predecessors
            .iter()
            .map(|predecessor| step_id(predecessor))
            .collect(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        fan_out: None,
    }
}

fn definition(version: u64) -> WorkflowDefinition {
    WorkflowDefinition::new(
        WorkflowDefinitionId::new("workflow.fixture").expect("definition ID"),
        version,
        ProjectId::new("project.workflow-topology").expect("project"),
        vec![
            step("d", &["b", "c"]),
            step("c", &["a"]),
            step("a", &[]),
            step("b", &["a"]),
        ],
        digest('a'),
        digest('b'),
        digest('c'),
    )
    .expect("definition")
}

fn store(definition: &WorkflowDefinition) -> WorkflowTopologyStore {
    WorkflowTopologyStore::publish_from_definition(definition, &|| Ok(()), |manifest, key| {
        assert_eq!(
            key,
            GraphIdempotencyKey::new(format!("publish:{}", manifest.generation.as_str()))
                .expect("idempotency")
        );
        VerifiedGraphSnapshot::memory(manifest.clone(), Arc::new(NeverCancelled))
    })
    .expect("store")
}

#[test]
fn workflow_topology_orders_ready_steps_and_fan_out_deterministically() {
    let definition = definition(1);
    let store = store(&definition);
    let cancellation = Arc::new(NeverCancelled);

    assert_eq!(
        store
            .ready_steps(&BTreeSet::new(), 8, cancellation.clone())
            .expect("initial ready"),
        vec![step_id("a")]
    );
    assert_eq!(
        store
            .ready_steps(&BTreeSet::from([step_id("a")]), 8, cancellation.clone())
            .expect("fan-out ready"),
        vec![step_id("b"), step_id("c")]
    );
    assert_eq!(
        store
            .topological_order(cancellation.clone())
            .expect("topological order"),
        vec![step_id("a"), step_id("b"), step_id("c"), step_id("d")]
    );
    let mut descendants = store
        .descendants(&step_id("a"), 8, 16, cancellation)
        .expect("descendants");
    descendants.sort();
    assert_eq!(descendants, vec![step_id("b"), step_id("c"), step_id("d")]);
}

#[test]
fn workflow_topology_replay_is_exact_and_stale_definition_is_rejected() {
    let original_definition = definition(1);
    let identity = workflow_topology_projection_identity(
        GraphNamespace::new("workflow-topology-test").expect("namespace"),
    )
    .expect("identity");
    let revision =
        GraphProjectorRevision::try_from(WORKFLOW_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned())
            .expect("revision");
    let original = build_workflow_topology_manifest_checked(
        identity.clone(),
        &original_definition,
        &revision,
        &|| Ok(()),
    )
    .expect("original");
    let replayed =
        build_workflow_topology_manifest_checked(identity, &definition(1), &revision, &|| Ok(()))
            .expect("replayed");
    assert_eq!(original.generation, replayed.generation);
    assert_eq!(
        original
            .expected_recovered_digest(&|| Ok(()))
            .expect("original digest"),
        replayed
            .expected_recovered_digest(&|| Ok(()))
            .expect("replayed digest")
    );

    let snapshot = VerifiedGraphSnapshot::memory(original, Arc::new(NeverCancelled))
        .expect("verified snapshot");
    assert!(matches!(
        WorkflowTopologyStore::from_verified_snapshot(snapshot, &definition(2)),
        Err(WorkflowTopologyError::GenerationMismatch)
    ));
}

#[test]
fn workflow_topology_publication_identity_is_content_addressed() {
    let current_definition = definition(1);
    let revision =
        GraphProjectorRevision::try_from(WORKFLOW_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned())
            .expect("revision");
    let namespace = workflow_topology_namespace(&current_definition).expect("namespace");
    let identity =
        workflow_topology_projection_identity(namespace.clone()).expect("projection identity");
    let manifest =
        build_workflow_topology_manifest_checked(identity, &current_definition, &revision, &|| {
            Ok(())
        })
        .expect("manifest");

    assert_eq!(
        namespace,
        workflow_topology_namespace(&definition(2)).expect("next-version namespace")
    );
    assert_eq!(
        workflow_topology_idempotency_key(&current_definition, &revision).expect("idempotency"),
        GraphIdempotencyKey::new(format!("publish:{}", manifest.generation.as_str()))
            .expect("expected idempotency")
    );
}

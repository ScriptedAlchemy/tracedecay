use tracedecay_domain::{
    BranchStackEdgeV1, BranchStackId, BranchStackNodeV1, BranchStackRevisionId,
    BranchStackRevisionV1, BranchStackSourceV1, CommitId, DomainError, ProjectId, RefId,
    RepositoryId, StackNodeId, WorktreeId, WorktreeInventoryEpoch, WorktreeInventorySnapshotId,
};

fn node(node: &str, reference: &str, tip: &str, worktree: Option<&str>) -> BranchStackNodeV1 {
    BranchStackNodeV1 {
        node_id: StackNodeId::new(node).unwrap(),
        project_id: ProjectId::new("project.fixture").unwrap(),
        repository_id: RepositoryId::new("repository.fixture").unwrap(),
        reference: RefId::new(reference).unwrap(),
        tip: CommitId::new(tip).unwrap(),
        worktree_id: worktree.map(|value| WorktreeId::new(value).unwrap()),
    }
}

fn revision(
    nodes: Vec<BranchStackNodeV1>,
    edges: Vec<BranchStackEdgeV1>,
) -> Result<BranchStackRevisionV1, DomainError> {
    BranchStackRevisionV1::new(
        BranchStackId::new("branch-stack.fixture").unwrap(),
        BranchStackRevisionId::new("branch-stack-revision.fixture.1").unwrap(),
        WorktreeInventorySnapshotId::new("worktree-inventory.fixture.1").unwrap(),
        WorktreeInventoryEpoch::new(7)?,
        BranchStackSourceV1::ExplicitDeclaration,
        nodes,
        edges,
    )
}

#[test]
fn stack_revision_canonicalizes_nodes_and_edges_without_path_identity() {
    let revision = revision(
        vec![
            node(
                "stack-node.dependent",
                "refs/heads/dependent",
                "commit.dependent",
                Some("worktree.dependent"),
            ),
            node(
                "stack-node.base",
                "refs/heads/base",
                "commit.base",
                Some("worktree.base"),
            ),
        ],
        vec![BranchStackEdgeV1 {
            dependency: StackNodeId::new("stack-node.base").unwrap(),
            dependent: StackNodeId::new("stack-node.dependent").unwrap(),
        }],
    )
    .unwrap();

    revision.validate().unwrap();
    assert_eq!(revision.nodes[0].node_id.as_str(), "stack-node.base");
    assert_eq!(revision.edges[0].dependency.as_str(), "stack-node.base");
    assert_eq!(
        revision.canonical_order(),
        &[
            StackNodeId::new("stack-node.base").unwrap(),
            StackNodeId::new("stack-node.dependent").unwrap(),
        ]
    );
    assert_eq!(revision.inventory_epoch.get(), 7);
    assert_eq!(revision.digest, revision.compute_digest().unwrap());

    let encoded = serde_json::to_value(&revision).unwrap();
    let object = encoded.as_object().unwrap();
    assert!(!object.contains_key("repository_root"));
    assert!(!object.contains_key("worktree_path"));
    assert!(!object.contains_key("provider"));
}

#[test]
fn stack_revision_schema_exports_declared_topology_nodes_and_edges() {
    let schema = serde_json::to_value(schemars::schema_for!(BranchStackRevisionV1))
        .expect("branch-stack schema");
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .expect("branch-stack schema properties");

    assert!(properties.contains_key("nodes"));
    assert!(properties.contains_key("edges"));
    assert!(properties.contains_key("source"));
}

#[test]
fn stack_revision_rejects_cross_repository_nodes_and_duplicate_refs() {
    let mut foreign = node(
        "stack-node.foreign",
        "refs/heads/foreign",
        "commit.foreign",
        None,
    );
    foreign.repository_id = RepositoryId::new("repository.other").unwrap();
    assert_eq!(
        revision(
            vec![
                node("stack-node.base", "refs/heads/base", "commit.base", None,),
                foreign,
            ],
            vec![],
        ),
        Err(DomainError::SnapshotMismatch {
            field: "branch stack node repository",
        })
    );

    assert_eq!(
        revision(
            vec![
                node("stack-node.base", "refs/heads/shared", "commit.base", None,),
                node(
                    "stack-node.dependent",
                    "refs/heads/shared",
                    "commit.dependent",
                    None,
                ),
            ],
            vec![],
        ),
        Err(DomainError::DuplicateId {
            field: "branch stack node reference",
        })
    );
}

#[test]
fn stack_revision_rejects_missing_self_and_cyclic_edges() {
    let base = node("stack-node.base", "refs/heads/base", "commit.base", None);
    let dependent = node(
        "stack-node.dependent",
        "refs/heads/dependent",
        "commit.dependent",
        None,
    );

    assert!(matches!(
        revision(
            vec![base.clone()],
            vec![BranchStackEdgeV1 {
                dependency: StackNodeId::new("stack-node.missing").unwrap(),
                dependent: base.node_id.clone(),
            }],
        ),
        Err(DomainError::UnknownReference {
            field: "branch stack edge node",
        })
    ));
    assert_eq!(
        revision(
            vec![base.clone()],
            vec![BranchStackEdgeV1 {
                dependency: base.node_id.clone(),
                dependent: base.node_id.clone(),
            }],
        ),
        Err(DomainError::NonCanonical {
            field: "branch stack self edge",
        })
    );
    assert_eq!(
        revision(
            vec![base.clone(), dependent.clone()],
            vec![
                BranchStackEdgeV1 {
                    dependency: base.node_id.clone(),
                    dependent: dependent.node_id.clone(),
                },
                BranchStackEdgeV1 {
                    dependency: dependent.node_id,
                    dependent: base.node_id,
                },
            ],
        ),
        Err(DomainError::NonCanonical {
            field: "branch stack cycle",
        })
    );
}

#[test]
fn stack_revision_detects_identity_and_inventory_tampering() {
    let mut revision = revision(
        vec![node(
            "stack-node.base",
            "refs/heads/base",
            "commit.base",
            Some("worktree.base"),
        )],
        vec![],
    )
    .unwrap();

    revision.revision_id = BranchStackRevisionId::new("branch-stack-revision.fixture.2").unwrap();
    assert_eq!(revision.validate(), Err(DomainError::DigestMismatch));

    assert!(WorktreeInventoryEpoch::new(0).is_err());
}

//! `lsp` module test coverage (split from the former monolithic
//! `invocation::tests` module).

use super::*;

#[test]
fn lsp_scope_roots_canonicalize_independent_of_folder_order() {
    let scope_a = ResolvedScope::new(
        ProjectId::new("project.a").unwrap(),
        tracedecay_domain::RepositoryId::new("repository.a").unwrap(),
        tracedecay_domain::WorktreeId::new("worktree.a").unwrap(),
        None,
    )
    .unwrap();
    let scope_b = ResolvedScope::new(
        ProjectId::new("project.b").unwrap(),
        tracedecay_domain::RepositoryId::new("repository.b").unwrap(),
        tracedecay_domain::WorktreeId::new("worktree.b").unwrap(),
        None,
    )
    .unwrap();
    let mut forward = vec![
        (PathBuf::from("/a"), "file:///a".to_owned(), scope_a.clone()),
        (PathBuf::from("/b"), "file:///b".to_owned(), scope_b.clone()),
    ];
    let mut reverse = vec![
        (PathBuf::from("/b"), "file:///b".to_owned(), scope_b),
        (PathBuf::from("/a"), "file:///a".to_owned(), scope_a),
    ];

    assert!(canonicalize_lsp_roots(&mut forward));
    assert!(canonicalize_lsp_roots(&mut reverse));
    assert_eq!(forward, reverse);
}

#[tokio::test]
async fn linked_workspace_owner_requires_its_exact_registered_scope() {
    let root = PathBuf::from("/linked/worktree");
    let expected = ResolvedScope::new(
        ProjectId::new("project.linked").unwrap(),
        tracedecay_domain::RepositoryId::new("repository.linked").unwrap(),
        tracedecay_domain::WorktreeId::new("worktree.expected").unwrap(),
        None,
    )
    .unwrap();
    let sibling = ResolvedScope::new(
        ProjectId::new("project.linked").unwrap(),
        tracedecay_domain::RepositoryId::new("repository.linked").unwrap(),
        tracedecay_domain::WorktreeId::new("worktree.sibling").unwrap(),
        None,
    )
    .unwrap();
    let capability =
        CapabilityId::new(crate::daemon::project_open_owners::LSP_WORKSPACE_CAPABILITY_ID_V1)
            .unwrap();
    let use_case =
        UseCaseId::new(crate::daemon::project_open_owners::LSP_WORKSPACE_USE_CASE_ID_V1).unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.lsp.linked").unwrap(),
        1,
        canonical_sha256(&"grant.lsp.linked").unwrap(),
        ActorId::new("actor.lsp.linked").unwrap(),
        UtcMicros(1),
        UtcMicros(10_000),
        expected.clone(),
        std::collections::BTreeSet::from([capability]),
        std::collections::BTreeSet::from([use_case]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    let service = DaemonInvocationService::default();
    service
        .install_lsp_owner(
            root.clone(),
            DaemonLspInvocationOwner {
                factory: unavailable_lsp_session_factory(),
                scope_grant: Some(grant),
                scope_set_storage: None,
            },
        )
        .await
        .unwrap();

    assert!(service.lsp_owner_matches_scope(&root, &expected).await);
    assert!(!service.lsp_owner_matches_scope(&root, &sibling).await);
}

use tracedecay_application::{
    WorkflowProviderPlacementErrorV1, WorkflowProviderPlacementServiceV1,
    WorkflowProviderRegistrationV1, WorkflowProviderRegistryV1, WorkflowTopologyPlacementRequestV1,
};
use tracedecay_domain::configuration::safe_work_topology_policy_v1;
use tracedecay_domain::{
    ManifestDigest, ProviderId, RunId, WorkProviderBackendV1, WorkProviderRouteId,
    WorkProviderRouteV1, WorkflowStepId,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn registration(
    provider: &str,
    route: &str,
    backend: WorkProviderBackendV1,
    model: &str,
    priority: u32,
) -> WorkflowProviderRegistrationV1 {
    WorkflowProviderRegistrationV1::new(
        WorkProviderRouteV1::new(id::<ProviderId>(provider), id::<WorkProviderRouteId>(route))
            .unwrap(),
        backend,
        model.to_owned(),
        priority,
    )
    .unwrap()
}

#[test]
fn placement_is_registry_backed_and_pins_the_topology_decision() {
    let configuration_digest = digest('a');
    let registry = WorkflowProviderRegistryV1::new(
        configuration_digest.clone(),
        vec![
            registration(
                "provider.work.claude-code-cli",
                "route.work.claude-code-cli.v1",
                WorkProviderBackendV1::ClaudeCodeCli,
                "claude-sonnet",
                20,
            ),
            registration(
                "provider.work.codex-app-server",
                "route.work.codex-app-server.v1",
                WorkProviderBackendV1::CodexAppServer,
                "gpt-5.6",
                10,
            ),
        ],
    )
    .unwrap();
    let policy = safe_work_topology_policy_v1();
    let topology_digest = policy.compute_digest().unwrap().0;
    let request = WorkflowTopologyPlacementRequestV1 {
        run_id: id::<RunId>("run.workflow.provider"),
        step_id: id::<WorkflowStepId>("prepare"),
        configuration_digest,
        topology_digest: topology_digest.clone(),
    };

    let receipt = WorkflowProviderPlacementServiceV1::new(registry.clone())
        .place(&request, &policy)
        .unwrap();

    assert_eq!(
        receipt.route().provider_id().as_str(),
        "provider.work.codex-app-server"
    );
    assert_eq!(receipt.backend(), WorkProviderBackendV1::CodexAppServer);
    assert_eq!(receipt.model(), "gpt-5.6");
    assert_eq!(receipt.topology_digest(), &topology_digest);
    assert_eq!(receipt.provider_registry_digest(), registry.digest());
    assert_eq!(receipt.worktree_placement(), &policy.placement);
}

#[test]
fn placement_rejects_stale_configuration_and_topology() {
    let configuration_digest = digest('a');
    let registry = WorkflowProviderRegistryV1::new(
        configuration_digest.clone(),
        vec![registration(
            "provider.work.codex-app-server",
            "route.work.codex-app-server.v1",
            WorkProviderBackendV1::CodexAppServer,
            "gpt-5.6",
            10,
        )],
    )
    .unwrap();
    let policy = safe_work_topology_policy_v1();
    let service = WorkflowProviderPlacementServiceV1::new(registry);

    for (configuration_digest, topology_digest, expected) in [
        (
            digest('9'),
            policy.compute_digest().unwrap().0,
            WorkflowProviderPlacementErrorV1::ConfigurationDigestMismatch,
        ),
        (
            configuration_digest,
            digest('9'),
            WorkflowProviderPlacementErrorV1::TopologyDigestMismatch,
        ),
    ] {
        assert_eq!(
            service
                .place(
                    &WorkflowTopologyPlacementRequestV1 {
                        run_id: id::<RunId>("run.workflow.provider.stale"),
                        step_id: id::<WorkflowStepId>("prepare"),
                        configuration_digest,
                        topology_digest,
                    },
                    &policy,
                )
                .unwrap_err(),
            expected
        );
    }
}

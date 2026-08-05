use std::collections::BTreeSet;

use tracedecay_domain::{
    ConfigurationRevisionId, ConfigurationSnapshotId, CredentialReferenceId, ManifestDigest,
    ProviderId, UtcMicros, WorkApprovalPolicy, WorkEgressPolicy, WorkExecutableReference,
    WorkExecutionLimits, WorkExecutionSnapshot, WorkExecutionSnapshotInput, WorkFallbackTopology,
    WorkFilesystemPolicy, WorkProviderBackendV1, WorkProviderFallbackDispositionV1,
    WorkProviderProtocol, WorkProviderRouteId, WorkProviderRouteV1, WorkProviderSelectionReceiptV1,
    WorkRuntimeContractError, WorkSandboxPolicy,
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

fn route(provider: &str, route: &str) -> WorkProviderRouteV1 {
    WorkProviderRouteV1::new(id::<ProviderId>(provider), id::<WorkProviderRouteId>(route)).unwrap()
}

fn executable(name: &str, byte: char) -> WorkExecutableReference {
    WorkExecutableReference::new(name.to_owned(), digest(byte)).unwrap()
}

fn input() -> WorkExecutionSnapshotInput {
    WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>("configuration-revision.work.1"),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>("configuration-snapshot.work.1"),
        effective_behavior_digest: digest('a'),
        resolution_provenance_digest: digest('b'),
        route: route(
            "provider.work.codex-app-server",
            "route.work.codex-app-server.primary",
        ),
        backend: WorkProviderBackendV1::CodexAppServer,
        protocol: WorkProviderProtocol::CodexAppServerJsonRpc,
        model: "gpt-test".to_owned(),
        executable: executable("executable.codex.app-server", 'c'),
        sandbox: WorkSandboxPolicy::Required,
        approval: WorkApprovalPolicy::OnRequest,
        filesystem: WorkFilesystemPolicy::WorkspaceWrite,
        egress: WorkEgressPolicy::Deny,
        environment_allowlist: BTreeSet::from(["PATH".to_owned(), "TMPDIR".to_owned()]),
        credential_references: BTreeSet::from([id::<CredentialReferenceId>(
            "credential-reference.codex",
        )]),
        limits: WorkExecutionLimits::new(128_000, 8_192, 65_536, 65_536, 262_144, 4).unwrap(),
        deadline: UtcMicros(5_000_000),
        fallback: WorkFallbackTopology::CodexCli {
            route: route("provider.work.codex-cli", "route.work.codex-cli.fallback"),
            executable: executable("executable.codex.cli", 'd'),
        },
        topology_policy_digest: digest('e'),
    }
}

#[test]
fn execution_snapshot_pins_the_complete_provider_authority() {
    let snapshot = WorkExecutionSnapshot::new(input()).unwrap();

    assert_eq!(
        snapshot.configuration_revision_id().as_str(),
        "configuration-revision.work.1"
    );
    assert_eq!(snapshot.backend(), WorkProviderBackendV1::CodexAppServer);
    assert_eq!(
        snapshot.protocol(),
        WorkProviderProtocol::CodexAppServerJsonRpc
    );
    assert_eq!(snapshot.environment_allowlist().len(), 2);
    assert_eq!(snapshot.credential_references().len(), 1);
    assert!(matches!(
        snapshot.fallback(),
        WorkFallbackTopology::CodexCli { .. }
    ));

    let wire = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(wire["sandbox"], "required");
    assert_eq!(wire["egress"], "deny");
    assert_eq!(wire["limits"]["max_concurrency"], 4);
}

#[test]
fn execution_snapshot_rejects_backend_protocol_drift() {
    let mut input = input();
    input.protocol = WorkProviderProtocol::CodexExecJson;

    assert_eq!(
        WorkExecutionSnapshot::new(input),
        Err(WorkRuntimeContractError::InvalidExecutionSnapshot)
    );
}

#[test]
fn provider_selection_records_that_the_pinned_fallback_was_not_used() {
    let snapshot = WorkExecutionSnapshot::new(input()).unwrap();
    let requested = snapshot.route().clone();
    let primary = WorkProviderSelectionReceiptV1::primary(&snapshot, requested.clone()).unwrap();
    assert_eq!(primary.requested_route(), &requested);
    assert_eq!(primary.actual_route(), &requested);

    let WorkFallbackTopology::CodexCli { route, executable } = snapshot.fallback() else {
        panic!("fixture must pin a fallback");
    };
    assert_eq!(
        primary.fallback(),
        &WorkProviderFallbackDispositionV1::NotUsed {
            topology_policy_digest: snapshot.topology_policy_digest().clone(),
            route: route.clone(),
            executable: executable.clone(),
        }
    );

    let mut wire = serde_json::to_value(&primary).unwrap();
    wire["actual_route"] =
        serde_json::to_value(route("provider.work.codex-cli", "route.work.unrecorded")).unwrap();
    assert!(serde_json::from_value::<WorkProviderSelectionReceiptV1>(wire).is_err());
}

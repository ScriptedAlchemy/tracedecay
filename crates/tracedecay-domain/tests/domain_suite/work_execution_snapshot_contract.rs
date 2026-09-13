use std::collections::BTreeSet;

use tracedecay_domain::{
    AutomaticWorktreeGcV1, ConfigurationRevisionId, ConfigurationSnapshotId, CredentialReferenceId,
    CrossMergeModeV1, ManifestDigest, ProviderId, TopologyNotificationLevelV1, UtcMicros,
    WorkApprovalPolicy, WorkEgressPolicy, WorkExecutableReference, WorkExecutionLimits,
    WorkExecutionSnapshot, WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFilesystemPolicy,
    WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteId, WorkProviderRouteV1,
    WorkRuntimeContractError, WorkSandboxPolicy, WorktreeCleanlinessRequirementV1,
    safe_work_topology_policy_v1,
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
        topology: safe_work_topology_policy_v1(),
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
fn execution_snapshot_names_its_topology_constraints_inline() {
    let snapshot = WorkExecutionSnapshot::new(input()).unwrap();

    let topology = snapshot.topology();
    assert!(topology.meets_protected_ref_floor());
    assert_eq!(
        topology.notifications,
        TopologyNotificationLevelV1::CriticalOnly
    );
    assert_eq!(
        topology.cross_merge.default_mode,
        CrossMergeModeV1::Disabled
    );
    assert_eq!(
        topology.gates.cleanliness,
        WorktreeCleanlinessRequirementV1::RequireClean
    );
    assert_eq!(
        topology.retention.automatic_gc,
        AutomaticWorktreeGcV1::Disabled
    );

    // The named constraints reach the wire; the reader never has to resolve an
    // opaque digest against a mutable configuration store.
    let wire = serde_json::to_value(&snapshot).unwrap();
    assert!(wire["topology"]["protected_refs"].is_array());
    assert_eq!(wire["topology"]["notifications"], "critical_only");
    assert_eq!(
        wire["topology"]["placement"]["kind"],
        "existing_worktree_only"
    );

    let decoded: WorkExecutionSnapshot = serde_json::from_value(wire).unwrap();
    assert_eq!(decoded, snapshot);
}

#[test]
fn execution_snapshot_refuses_a_topology_below_the_protected_ref_floor() {
    let mut input = input();
    input.topology.protected_refs.clear();

    assert_eq!(
        WorkExecutionSnapshot::new(input),
        Err(WorkRuntimeContractError::InvalidExecutionSnapshot)
    );
}

#[test]
fn execution_snapshot_refuses_a_native_integration_without_its_gates() {
    let mut input = input();
    input.topology.cross_merge.allowed_modes = BTreeSet::from([
        CrossMergeModeV1::Disabled,
        CrossMergeModeV1::FastForwardOnly,
    ]);

    // Native fast-forward integration requires clean/test/preflight gates, and
    // the safe default carries no required test.
    assert_eq!(
        WorkExecutionSnapshot::new(input),
        Err(WorkRuntimeContractError::InvalidExecutionSnapshot)
    );
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

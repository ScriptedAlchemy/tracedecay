use super::{
    CapabilityGrantSnapshot, DaemonWorkProposalRoutingAuthorityV1, ManifestDigest, ResolvedScope,
};
use std::collections::{BTreeMap, BTreeSet};
use tracedecay_domain::test_fixtures::id;
use tracedecay_domain::{
    ManifestDigestHasher, ProviderId, WorkApprovalPolicy, WorkContentLocationClassV1,
    WorkEffortClassV1, WorkEgressPolicy, WorkExecutableReference, WorkExecutionLimits,
    WorkFallbackTopology, WorkFilesystemPolicy, WorkOrdinalBandV1, WorkProviderBackendV1,
    WorkProviderRouteId, WorkProviderRouteV1, WorkRouteCandidateV1, WorkRouteExecutionProfileV1,
    WorkSandboxPolicy,
};
use tracedecay_runtime_core::test_executable::write_executable_script;

pub(super) fn provider_route() -> WorkProviderRouteV1 {
    WorkProviderRouteV1::new(
        id::<ProviderId>("provider.work.codex-cli"),
        id::<WorkProviderRouteId>("route.work.evidence-codex.v1"),
    )
    .expect("provider route")
}

pub(super) fn configured_work_proposal_routing(
    project: &std::path::Path,
    scope: ResolvedScope,
    grant: &CapabilityGrantSnapshot,
) -> (DaemonWorkProposalRoutingAuthorityV1, ManifestDigest) {
    use tracedecay_domain::configuration::{
        ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationValueV1, SettingKey,
        WORK_EXECUTABLE_BINDINGS_SETTING_KEY, WorkExecutableBindingV1, WorkExecutableCapabilityV1,
    };

    let executable_bytes = b"#!/bin/sh\nexit 0\n";
    let executable_path = project.join("work-evidence-provider");
    write_executable_script(&executable_path, executable_bytes).expect("write provider executable");
    let executable_path = executable_path
        .canonicalize()
        .expect("canonical provider executable");
    let mut hasher = ManifestDigestHasher::new();
    hasher.update(executable_bytes);
    let executable = WorkExecutableReference::new(
        "executable.work.evidence-provider".to_owned(),
        hasher.finalize().expect("provider executable digest"),
    )
    .expect("provider executable reference");
    let route = WorkRouteCandidateV1 {
        route_id: provider_route().route_id().as_str().to_owned(),
        provider_capability_id: WorkProviderBackendV1::CodexCli
            .provider_id()
            .as_str()
            .to_owned(),
        model_id: "gpt-5.6".to_owned(),
        effort: WorkEffortClassV1::Standard,
        declared_budget_ceiling: 1,
        content_location: WorkContentLocationClassV1::Local,
        correctness: WorkOrdinalBandV1::High,
        sensitive_data_fitness: WorkOrdinalBandV1::High,
        latency: WorkOrdinalBandV1::Moderate,
        cost: WorkOrdinalBandV1::Moderate,
        autonomy: WorkOrdinalBandV1::High,
        evidence_quality: WorkOrdinalBandV1::High,
        execution: WorkRouteExecutionProfileV1 {
            sandbox: WorkSandboxPolicy::Required,
            approval: WorkApprovalPolicy::Never,
            filesystem: WorkFilesystemPolicy::WorkspaceWrite,
            egress: WorkEgressPolicy::Deny,
            environment_allowlist: BTreeSet::new(),
            credential_references: BTreeSet::new(),
            limits: WorkExecutionLimits::new(128_000, 8_192, 16_384, 16_384, 65_536, 1)
                .expect("execution limits"),
            maximum_duration_micros: 60_000_000,
            fallback: WorkFallbackTopology::Disabled,
        },
    };
    let binding = WorkExecutableBindingV1::new(
        executable,
        executable_path,
        vec![WorkExecutableCapabilityV1::CodexCliExecJson],
        vec![route],
    )
    .expect("configured provider binding");
    let revision = ConfigurationRevisionId::new("configuration.revision.work-evidence-routing")
        .expect("configuration revision");
    let key = SettingKey::new(WORK_EXECUTABLE_BINDINGS_SETTING_KEY)
        .expect("work executable bindings key");
    let snapshot = tracedecay_project::config::resolver::resolve_configuration(
        &tracedecay_project::config::registry::ConfigurationRegistry::core()
            .expect("configuration registry defaults"),
        &[tracedecay_project::config::resolver::ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::Project {
                project_id: scope.project_id.clone(),
            },
            revision_id: revision.clone(),
            entries: BTreeMap::from([(
                key,
                ConfigurationValueV1::WorkExecutableBindings(vec![binding]),
            )]),
        }],
    )
    .expect("configured Work routing")
    .snapshot;
    let configuration_digest = snapshot.effective_behavior_digest.clone();
    let configuration = tracedecay_configuration::config::PinnedRuntimeConfiguration::new(
        tracedecay_configuration::config::RuntimeConfigurationTarget {
            profile_root: project.join("profile"),
            project_id: scope.project_id.clone(),
            project_root: project.to_path_buf(),
        },
        revision,
        snapshot,
    )
    .expect("pinned Work routing configuration");
    let routing = DaemonWorkProposalRoutingAuthorityV1::mount(
        scope,
        &configuration,
        &configuration_digest,
        grant,
    )
    .expect("configured Work proposal routing");
    (routing, configuration_digest)
}

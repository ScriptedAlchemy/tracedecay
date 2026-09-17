//! Project-open source-access and Work-grant authorization.

use crate::source_authorization::ProjectSourceAccessSnapshot;
use tracedecay_configuration::config::registry::ConfigurationRegistry;
use tracedecay_configuration::config::resolver::{ConfigurationLayerV1, resolve_configuration};
use tracedecay_contracts::ResolvedScope;
use tracedecay_domain::configuration::{
    AuthorityRef, ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationValueV1,
    SOURCE_BINDINGS_SETTING_KEY, ScopeSourceBinding, SettingKey, SourceBindingId, SourceKindV1,
};
use tracedecay_domain::{
    ActorId, LocatorDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId, canonical_sha256,
};
use tracedecay_tool_catalog::CapabilityId;

use super::{
    project_open_source_access_at, project_open_work_capabilities, project_open_work_grant,
};

#[test]
fn project_source_access_uses_the_configured_binding_and_refuses_foreign_configuration() {
    let root = tempfile::tempdir().expect("project root");
    let project_id = ProjectId::new("project.source-access").expect("project id");
    let scope = ResolvedScope::new(
        project_id.clone(),
        RepositoryId::new("repository.source-access").expect("repository id"),
        WorktreeId::new("worktree.source-access").expect("worktree id"),
        None,
    )
    .expect("scope");
    let revision =
        ConfigurationRevisionId::new("revision.source-access.1").expect("configuration revision");
    let binding =
        tracedecay_configuration::config::scope_control::daemon_owned_project_source_binding(
            &project_id,
            root.path(),
        )
        .expect("daemon source binding");
    let source_bindings_key =
        SettingKey::new(SOURCE_BINDINGS_SETTING_KEY).expect("source bindings key");
    let snapshot = resolve_configuration(
        &ConfigurationRegistry::core().expect("configuration registry"),
        &[ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::Project {
                project_id: project_id.clone(),
            },
            revision_id: revision.clone(),
            entries: std::collections::BTreeMap::from([(
                source_bindings_key,
                ConfigurationValueV1::SourceBindings(vec![binding.clone()]),
            )]),
        }],
    )
    .expect("configuration resolution")
    .snapshot;
    let configuration = tracedecay_configuration::config::PinnedRuntimeConfiguration::new(
        tracedecay_configuration::config::RuntimeConfigurationTarget {
            project_id: project_id.clone(),
            project_root: root.path().to_path_buf(),
        },
        revision,
        snapshot,
    )
    .expect("pinned configuration");
    let requester = ActorId::new("actor.source-access").expect("requester");
    let capabilities = project_open_work_capabilities().expect("capabilities");
    let access = project_open_source_access_at(
        &scope,
        root.path(),
        &configuration,
        requester.clone(),
        capabilities.clone(),
        UtcMicros(10),
        UtcMicros(100),
    )
    .expect("source access");

    assert_eq!(access.binding, binding);
    assert_eq!(access.requester, requester);
    assert_eq!(access.effective_capabilities, capabilities);

    let foreign_scope = ResolvedScope::new(
        ProjectId::new("project.foreign").expect("foreign project"),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        scope.reference.clone(),
    )
    .expect("foreign scope");
    assert!(
        project_open_source_access_at(
            &foreign_scope,
            root.path(),
            &configuration,
            access.requester,
            access.effective_capabilities,
            UtcMicros(10),
            UtcMicros(100),
        )
        .is_err(),
        "foreign configuration identity must fail closed"
    );
}

#[test]
fn production_project_owner_grants_every_work_operation() {
    let capabilities = project_open_work_capabilities().expect("production capabilities");

    for (_, capability, _) in tracedecay_contracts::WORK_APPLICATION_OPERATION_IDS_V1
        .into_iter()
        .chain(tracedecay_contracts::WORKFLOW_APPLICATION_OPERATION_IDS)
        .chain(tracedecay_contracts::HANDOFF_APPLICATION_OPERATION_IDS_V1)
    {
        let capability = CapabilityId::new(capability).expect("Work attempt capability");
        assert!(
            capabilities.contains(&capability),
            "{} must be granted to the daemon-owned Work route",
            capability.as_str()
        );
    }
}

#[test]
fn work_grant_is_absent_for_partial_access_but_expiry_still_fails_closed() {
    let mut access = access_snapshot();
    access.effective_capabilities.remove(
        &CapabilityId::new("capability.work.generate_proposal")
            .expect("generate proposal capability"),
    );

    assert!(
        project_open_work_grant(&access, UtcMicros(10))
            .expect("partial Work access is valid")
            .is_none(),
        "a denied Work operation must leave the Work owner unmounted"
    );
    assert!(
        project_open_work_grant(&access, access.grant_expires_at).is_err(),
        "an expired project-open authority must still fail closed"
    );
}

#[test]
fn work_grant_identity_tracks_access_authority_not_configuration_state() {
    let access = access_snapshot();
    let original = project_open_work_grant(&access, UtcMicros(10))
        .expect("original Work grant")
        .expect("complete Work access");

    let mut reconfigured = access.clone();
    reconfigured.configuration_revision =
        ConfigurationRevisionId::new("revision.work-grant.2").expect("reconfigured revision");
    reconfigured.configuration_digest =
        canonical_sha256(&"work-grant-configuration-2").expect("reconfigured digest");
    reconfigured.configuration_provenance_digest =
        canonical_sha256(&"work-grant-provenance-2").expect("reconfigured provenance");
    let reconfigured = project_open_work_grant(&reconfigured, UtcMicros(10))
        .expect("reconfigured Work grant")
        .expect("complete Work access");
    assert_eq!(
        reconfigured.digest, original.digest,
        "ordinary configuration changes must not abandon durable Work rows"
    );

    let mut rebound = access.clone();
    rebound.binding = source_binding(&access.scope.project_id, "binding.work-grant.rebound", 'b');
    let rebound = project_open_work_grant(&rebound, UtcMicros(10))
        .expect("rebound Work grant")
        .expect("complete Work access");
    assert_ne!(
        rebound.digest, original.digest,
        "a different admitted source binding must change Work authority"
    );

    let mut expanded = access;
    expanded
        .effective_capabilities
        .insert(CapabilityId::new("capability.test.work-grant-extra").expect("extra capability"));
    let expanded = project_open_work_grant(&expanded, UtcMicros(10))
        .expect("expanded Work grant")
        .expect("complete Work access");
    assert_ne!(
        expanded.digest, original.digest,
        "a different effective capability set must change Work authority"
    );
}

fn access_snapshot() -> ProjectSourceAccessSnapshot {
    let project_id = ProjectId::new("project.work-grant").expect("project id");
    let scope = ResolvedScope::new(
        project_id.clone(),
        RepositoryId::new("repository.work-grant").expect("repository id"),
        WorktreeId::new("worktree.work-grant").expect("worktree id"),
        None,
    )
    .expect("resolved scope");
    ProjectSourceAccessSnapshot {
        scope,
        requester: ActorId::new("actor.work-grant").expect("requester"),
        binding: source_binding(&project_id, "binding.work-grant", 'a'),
        configuration_revision: ConfigurationRevisionId::new("revision.work-grant.1")
            .expect("configuration revision"),
        configuration_digest: canonical_sha256(&"work-grant-configuration-1")
            .expect("configuration digest"),
        configuration_provenance_digest: canonical_sha256(&"work-grant-provenance-1")
            .expect("configuration provenance"),
        effective_capabilities: project_open_work_capabilities().expect("production capabilities"),
        grant_expires_at: UtcMicros(100),
    }
}

fn source_binding(project_id: &ProjectId, binding_id: &str, locator: char) -> ScopeSourceBinding {
    ScopeSourceBinding::new(
        SourceBindingId::new(binding_id).expect("binding id"),
        SourceKindV1::Cursor,
        LocatorDigest::new(format!("sha256:{}", locator.to_string().repeat(64)))
            .expect("locator digest"),
        AuthorityRef::Project(project_id.clone()),
    )
    .expect("source binding")
}

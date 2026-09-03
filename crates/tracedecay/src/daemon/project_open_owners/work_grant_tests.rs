//! Project-open Work grant identity and catalog coverage.

use tracedecay_application::ResolvedScope;
use tracedecay_domain::configuration::{
    AuthorityRef, ConfigurationRevisionId, ScopeSourceBinding, SourceBindingId, SourceKindV1,
};
use tracedecay_domain::{
    ActorId, LocatorDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId, canonical_sha256,
};
use tracedecay_tool_catalog::CapabilityId;
use tracedecay_usecases::source_authorization::ProjectSourceAccessSnapshot;

use super::{production_owner_capabilities, project_open_work_grant};

#[test]
fn production_project_owner_grants_every_work_operation() {
    let capabilities = production_owner_capabilities().expect("production capabilities");

    for (_, capability, _) in tracedecay_application::WORK_APPLICATION_OPERATION_IDS_V1
        .into_iter()
        .chain(tracedecay_application::WORKFLOW_APPLICATION_OPERATION_IDS)
        .chain(tracedecay_application::HANDOFF_APPLICATION_OPERATION_IDS_V1)
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
fn work_grant_identity_tracks_access_authority_not_configuration_state() {
    let access = access_snapshot();
    let original = project_open_work_grant(&access, UtcMicros(10)).expect("original Work grant");

    let mut reconfigured = access.clone();
    reconfigured.configuration_revision =
        ConfigurationRevisionId::new("revision.work-grant.2").expect("reconfigured revision");
    reconfigured.configuration_digest =
        canonical_sha256(&"work-grant-configuration-2").expect("reconfigured digest");
    reconfigured.configuration_provenance_digest =
        canonical_sha256(&"work-grant-provenance-2").expect("reconfigured provenance");
    let reconfigured =
        project_open_work_grant(&reconfigured, UtcMicros(10)).expect("reconfigured Work grant");
    assert_eq!(
        reconfigured.digest, original.digest,
        "ordinary configuration changes must not abandon durable Work rows"
    );

    let mut rebound = access.clone();
    rebound.binding = source_binding(&access.scope.project_id, "binding.work-grant.rebound", 'b');
    let rebound = project_open_work_grant(&rebound, UtcMicros(10)).expect("rebound Work grant");
    assert_ne!(
        rebound.digest, original.digest,
        "a different admitted source binding must change Work authority"
    );

    let mut expanded = access;
    expanded
        .effective_capabilities
        .insert(CapabilityId::new("capability.test.work-grant-extra").expect("extra capability"));
    let expanded = project_open_work_grant(&expanded, UtcMicros(10)).expect("expanded Work grant");
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
        effective_capabilities: production_owner_capabilities().expect("production capabilities"),
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

//! `registrars` module test coverage (split from the former monolithic
//! `invocation::tests` module).

use super::*;

#[test]
fn direct_configuration_grants_reject_foreign_caller_selected_layers() {
    let exact_project = tracedecay_domain::configuration::ConfigurationLayerIdV1::Project {
        project_id: ProjectId::new("project.configuration.exact").expect("project"),
    };
    let exact_profile = tracedecay_domain::configuration::ConfigurationLayerIdV1::UserProfile {
        profile_id: tracedecay_domain::UserProfileId::new("profile.configuration.exact")
            .expect("profile"),
    };
    let exact_collection = tracedecay_domain::configuration::ConfigurationLayerIdV1::Collection {
        collection_id: tracedecay_domain::QueryCollectionId::new("collection.configuration.exact")
            .expect("collection"),
    };
    let authority = DaemonConfigurationGrantAuthority::for_test(
        [
            exact_project.clone(),
            exact_profile.clone(),
            exact_collection.clone(),
        ],
        UtcMicros(100),
    );
    let expected_revision =
        ConfigurationRevisionId::new("configuration.revision.exact").expect("revision");

    for (index, layer) in [exact_project, exact_profile, exact_collection]
        .into_iter()
        .enumerate()
    {
        let mutation = DirectConfigurationMutation::Unset {
            layer,
            key: tracedecay_domain::configuration::SettingKey::new("sync.auto_watch")
                .expect("setting"),
        };
        assert!(
            authority
                .issue_direct(
                    &format!("request.configuration.exact.{index}"),
                    &mutation,
                    expected_revision.clone(),
                    UtcMicros(1),
                )
                .is_ok()
        );
    }

    for (index, layer) in [
        tracedecay_domain::configuration::ConfigurationLayerIdV1::Project {
            project_id: ProjectId::new("project.configuration.foreign").expect("project"),
        },
        tracedecay_domain::configuration::ConfigurationLayerIdV1::UserProfile {
            profile_id: tracedecay_domain::UserProfileId::new("profile.configuration.foreign")
                .expect("profile"),
        },
        tracedecay_domain::configuration::ConfigurationLayerIdV1::Collection {
            collection_id: tracedecay_domain::QueryCollectionId::new(
                "collection.configuration.foreign",
            )
            .expect("collection"),
        },
    ]
    .into_iter()
    .enumerate()
    {
        let foreign = DirectConfigurationMutation::Unset {
            layer,
            key: tracedecay_domain::configuration::SettingKey::new("sync.auto_watch")
                .expect("setting"),
        };
        assert!(matches!(
            authority.issue_direct(
                &format!("request.configuration.foreign.{index}"),
                &foreign,
                expected_revision.clone(),
                UtcMicros(1),
            ),
            Err(DaemonInvocationProblem::NotFoundOrNotAuthorized)
        ));
    }
}

#[test]
fn mounted_configuration_layers_exclude_stale_collection_provenance() {
    use tracedecay_domain::configuration::{
        CandidateDispositionV1, ConfigurationCandidateV1, ConfigurationSnapshotV1,
        ConfigurationValueV1,
    };

    let project_id = ProjectId::new("project.configuration.mounted").expect("project");
    let profile_id =
        tracedecay_domain::UserProfileId::new("profile.configuration.mounted").expect("profile");
    let winning = tracedecay_domain::QueryCollectionId::new("collection.configuration.winning")
        .expect("collection");
    let overridden =
        tracedecay_domain::QueryCollectionId::new("collection.configuration.overridden")
            .expect("collection");
    let rejected = tracedecay_domain::QueryCollectionId::new("collection.configuration.rejected")
        .expect("collection");
    let key =
        tracedecay_domain::configuration::SettingKey::new("sync.auto_watch").expect("setting");
    let revision =
        ConfigurationRevisionId::new("configuration.revision.mounted").expect("revision");
    let candidate = |collection_id, disposition| ConfigurationCandidateV1 {
        layer: ConfigurationLayerIdV1::Collection { collection_id },
        revision_id: revision.clone(),
        disposition,
        safe_reason: None,
    };
    let snapshot = ConfigurationSnapshotV1::new(
        BTreeMap::from([(key.clone(), ConfigurationValueV1::Boolean(true))]),
        BTreeMap::from([(
            key,
            vec![
                candidate(winning.clone(), CandidateDispositionV1::Winning),
                candidate(overridden.clone(), CandidateDispositionV1::Overridden),
                candidate(rejected.clone(), CandidateDispositionV1::Rejected),
            ],
        )]),
    )
    .expect("snapshot");

    let mounted =
        mounted_configuration_layers(&project_id, &profile_id, &snapshot).expect("layers");
    let contains = |layer: ConfigurationLayerIdV1| {
        let digest = configuration_layer_scope_digest(&layer).expect("digest");
        mounted.get(&digest) == Some(&layer)
    };
    assert!(contains(ConfigurationLayerIdV1::Collection {
        collection_id: winning,
    }));
    assert!(!contains(ConfigurationLayerIdV1::Collection {
        collection_id: overridden,
    }));
    assert!(!contains(ConfigurationLayerIdV1::Collection {
        collection_id: rejected,
    }));
}

#[tokio::test]
async fn registered_work_runtime_dispatches_attempt_requests() {
    let _pin = crate::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project root");
    let project_id = ProjectId::new("project.work.invocation").expect("project id");
    let host = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
        crate::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered project runtime");
    let database = host
        .project_observation_database_arc_for_test()
        .expect("registered project database");
    let actor = ActorId::new("actor.work.invocation").expect("actor id");
    let scope = ResolvedScope::new(
        project_id.clone(),
        tracedecay_domain::RepositoryId::new("repository.work.invocation").expect("repository id"),
        tracedecay_domain::WorktreeId::new("worktree.work.invocation").expect("worktree id"),
        None,
    )
    .expect("resolved scope");
    let grant_digest =
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("grant digest");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.work.invocation").expect("grant id"),
        1,
        grant_digest.clone(),
        actor.clone(),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        std::collections::BTreeSet::from([CapabilityId::new(
            "capability.work.attempt_renew_lease",
        )
        .expect("capability")]),
        std::collections::BTreeSet::from([
            UseCaseId::new("use-case.work.attempt_renew_lease").expect("use case")
        ]),
        DisclosureClass::Sensitive,
    )
    .expect("Work grant");
    let authority = WorkAuthority::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        actor.clone(),
        grant_digest,
    )
    .expect("Work authority");
    let policy_digest =
        ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).expect("policy digest");
    let configuration_digest =
        ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).expect("configuration digest");
    let service = DaemonInvocationService::default();
    DaemonWorkRuntimeRegistrar::new(&service)
        .register(
            project.path().to_path_buf(),
            Arc::clone(&database),
            authority.clone(),
            actor.clone(),
            grant.clone(),
            policy_digest.clone(),
            configuration_digest.clone(),
            crate::sessions::codex_app_server::CodexAppServerSummaryConfig {
                codex_bin: "tracedecay-work-provider-not-used".to_owned(),
                model: None,
                timeout: Duration::from_secs(5),
            },
        )
        .await
        .expect("registered Work runtime");
    assert!(
        DaemonWorkRuntimeRegistrar::new(&service)
            .authority_matches(
                project.path(),
                &authority,
                &actor,
                &grant,
                &policy_digest,
                &configuration_digest,
            )
            .await
    );
    let rotated_grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.work.invocation.rotated").expect("grant id"),
        1,
        grant.digest.clone(),
        actor.clone(),
        UtcMicros(2),
        UtcMicros(20_000),
        scope.clone(),
        grant.allowed_capabilities.clone(),
        grant.allowed_use_cases.clone(),
        DisclosureClass::Sensitive,
    )
    .expect("rotated Work grant");
    let rotated_authority = WorkAuthority::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        actor.clone(),
        rotated_grant.digest.clone(),
    )
    .expect("rotated Work authority");
    DaemonWorkRuntimeRegistrar::new(&service)
        .register(
            project.path().to_path_buf(),
            Arc::clone(&database),
            rotated_authority.clone(),
            actor.clone(),
            rotated_grant.clone(),
            policy_digest.clone(),
            configuration_digest.clone(),
            crate::sessions::codex_app_server::CodexAppServerSummaryConfig {
                codex_bin: "tracedecay-work-provider-not-used".to_owned(),
                model: None,
                timeout: Duration::from_secs(5),
            },
        )
        .await
        .expect("rotated Work runtime authority");
    assert!(
        DaemonWorkRuntimeRegistrar::new(&service)
            .authority_matches(
                project.path(),
                &rotated_authority,
                &actor,
                &rotated_grant,
                &policy_digest,
                &configuration_digest,
            )
            .await
    );
    let mismatched_authority = WorkAuthority::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        ActorId::new("actor.work.mismatched").expect("mismatched actor"),
        rotated_grant.digest.clone(),
    )
    .expect("mismatched Work authority");
    assert!(
        DaemonWorkRuntimeRegistrar::new(&service)
            .register(
                project.path().to_path_buf(),
                database,
                mismatched_authority,
                actor.clone(),
                rotated_grant.clone(),
                policy_digest.clone(),
                configuration_digest.clone(),
                crate::sessions::codex_app_server::CodexAppServerSummaryConfig {
                    codex_bin: "tracedecay-work-provider-not-used".to_owned(),
                    model: None,
                    timeout: Duration::from_secs(5),
                },
            )
            .await
            .is_err()
    );
    let identity = tracedecay_domain::WorkAttemptIdentityV1::new(
        tracedecay_domain::TaskId::new("task.work.invocation").expect("task id"),
        tracedecay_domain::RunId::new("run.work.invocation").expect("run id"),
        tracedecay_domain::AttemptId::new("attempt.work.invocation").expect("attempt id"),
    )
    .expect("attempt identity");
    let expected = tracedecay_domain::WorkLeaseFenceV1::new(
        tracedecay_domain::WorkLeaseId::new("lease.work.invocation").expect("lease id"),
        tracedecay_domain::WorkFenceEpochV1::new(1).expect("fence epoch"),
    )
    .expect("expected lease");
    let replacement = tracedecay_domain::WorkLeaseFenceV1::new(
        tracedecay_domain::WorkLeaseId::new("lease.work.invocation").expect("lease id"),
        tracedecay_domain::WorkFenceEpochV1::new(2).expect("fence epoch"),
    )
    .expect("replacement lease");
    let request = DaemonInvocationRequest::work_attempt(
        "request.work.invocation",
        WorkAttemptInvocationV1::RenewLease(
            tracedecay_application::WorkAttemptRenewLeaseRequestV1 {
                identity,
                expected,
                replacement,
            },
        ),
        UtcMicros(1),
        Deadline::new(UtcMicros(2)).expect("deadline"),
        CancellationContext::active("cancel.work.invocation").expect("cancellation"),
    );
    assert_eq!(request.operation(), DaemonInvocationOperation::WorkAttempt);
    let response = service
        .invoke(
            &Arc::new(Mutex::new(LspSessionRegistry::default())),
            Some(project.path()),
            None,
            None,
            request,
        )
        .await;
    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
    ));
}

/// Daemon expiry must stop the provider processes a registered Work runtime
/// owns, not merely drop the registry that owned them.
#[cfg(unix)]
#[tokio::test]
async fn expiring_registries_reaps_running_work_executions() {
    let _pin = crate::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project root");
    let project_id = ProjectId::new("project.work.expire").expect("project id");
    let host = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
        crate::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered project runtime");
    let database = host
        .project_observation_database_arc_for_test()
        .expect("registered project database");
    let actor = ActorId::new("actor.work.expire").expect("actor id");
    let scope = ResolvedScope::new(
        project_id,
        tracedecay_domain::RepositoryId::new("repository.work.expire").expect("repository id"),
        tracedecay_domain::WorktreeId::new("worktree.work.expire").expect("worktree id"),
        None,
    )
    .expect("resolved scope");
    let grant_digest =
        ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).expect("grant digest");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.work.expire").expect("grant id"),
        1,
        grant_digest.clone(),
        actor.clone(),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        std::collections::BTreeSet::from([
            CapabilityId::new("capability.work.expire").expect("capability")
        ]),
        std::collections::BTreeSet::from([
            UseCaseId::new("use-case.work.expire").expect("use case")
        ]),
        DisclosureClass::Sensitive,
    )
    .expect("Work grant");
    let authority = WorkAuthority::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        actor.clone(),
        grant_digest,
    )
    .expect("Work authority");
    let context = RequestContext::new(
        actor.clone(),
        scope,
        grant.clone(),
        RequestId::new("request.work.expire").expect("request id"),
        Deadline::new(UtcMicros(9_000)).expect("deadline"),
        CancellationContext::active("cancel.work.expire").expect("cancellation"),
    )
    .expect("request context");

    let storage = database.work_storage().expect("Work storage");
    let work = tracedecay_application::WorkService::new(storage);
    let task_id = tracedecay_domain::TaskId::new("task.work.expire").expect("task id");
    work.create(
        &context,
        CreateWorkCommand {
            task_id: task_id.clone(),
            title: "Reap the provider on expiry".to_owned(),
            dependencies: std::collections::BTreeSet::new(),
            command_id: tracedecay_domain::WorkCommandId::new("command.work.expire.create")
                .expect("command id"),
            occurred_at: UtcMicros(10),
        },
    )
    .expect("created Work");
    work.accept_proposal(
        &context,
        AcceptProposalCommand {
            review: tracedecay_application::ReviewProposalCommand {
                task_id: task_id.clone(),
                proposal_id: tracedecay_domain::ProposalId::new("proposal.work.expire")
                    .expect("proposal id"),
                proposal_digest: ManifestDigest::new(format!("sha256:{}", "e".repeat(64)))
                    .expect("proposal digest"),
                expected_version: tracedecay_domain::WorkVersion::initial(),
                command_id: tracedecay_domain::WorkCommandId::new("command.work.expire.proposal")
                    .expect("command id"),
                occurred_at: UtcMicros(20),
            },
        },
    )
    .expect("accepted proposal");
    work.admit_execution(
        &context,
        AdmitExecutionCommand {
            task_id: task_id.clone(),
            expected_version: tracedecay_domain::WorkVersion::new(2).expect("version"),
            command_id: tracedecay_domain::WorkCommandId::new("command.work.expire.admit")
                .expect("command id"),
            occurred_at: UtcMicros(30),
        },
    )
    .expect("admitted execution");
    let snapshot = tracedecay_domain::WorkProjectionSnapshotV1::new(
        authority
            .projection_generation_id()
            .expect("projection generation"),
        tracedecay_domain::WorkProjectionSequenceV1::new(3),
        vec![work.load(&context, &task_id).expect("projection")],
        tracedecay_domain::WorkProjectionCoverageV1::complete(1, 1).expect("coverage"),
    )
    .expect("projection snapshot");

    let fixture = project.path().join("codex-work-expire-fixture");
    std::fs::write(
        &fixture,
        "#!/usr/bin/env python3\nimport time\nwhile True:\n    time.sleep(1)\n",
    )
    .expect("fixture");
    let mut permissions = std::fs::metadata(&fixture).expect("metadata").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&fixture, permissions).expect("fixture mode");

    let service = DaemonInvocationService::default();
    let configuration_digest =
        ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).expect("configuration digest");
    DaemonWorkRuntimeRegistrar::new(&service)
        .register(
            project.path().to_path_buf(),
            Arc::clone(&database),
            authority.clone(),
            actor,
            grant,
            ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).expect("policy digest"),
            configuration_digest.clone(),
            crate::sessions::codex_app_server::CodexAppServerSummaryConfig {
                codex_bin: fixture.to_string_lossy().into_owned(),
                model: None,
                timeout: Duration::from_secs(30),
            },
        )
        .await
        .expect("registered Work runtime");
    let registered = service
        .project_runtimes
        .get::<RegisteredWorkRuntime>(project.path())
        .await
        .expect("registered Work runtime handle");

    let identity = tracedecay_domain::WorkAttemptIdentityV1::new(
        task_id,
        tracedecay_domain::RunId::new("run.work.expire").expect("run id"),
        tracedecay_domain::AttemptId::new("attempt.work.expire").expect("attempt id"),
    )
    .expect("attempt identity");
    let lease = tracedecay_domain::WorkLeaseFenceV1::new(
        tracedecay_domain::WorkLeaseId::new("lease.work.expire").expect("lease id"),
        tracedecay_domain::WorkFenceEpochV1::new(1).expect("fence epoch"),
    )
    .expect("lease");
    let projection = snapshot.projections().first().expect("admitted projection");
    let execution = tracedecay_domain::WorkExecutionEnvelopeV1::new(
        identity.clone(),
        tracedecay_domain::WorkAttemptProjectionBindingV1::new(
            snapshot.generation_id().clone(),
            snapshot.sequence(),
            projection.version(),
            projection
                .accepted_proposal()
                .cloned()
                .expect("accepted proposal"),
        )
        .expect("projection binding"),
        tracedecay_domain::WorkflowOperationRef::new("operation.work.attempt_start")
            .expect("operation"),
        tracedecay_domain::WorkExecutionSnapshot::new(
            tracedecay_domain::WorkExecutionSnapshotInput {
                configuration_revision_id:
                    tracedecay_domain::configuration::ConfigurationRevisionId::new(
                        "configuration-revision.work.expire",
                    )
                    .expect("configuration revision"),
                configuration_snapshot_id:
                    tracedecay_domain::configuration::ConfigurationSnapshotId::new(
                        "configuration-snapshot.work.expire",
                    )
                    .expect("configuration snapshot"),
                effective_behavior_digest: configuration_digest,
                resolution_provenance_digest: ManifestDigest::new(format!(
                    "sha256:{}",
                    "d".repeat(64)
                ))
                .expect("provenance digest"),
                route: tracedecay_domain::WorkProviderRouteV1::new(
                    tracedecay_domain::ProviderId::new(
                        crate::daemon::work_runtime::CODEX_PROVIDER_ID,
                    )
                    .expect("provider id"),
                    tracedecay_domain::WorkProviderRouteId::new("route.work.codex-app-server.v1")
                        .expect("route id"),
                )
                .expect("provider route"),
                backend: tracedecay_domain::WorkProviderBackendV1::CodexAppServer,
                protocol: tracedecay_domain::WorkProviderProtocol::CodexAppServerJsonRpc,
                model: "codex-work-expire".to_owned(),
                executable: tracedecay_domain::WorkExecutableReference::new(
                    "executable.codex.app-server".to_owned(),
                    ManifestDigest::new(format!("sha256:{}", "e".repeat(64)))
                        .expect("executable digest"),
                )
                .expect("executable"),
                sandbox: tracedecay_domain::WorkSandboxPolicy::Required,
                approval: tracedecay_domain::WorkApprovalPolicy::Never,
                filesystem: tracedecay_domain::WorkFilesystemPolicy::WorkspaceWrite,
                egress: tracedecay_domain::WorkEgressPolicy::Deny,
                environment_allowlist: std::collections::BTreeSet::new(),
                credential_references: std::collections::BTreeSet::new(),
                limits: tracedecay_domain::WorkExecutionLimits::new(
                    128_000, 8_192, 16_384, 16_384, 65_536, 1,
                )
                .expect("execution limits"),
                deadline: UtcMicros(i64::MAX),
                fallback: tracedecay_domain::WorkFallbackTopology::Disabled,
                topology_policy_digest: ManifestDigest::new(format!("sha256:{}", "f".repeat(64)))
                    .expect("topology digest"),
            },
        )
        .expect("execution snapshot"),
        authority.project_id().clone(),
        authority.repository_id().clone(),
        authority.worktree_id().clone(),
        project.path().to_string_lossy().into_owned(),
        None,
        tracedecay_domain::CommitId::new("0123456789abcdef0123456789abcdef01234567")
            .expect("commit"),
        1,
        tracedecay_domain::WorkEffectStateV1::Observational,
    )
    .expect("execution envelope");
    registered
        .runtime
        .acquire_lease(&snapshot, identity.clone(), execution, lease.clone())
        .await
        .expect("leased attempt");
    registered
        .runtime
        .start(
            &identity,
            &lease,
            tracedecay_domain::WorkRecoveryStateV1::Fresh,
        )
        .await
        .expect("started attempt");
    assert_eq!(
        registered.runtime.in_flight(),
        1,
        "the registered runtime must own the provider execution"
    );

    service.expire_all().await;

    assert_eq!(
        registered.runtime.in_flight(),
        0,
        "daemon expiry must stop and join every provider execution"
    );
    assert!(service.project_runtimes.is_empty().await);
}

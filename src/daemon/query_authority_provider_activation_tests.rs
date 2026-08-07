use super::*;

use crate::application::semantic_runtime::{
    project_committed_semantic_pins, project_semantic_retained_vector_generations,
};

#[tokio::test]
async fn committed_query_routes_install_and_rollback_as_one_revision() {
    let project = TempDir::new().expect("project root");
    git(project.path(), &["init", "-q", "-b", "main"]);
    git(project.path(), &["config", "user.name", "TraceDecay Test"]);
    git(
        project.path(),
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    std::fs::create_dir_all(project.path().join("src")).expect("source directory");
    std::fs::write(project.path().join("src/lib.rs"), "pub fn indexed() {}\n")
        .expect("source file");
    git(project.path(), &["add", "."]);
    git(project.path(), &["commit", "-qm", "fixture"]);

    let project_id = ProjectId::new("project.query-semantic-activation").expect("project id");
    let scope =
        crate::daemon::project_open_owners::resolved_scope_for_project(project.path(), &project_id)
            .expect("resolved scope");
    let store = TempDir::new().expect("store root");
    let registry = crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(project_id, project.path(), store.path().to_path_buf(), None)
        .await
        .expect("mount code index");
    let cursor_store = TempDir::new().expect("cursor store");
    let profile_root = cursor_store.path().join("profile");
    let identity =
        crate::daemon::profile_identity::load_or_create(&profile_root).expect("profile identity");
    let _cursor_scope =
        crate::db::enter_daemon_database_scope(&profile_root, 2, "query-semantic-activation")
            .expect("database scope");
    let session_registry =
        crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
            identity,
        )
        .await
        .expect("session registry");
    let session_db = session_registry
        .profile_sessions()
        .await
        .expect("session database");
    let latest = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(latest) = registry.latest_complete_fresh_for_scope(&scope).await {
                break latest;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial code generation");
    let cursor_keys = Arc::new(
        session_db
            .load_session_cursor_key_provider_result()
            .await
            .expect("cursor keys"),
    );
    let provider = DaemonQueryAuthorityProviderV1::default();
    let semantic = semantic_committed_state(scope.clone());
    let prepared = provider
        .prepare_after_successful_activation(
            scope.clone(),
            semantic.state.clone(),
            Arc::clone(&cursor_keys),
            &latest.generation().manifest().privacy_domain,
        )
        .expect("prepare semantic activation");
    let delayed_semantic = provider
        .prepare_after_successful_activation(
            scope.clone(),
            semantic.state.clone(),
            Arc::clone(&cursor_keys),
            &latest.generation().manifest().privacy_domain,
        )
        .expect("prepare delayed semantic activation");
    let semantic_authority = Arc::new(
            crate::daemon::code_index_scheduler::semantic_query_runtime::SemanticQueryAuthorityV1::from_committed(
                semantic.clone(),
            )
            .expect("prepare semantic route"),
        );
    let standalone_query_authority = Arc::clone(prepared.query_authority());
    let standalone_semantic_authority = Arc::clone(&semantic_authority);
    let semantic_attempt = registry
        .begin_committed_query_activation(
            project.path(),
            &scope,
            semantic.epoch,
            semantic.state.configuration_revision(),
            &semantic.transition_digest,
            &prepare_project_semantic_redundancy_authority(&semantic),
        )
        .await
        .expect("reserve semantic activation");
    registry
        .install_committed_query_authorities(
            project.path(),
            &scope,
            &provider,
            prepared,
            Some(semantic_authority),
            None,
            None,
            prepare_project_semantic_redundancy_authority(&semantic),
            &semantic_attempt,
        )
        .await
        .expect("install semantic activation");
    assert!(
        registry
            .mount_query_authority(project.path(), &scope, standalone_query_authority)
            .await
            .is_err(),
        "standalone query installation cannot reset a committed pair"
    );
    assert!(
        registry
            .mount_semantic_query_authority(project.path(), &scope, standalone_semantic_authority,)
            .await
            .is_err(),
        "standalone semantic installation cannot replace a committed pair"
    );

    assert!(matches!(
        provider.status(Some(&scope)),
        QueryAuthorityProviderStatusV1::Available { profile_id, .. }
            if profile_id.as_str() == "profile.query-baseline"
    ));
    assert!(
        registry.has_query_authority_for_scope(&scope).await,
        "semantic activation must keep the mounted query fallback query authority"
    );
    assert_eq!(
        registry
            .query_authority_installation_for_scope(&scope)
            .await,
        Some((
            true,
            true,
            Some(semantic.state.configuration_revision().clone())
        ))
    );
    let semantic_generation = semantic_pins().vector_generation_id;
    assert_eq!(
        project_committed_semantic_pins(project.path()).map(|pins| pins.vector_generation_id),
        Some(semantic_generation.clone())
    );
    assert_eq!(
        project_semantic_retained_vector_generations(project.path())
            .expect("semantic retention roots")
            .generation_ids(),
        &BTreeSet::from([semantic_generation.clone()])
    );
    assert_eq!(
        project_semantic_redundancy_revision(project.path()),
        Some(semantic.state.configuration_revision().clone()),
        "redundancy roots and authority publish under the installed query revision"
    );

    let rollback = query_rollback_committed_state(&semantic);
    let prepared = provider
        .prepare_after_successful_activation(
            scope.clone(),
            rollback.state.clone(),
            Arc::clone(&cursor_keys),
            &latest.generation().manifest().privacy_domain,
        )
        .expect("prepare query rollback");
    let rollback_core_authority = Arc::clone(prepared.query_authority());
    let rollback_attempt = registry
        .begin_committed_query_activation(
            project.path(),
            &scope,
            rollback.epoch,
            rollback.state.configuration_revision(),
            &rollback.transition_digest,
            &prepare_project_semantic_redundancy_authority(&rollback),
        )
        .await
        .expect("reserve query rollback");
    assert!(
        project_committed_semantic_pins(project.path()).is_none(),
        "reservation must revoke the prior semantic redundancy authority before any install"
    );
    assert_eq!(
        project_semantic_redundancy_revision(project.path()),
        Some(rollback.state.configuration_revision().clone())
    );
    assert_eq!(
        project_semantic_retained_vector_generations(project.path())
            .expect("reserved rollback roots")
            .generation_ids(),
        &BTreeSet::from([semantic_generation.clone()]),
        "reservation publishes the durable desired roots without serving stale redundancy"
    );
    assert!(
        registry
            .begin_committed_query_activation(
                project.path(),
                &scope,
                semantic.epoch,
                semantic.state.configuration_revision(),
                &semantic.transition_digest,
                &prepare_project_semantic_redundancy_authority(&semantic),
            )
            .await
            .is_err(),
        "an older durable epoch cannot reserve after the newer rollback"
    );
    assert!(project_committed_semantic_pins(project.path()).is_none());
    assert_eq!(
        project_semantic_redundancy_revision(project.path()),
        Some(rollback.state.configuration_revision().clone()),
        "stale reservation cannot restore prior redundancy"
    );
    registry
        .install_committed_query_authorities(
            project.path(),
            &scope,
            &provider,
            prepared,
            None,
            None,
            Some(&semantic_pins().vector_generation_id),
            prepare_project_semantic_redundancy_authority(&rollback),
            &rollback_attempt,
        )
        .await
        .expect("install query rollback");
    assert_eq!(
        registry
            .query_authority_installation_for_scope(&scope)
            .await,
        Some((
            true,
            false,
            Some(rollback.state.configuration_revision().clone())
        )),
        "semantic disable replaces both routes in one mounted revision"
    );
    assert_eq!(
        Arc::strong_count(&rollback_core_authority),
        2,
        "the registry retains the exact prepared core authority"
    );
    assert!(
        project_committed_semantic_pins(project.path()).is_none(),
        "semantic disable clears the executable redundancy authority"
    );
    assert_eq!(
        project_semantic_retained_vector_generations(project.path())
            .expect("rollback retention roots")
            .generation_ids(),
        &BTreeSet::from([semantic_generation.clone()]),
        "semantic disable retains the rollback generation root"
    );
    assert_eq!(
        project_semantic_redundancy_revision(project.path()),
        Some(rollback.state.configuration_revision().clone())
    );

    registry
        .clear_failed_query_activation(
            project.path(),
            &scope,
            None,
            prepare_project_semantic_redundancy_authority(&semantic),
            &semantic_attempt,
        )
        .await
        .expect("settle delayed failed observer");
    assert_eq!(
        registry
            .query_authority_installation_for_scope(&scope)
            .await,
        Some((
            true,
            false,
            Some(rollback.state.configuration_revision().clone())
        )),
        "a delayed older failure cannot erase the coherently installed rollback"
    );
    assert!(project_committed_semantic_pins(project.path()).is_none());
    assert_eq!(
        project_semantic_retained_vector_generations(project.path())
            .expect("newer roots after delayed failure")
            .generation_ids(),
        &BTreeSet::from([semantic_generation.clone()])
    );
    registry
        .clear_failed_query_activation(
            project.path(),
            &scope,
            None,
            prepare_project_semantic_redundancy_authority(&rollback),
            &rollback_attempt,
        )
        .await
        .expect("fail closed current rollback observation");
    assert_eq!(
        registry
            .query_authority_installation_for_scope(&scope)
            .await,
        Some((
            true,
            false,
            Some(rollback.state.configuration_revision().clone())
        )),
        "a failed semantic observation retains byte-stable core fallback"
    );
    assert_eq!(
        Arc::strong_count(&rollback_core_authority),
        2,
        "semantic observation failure must retain the same mounted core authority Arc"
    );
    assert!(project_committed_semantic_pins(project.path()).is_none());
    assert_eq!(
        project_semantic_retained_vector_generations(project.path())
            .expect("failed desired roots")
            .generation_ids(),
        &BTreeSet::from([semantic_generation.clone()])
    );
    assert_eq!(
        project_semantic_redundancy_revision(project.path()),
        Some(rollback.state.configuration_revision().clone()),
        "failed desired observation publishes its exact durable roots without an authority"
    );

    let delayed_semantic_authority = Arc::new(
            crate::daemon::code_index_scheduler::semantic_query_runtime::SemanticQueryAuthorityV1::from_committed(
                semantic.clone(),
            )
            .expect("prepare delayed semantic route"),
        );
    assert!(
        registry
            .install_committed_query_authorities(
                project.path(),
                &scope,
                &provider,
                delayed_semantic,
                Some(delayed_semantic_authority),
                None,
                None,
                prepare_project_semantic_redundancy_authority(&semantic),
                &semantic_attempt,
            )
            .await
            .is_err(),
        "an older successful observer cannot cross the newer failed desired fence"
    );
    assert_eq!(
        registry
            .query_authority_installation_for_scope(&scope)
            .await,
        Some((
            true,
            false,
            Some(rollback.state.configuration_revision().clone())
        ))
    );
    assert!(project_committed_semantic_pins(project.path()).is_none());
    assert_eq!(
        project_semantic_retained_vector_generations(project.path())
            .expect("failed desired roots after delayed install")
            .generation_ids(),
        &BTreeSet::from([semantic_generation.clone()])
    );

    let retry_attempt = registry
        .begin_committed_query_activation(
            project.path(),
            &scope,
            rollback.epoch,
            rollback.state.configuration_revision(),
            &rollback.transition_digest,
            &prepare_project_semantic_redundancy_authority(&rollback),
        )
        .await
        .expect("reserve exact rollback retry");
    let retry = provider
        .prepare_after_successful_activation(
            scope.clone(),
            rollback.state.clone(),
            Arc::clone(&cursor_keys),
            &latest.generation().manifest().privacy_domain,
        )
        .expect("prepare exact rollback retry");
    let conflicting_profile = accepted_profile(
        "same-revision-conflict",
        &RetrieverKind::QUERY_FALLBACK_LANES,
    );
    let conflicting_state = RetrievalProfileStateV1::new(
        rollback.state.configuration_revision().clone(),
        conflicting_profile.clone(),
        &RetrievalRuntimeCompatibilityV1 {
            retrieval_ceiling: conflicting_profile.profile().retrieval_budget,
            semantic: None,
            semantic_ceiling: None,
            rerank: None,
            rerank_ceiling: None,
        },
    )
    .expect("same-revision conflicting state");
    assert_eq!(
        provider
            .prepare_after_successful_activation(
                scope.clone(),
                conflicting_state,
                cursor_keys,
                &latest.generation().manifest().privacy_domain,
            )
            .err(),
        Some(QueryAuthorityUpdateErrorV1::ActivationNotCurrent),
        "exact retry requires full scope and state identity"
    );
    registry
        .install_committed_query_authorities(
            project.path(),
            &scope,
            &provider,
            retry,
            None,
            None,
            Some(&semantic_pins().vector_generation_id),
            prepare_project_semantic_redundancy_authority(&rollback),
            &retry_attempt,
        )
        .await
        .expect("install exact rollback retry");
    assert_eq!(
        registry
            .query_authority_installation_for_scope(&scope)
            .await,
        Some((
            true,
            false,
            Some(rollback.state.configuration_revision().clone())
        )),
        "the exact desired revision can reconcile after a failed observation"
    );
    registry.shutdown().await;
}

fn restart_request(profile: &tracedecay_domain::FusionProfile) -> RetrievalRequest {
    RetrievalRequest {
        principal: PrincipalId::new("principal.query-restart").expect("principal"),
        scope: RetrievalScope {
            privacy_domain: PrivacyDomainId::new("privacy.query-restart").expect("privacy domain"),
            root: SingleRootScopeV1 {
                repository: RepositoryId::new("repository.query-restart").expect("repository"),
                worktree: Some(WorktreeId::new("worktree.query-restart").expect("worktree")),
                reference: None,
            },
        },
        temporal_mode: TemporalModeV1::Current,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: FreshnessVectorDigest::new(format!("sha256:{}", "7".repeat(64)))
                .expect("freshness digest"),
            authorization_revision: id("authorization.query-restart"),
            captured_at: UtcMicros(100),
        },
        profile_id: profile.profile_id.clone(),
        budget: profile.retrieval_budget,
    }
}

fn empty_restart_lanes() -> Vec<tracedecay_query::retrieval::fusion::CompositionLaneInput> {
    RetrieverKind::QUERY_FALLBACK_LANES
        .into_iter()
        .map(|lane| {
            tracedecay_query::retrieval::fusion::CompositionLaneInput::new(
                lane,
                RetrieverOutcome::Complete(RetrieverBatch {
                    candidates: Vec::new(),
                    evidence_by_occurrence: BTreeMap::<_, ()>::new(),
                    coverage: tracedecay_domain::retrieval::RetrieverCoverage::default(),
                    continuation: None,
                }),
            )
            .expect("empty lane")
        })
        .collect()
}

#[tokio::test]
async fn project_cursor_authority_resumes_prepared_and_fusion_after_reopen() {
    let directory = TempDir::new().expect("temporary profile");
    let profile_root = directory.path().join("profile");
    let identity =
        crate::daemon::profile_identity::load_or_create(&profile_root).expect("profile identity");
    let project_root = directory.path().join("project");
    std::fs::create_dir_all(&project_root).expect("project root");
    let project_id = ProjectId::new("project.query-restart").expect("project id");
    crate::storage::write_enrollment_marker(
        &project_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .expect("production project enrollment");
    let profile_sessions_path = crate::sessions::user_sessions_db_path(identity.profile_root());
    let _scope_guard =
        crate::db::enter_daemon_database_scope(&profile_root, 1, "query-cursor-restart")
            .expect("daemon database scope");
    let session_registry =
        crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
            identity.clone(),
        )
        .await
        .expect("session registry");
    let database = session_registry
        .project_sessions(project_id.clone(), [project_root.clone()])
        .await
        .expect("project session database");
    assert_ne!(database.db_path(), profile_sessions_path);
    assert!(
        !profile_sessions_path.exists(),
        "profile session shard must remain absent"
    );
    let cursor_keys = Arc::new(
        database
            .load_session_cursor_key_provider_result()
            .await
            .expect("durable cursor key provider"),
    );
    let scope = ResolvedScope::new(
        project_id.clone(),
        id("repository.query-restart"),
        id("worktree.query-restart"),
        None,
    )
    .expect("resolved scope");
    let accepted = accepted_profile("query-restart", &RetrieverKind::QUERY_FALLBACK_LANES);
    let state = RetrievalProfileStateV1::new(
        id::<ConfigurationRevisionId>("configuration.query-restart.1"),
        accepted.clone(),
        &RetrievalRuntimeCompatibilityV1 {
            retrieval_ceiling: accepted.profile().retrieval_budget,
            semantic: None,
            semantic_ceiling: None,
            rerank: None,
            rerank_ceiling: None,
        },
    )
    .expect("initial state");
    let provider = DaemonQueryAuthorityProviderV1::default();
    provider
        .install_evaluated_initial_state(scope.clone(), state.clone(), cursor_keys)
        .expect("install first production authority");
    let authority = crate::daemon::code_index_scheduler::query_runtime::prepare_query_authority(
        &scope,
        &PrivacyDomainId::new("privacy.query-restart").expect("privacy domain"),
        &provider,
    )
    .expect("first production query authority");
    let request = restart_request(accepted.profile());
    let query = EphemeralSanitizedQueryViewV1::sanitize(
        "restart-stable query",
        SanitizerRevision::new("query-sanitizer.query-restart").expect("sanitizer"),
        QueryNormalizationRevision::new("query-normalization.query-restart")
            .expect("normalization"),
    )
    .expect("query view");
    let bindings = tracedecay_query::retrieval::PreparedQueryBindingsV1::new(
        "code_symbol_search",
        scope.scope_digest.clone(),
        CodeGenerationId::new("generation.query-restart").expect("generation"),
        digest('8'),
    )
    .expect("prepared bindings");
    let prepared_cursor = tracedecay_query::retrieval::PreparedQueryV1::prepare(
        Arc::clone(&authority),
        request.clone(),
        None,
    )
    .expect("prepare first page")
    .paginate(
        &bindings,
        vec!["first".to_owned(), "second".to_owned()],
        1,
        UtcMicros(100),
    )
    .expect("issue prepared cursor")
    .next_cursor
    .expect("prepared continuation");
    let composed = authority
        .compose(&request, &query, empty_restart_lanes(), 1, None)
        .expect("compose first fusion page");
    let fusion_cursor = authority
        .continuation_cursor_at(&request, &query, &composed.composition, 0)
        .expect("issue fusion cursor");

    drop(authority);
    drop(provider);
    drop(database);
    drop(session_registry);
    let reopened_registry =
        crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
            identity,
        )
        .await
        .expect("reopened session registry");
    let reopened = reopened_registry
        .project_sessions(project_id.clone(), [project_root])
        .await
        .expect("reopened durable project session database");
    assert!(
        !profile_sessions_path.exists(),
        "reopen must not provision or consult the profile session shard"
    );
    let reopened_keys = Arc::new(
        reopened
            .load_session_cursor_key_provider_result()
            .await
            .expect("reopened durable cursor key provider"),
    );
    let reopened_provider = DaemonQueryAuthorityProviderV1::default();
    reopened_provider
        .install_evaluated_initial_state(scope.clone(), state.clone(), Arc::clone(&reopened_keys))
        .expect("install reopened production authority");
    let reopened_authority =
        crate::daemon::code_index_scheduler::query_runtime::prepare_query_authority(
            &scope,
            &PrivacyDomainId::new("privacy.query-restart").expect("privacy domain"),
            &reopened_provider,
        )
        .expect("reopened production query authority");

    let resumed = tracedecay_query::retrieval::PreparedQueryV1::prepare(
        Arc::clone(&reopened_authority),
        request.clone(),
        Some(&prepared_cursor),
    )
    .expect("authenticate prepared continuation after reopen")
    .paginate(
        &bindings,
        vec!["first".to_owned(), "second".to_owned()],
        1,
        UtcMicros(101),
    )
    .expect("resume prepared continuation after reopen");
    assert_eq!(resumed.items, vec!["second"]);
    reopened_authority
        .compose(
            &request,
            &query,
            empty_restart_lanes(),
            1,
            Some(&fusion_cursor),
        )
        .expect("resume fusion continuation after reopen");

    let foreign_root = directory.path().join("foreign-project");
    std::fs::create_dir_all(&foreign_root).expect("foreign project root");
    let foreign_project_id =
        ProjectId::new("project.query-restart-foreign").expect("foreign project id");
    crate::storage::write_enrollment_marker(
        &foreign_root,
        &crate::storage::EnrollmentMarker {
            project_id: foreign_project_id.as_str().to_owned(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .expect("foreign production project enrollment");
    let foreign_database = reopened_registry
        .project_sessions(foreign_project_id, [foreign_root])
        .await
        .expect("foreign project session database");
    let foreign_keys = Arc::new(
        foreign_database
            .load_session_cursor_key_provider_result()
            .await
            .expect("foreign durable cursor key provider"),
    );
    let mismatched_provider = DaemonQueryAuthorityProviderV1::default();
    mismatched_provider
        .install_evaluated_initial_state(scope.clone(), state, foreign_keys)
        .expect("install mismatched production authority");
    let mismatched_authority =
        crate::daemon::code_index_scheduler::query_runtime::prepare_query_authority(
            &scope,
            &PrivacyDomainId::new("privacy.query-restart").expect("privacy domain"),
            &mismatched_provider,
        )
        .expect("mismatched production query authority");
    assert!(
        tracedecay_query::retrieval::PreparedQueryV1::prepare(
            Arc::clone(&mismatched_authority),
            request.clone(),
            Some(&prepared_cursor),
        )
        .is_err(),
        "a foreign project's durable key must not authenticate the prepared cursor"
    );
    assert!(
        mismatched_authority
            .compose(
                &request,
                &query,
                empty_restart_lanes(),
                1,
                Some(&fusion_cursor),
            )
            .is_err(),
        "a foreign project's durable key must not authenticate the fusion cursor"
    );
}

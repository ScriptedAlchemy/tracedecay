use super::*;
use tracedecay_application::{
    CallableCodeSurfaceMeta, CallableCodeSurfaceRequest, CodeCalleesSurfaceRequest,
    CodeExactOccurrenceSurfaceRequest, CodeFacetSurfaceRequest, CodeNavigationSurfaceRequest,
    CodePhraseSearchSurfaceRequest, CodeTimelineSurfaceRequest, RegisteredRootLocatorV1,
};
use tracedecay_domain::{RepositoryId, WorktreeId};
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

fn lsp_deadline() -> Deadline {
    Deadline::new(UtcMicros(i64::MAX)).expect("LSP deadline")
}

fn lsp_cancellation() -> CancellationContext {
    CancellationContext::active("cancel.lsp.dispatch-test").expect("LSP cancellation")
}

async fn open_authorized_workspace(
    service: &DaemonInvocationService,
    registry: &Arc<Mutex<LspSessionRegistry>>,
    workspace: Option<AuthorizedLspWorkspace>,
    request_id: &str,
    root_uris: &[String],
    owner: &DaemonLspInvocationOwner,
) -> DaemonInvocationResponse {
    service
        .open_lsp_session(
            registry,
            workspace,
            request_id.to_owned(),
            "3.17".to_owned(),
            root_uris.first().cloned(),
            root_uris.to_vec(),
            0,
            Some(owner.clone()),
        )
        .await
}

fn assert_lsp_authorization_refusal(response: DaemonInvocationResponse, expected_request_id: &str) {
    assert_eq!(response.request_id, expected_request_id);
    assert_eq!(
        response.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
    );
}

#[test]
fn only_explicit_protocol_frames_select_the_invocation_route() {
    assert!(parse_daemon_invocation_request(r#"{"jsonrpc":"2.0","method":"ping"}"#).is_none());
    let request = DaemonInvocationRequest::lsp_open(
        "request.1",
        "client.1",
        Some("file:///untrusted".to_owned()),
        Vec::new(),
        lsp_deadline(),
        lsp_cancellation(),
    );
    let encoded = serde_json::to_string(&request).expect("encode request");
    assert!(matches!(
        parse_daemon_invocation_request(&encoded),
        Some(Ok(_))
    ));
}

#[tokio::test]
async fn lsp_gateway_control_terminates_before_owner_lookup() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let now = current_micros();
    let requests = [
        (
            DaemonInvocationRequest::lsp_open(
                "request.lsp.cancelled",
                "client.1",
                None,
                Vec::new(),
                Deadline::new(UtcMicros(now.0.saturating_add(30_000_000))).expect("deadline"),
                CancellationContext::cancelled("cancel.lsp.cancelled", now).expect("cancellation"),
            ),
            ApplicationProblemKind::Cancelled,
        ),
        (
            DaemonInvocationRequest::lsp_open(
                "request.lsp.timed-out",
                "client.1",
                None,
                Vec::new(),
                Deadline::new(now).expect("deadline"),
                CancellationContext::active("cancel.lsp.timed-out").expect("cancellation"),
            ),
            ApplicationProblemKind::TimedOut,
        ),
    ];

    for (request, expected) in requests {
        let response = service
            .invoke(&registry, None, None, None, None, request)
            .await;
        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::ApplicationProblem { problem }
                if problem.kind() == expected
        ));
    }
    assert_eq!(registry.lock().await.active_sessions(), 0);
    assert!(service.lsp_sessions.lock().await.is_empty());
}

#[test]
fn test_results_invocation_retains_the_transport_page() {
    let page = PageRequest::first(17).expect("page");
    let request = DaemonInvocationRequest::primitive(
        "request.test-results.page",
        ApplicationSurfaceOperation::TestResults,
        PrimitiveRequest::RecentTestResults(page.clone()),
        UtcMicros(1),
        Deadline::new(UtcMicros(2)).expect("deadline"),
        CancellationContext::active("cancel.test-results.page").expect("cancellation"),
    );
    let encoded = serde_json::to_string(&request).expect("encode request");
    let decoded = parse_daemon_invocation_request(&encoded)
        .expect("daemon protocol")
        .expect("valid request");
    let DaemonInvocationPayload::PrimitiveTestResults {
        page: decoded_page, ..
    } = decoded.payload
    else {
        panic!("test-results request must retain its typed payload");
    };
    assert_eq!(decoded_page, page);
}

#[test]
fn feedback_invocation_preserves_transport_deadline_and_cancellation() {
    let deadline = Deadline::new(UtcMicros(90)).expect("deadline");
    let cancellation = CancellationContext::cancelled("cancel.feedback.transport", UtcMicros(40))
        .expect("cancellation");
    let request = DaemonInvocationRequest::feedback(
        "request.feedback.transport",
        ApplicationSurfaceOperation::FeedbackList,
        "feedback-handle.transport".to_owned(),
        UtcMicros(30),
        deadline.clone(),
        cancellation.clone(),
    );

    assert!(matches!(
        request.payload,
        DaemonInvocationPayload::FeedbackList {
            observed_at: UtcMicros(30),
            deadline: carried_deadline,
            cancellation: carried_cancellation,
            ..
        } if carried_deadline == deadline && carried_cancellation == cancellation
    ));
}

#[test]
fn callable_code_invocation_preserves_typed_request_and_transport_controls() {
    let deadline = Deadline::new(UtcMicros(90)).expect("deadline");
    let cancellation =
        CancellationContext::cancelled("cancel.callable-code.transport", UtcMicros(40))
            .expect("cancellation");
    let phrase = CodePhraseSearchSurfaceRequest {
        query: "daemon invocation".to_owned(),
        phrases: vec!["daemon invocation".to_owned()],
        field_filters: vec![tracedecay_application::retrieval::CodeLexicalFieldFilter {
            field: tracedecay_application::retrieval::CodeLexicalField::Path,
            include: true,
        }],
        fuzzy_budget: 7,
        scope: tracedecay_application::CodeQueryScope::new(
            tracedecay_domain::CodeGenerationId::new("generation.callable-code")
                .expect("generation"),
            Some("src/daemon".to_owned()),
        )
        .expect("scope"),
        meta: CallableCodeSurfaceMeta {
            projection: tracedecay_application::ResultProjection::Evidence,
            order: tracedecay_application::RetrievalOrder::Relevance,
            cursor: None,
        },
    };
    let page = tracedecay_application::PageRequest::first(16).expect("page");
    let canonical = phrase
        .clone()
        .into_application_request(
            tracedecay_code_index_runtime::code_index_scheduler::queries::callable_query_sanitizer_revision(),
            tracedecay_code_index_runtime::code_index_scheduler::queries::callable_query_normalization_revision(),
            page.clone(),
        )
        .expect("canonical phrase request");
    assert_eq!(
        canonical.query.sanitizer_revision().as_str(),
        "query-sanitizer.daemon.v1"
    );
    assert_eq!(
        canonical.query.normalization_revision().as_str(),
        "query-normalization.daemon.v1"
    );
    assert_eq!(
        canonical.field_filters,
        [tracedecay_application::retrieval::CodeLexicalFieldFilter {
            field: tracedecay_application::retrieval::CodeLexicalField::Path,
            include: true,
        }]
    );
    assert_eq!(canonical.fuzzy_budget, 7);
    let request = CallableCodeSurfaceRequest::PhraseSearch(phrase);
    let invocation = DaemonInvocationRequest::callable_code(
        "request.callable-code.transport",
        ApplicationSurfaceOperation::CodePhraseSearch,
        request,
        page.clone(),
        UtcMicros(30),
        deadline.clone(),
        cancellation.clone(),
    );

    assert_eq!(
        invocation.operation(),
        DaemonInvocationOperation::CodePhraseSearch
    );
    assert!(matches!(
        invocation.payload,
        DaemonInvocationPayload::CallableCode {
            surface_operation: ApplicationSurfaceOperation::CodePhraseSearch,
            request: CallableCodeSurfaceRequest::PhraseSearch(CodePhraseSearchSurfaceRequest {
                query,
                phrases,
                ..
            }),
            page: carried_page,
            observed_at: UtcMicros(30),
            deadline: carried_deadline,
            cancellation: carried_cancellation,
        } if query == "daemon invocation"
            && phrases == ["daemon invocation"]
            && carried_page == page
            && carried_deadline == deadline
            && carried_cancellation == cancellation
    ));
}

#[test]
fn callable_code_validation_accepts_only_matching_operation_request_pairs() {
    let scope = tracedecay_application::CodeQueryScope::new(
        tracedecay_domain::CodeGenerationId::new("generation.callable-code").expect("generation"),
        None,
    )
    .expect("scope");
    let meta = CallableCodeSurfaceMeta {
        projection: tracedecay_application::ResultProjection::Evidence,
        order: tracedecay_application::RetrievalOrder::Relevance,
        cursor: None,
    };
    #[derive(Clone, Copy)]
    enum RequestCase {
        ExactOccurrence,
        PhraseSearch,
        Callees,
        Facets,
        Timeline,
        Declaration,
        Definition,
        TypeDefinition,
        References,
    }
    let navigation = |node_id: &str| CodeNavigationSurfaceRequest {
        node_id: node_id.to_owned(),
        scope: scope.clone(),
        meta: meta.clone(),
    };
    let request = |case| match case {
        RequestCase::ExactOccurrence => {
            CallableCodeSurfaceRequest::ExactOccurrence(CodeExactOccurrenceSurfaceRequest {
                literal: "CallableCode".to_owned(),
                kind: None,
                scope: scope.clone(),
                meta: meta.clone(),
            })
        }
        RequestCase::PhraseSearch => {
            CallableCodeSurfaceRequest::PhraseSearch(CodePhraseSearchSurfaceRequest {
                query: "callable code".to_owned(),
                phrases: vec!["callable code".to_owned()],
                field_filters: Vec::new(),
                fuzzy_budget: 0,
                scope: scope.clone(),
                meta: meta.clone(),
            })
        }
        RequestCase::Callees => CallableCodeSurfaceRequest::Callees(CodeCalleesSurfaceRequest {
            node_id: "node.callable-code".to_owned(),
            maximum_depth: 1,
            resolve_trait_dispatch: false,
            scope: scope.clone(),
            meta: meta.clone(),
        }),
        RequestCase::Facets => CallableCodeSurfaceRequest::Facets(CodeFacetSurfaceRequest {
            dimension: tracedecay_application::retrieval::CodeFacetDimension::Kind,
            scope: scope.clone(),
            meta: meta.clone(),
        }),
        RequestCase::Timeline => CallableCodeSurfaceRequest::Timeline(CodeTimelineSurfaceRequest {
            scope: scope.clone(),
            meta: meta.clone(),
        }),
        RequestCase::Declaration => {
            CallableCodeSurfaceRequest::Declaration(navigation("node.declaration"))
        }
        RequestCase::Definition => {
            CallableCodeSurfaceRequest::Definition(navigation("node.definition"))
        }
        RequestCase::TypeDefinition => {
            CallableCodeSurfaceRequest::TypeDefinition(navigation("node.type-definition"))
        }
        RequestCase::References => {
            CallableCodeSurfaceRequest::References(navigation("node.references"))
        }
    };
    let cases = [
        (
            ApplicationSurfaceOperation::CodeExactOccurrence,
            RequestCase::ExactOccurrence,
        ),
        (
            ApplicationSurfaceOperation::CodePhraseSearch,
            RequestCase::PhraseSearch,
        ),
        (
            ApplicationSurfaceOperation::CodeCallees,
            RequestCase::Callees,
        ),
        (ApplicationSurfaceOperation::CodeFacets, RequestCase::Facets),
        (
            ApplicationSurfaceOperation::CodeTimeline,
            RequestCase::Timeline,
        ),
        (
            ApplicationSurfaceOperation::CodeDeclaration,
            RequestCase::Declaration,
        ),
        (
            ApplicationSurfaceOperation::CodeDefinition,
            RequestCase::Definition,
        ),
        (
            ApplicationSurfaceOperation::CodeTypeDefinition,
            RequestCase::TypeDefinition,
        ),
        (
            ApplicationSurfaceOperation::CodeReferences,
            RequestCase::References,
        ),
    ];
    let page = tracedecay_application::PageRequest::first(16).expect("page");
    let deadline = Deadline::new(UtcMicros(90)).expect("deadline");
    let cancellation =
        CancellationContext::active("cancel.callable-code.matrix").expect("cancellation");

    for (request_index, (_, request_case)) in cases.iter().enumerate() {
        for (operation_index, (operation, _)) in cases.iter().enumerate() {
            let invocation = DaemonInvocationRequest {
                protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
                revision: DAEMON_INVOCATION_REVISION,
                request_id: format!(
                    "request.callable-code.matrix.{request_index}.{operation_index}"
                ),
                delivery_route: None,
                payload: DaemonInvocationPayload::CallableCode {
                    surface_operation: *operation,
                    request: request(*request_case),
                    page: page.clone(),
                    observed_at: UtcMicros(30),
                    deadline: deadline.clone(),
                    cancellation: cancellation.clone(),
                },
            };

            if request_index == operation_index {
                assert!(
                    invocation.validate().is_ok(),
                    "matching callable-code pair {request_index} must validate"
                );
            } else {
                assert!(
                    matches!(
                        invocation.validate(),
                        Err(DaemonInvocationProblem::InvalidRequest)
                    ),
                    "cross-pair operation {operation_index} and request {request_index} \
                         must retain InvalidRequest semantics"
                );
            }
        }
    }
}

#[tokio::test]
async fn lsp_session_rejects_a_client_root_that_differs_from_the_admitted_root() {
    let service = DaemonInvocationService::default();
    let project_root = PathBuf::from("/authoritative");
    DaemonLspOwnerRegistrar::new(&service)
        .register_factory_for_project(
            project_root.clone(),
            UserProfileId::new("profile.test.lsp").expect("test LSP profile"),
            ProjectId::new("project.test.lsp").expect("test LSP project"),
            unavailable_lsp_session_factory(),
        )
        .await
        .unwrap();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let response = service
        .invoke(
            &registry,
            Some(&project_root),
            Some(AuthorizedLspWorkspace::single(AdmittedRoot::new(
                "file:///authoritative",
            ))),
            None,
            None,
            DaemonInvocationRequest::lsp_open(
                "request.1",
                "client.1",
                Some("file:///untrusted".to_owned()),
                Vec::new(),
                lsp_deadline(),
                lsp_cancellation(),
            ),
        )
        .await;
    let DaemonInvocationOutcome::LspOpened { session, .. } = response.outcome else {
        panic!("expected an admitted LSP session");
    };

    let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///untrusted","capabilities":{}}}"#;
    let response = service
        .invoke(
            &registry,
            None,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_frame(
                "request.2",
                session.clone(),
                initialize,
                lsp_deadline(),
                lsp_cancellation(),
            ),
        )
        .await;
    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::LspFrameAccepted { .. }
    ));

    let response = service
        .invoke(
            &registry,
            None,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_poll(
                "request.3",
                session.clone(),
                lsp_deadline(),
                lsp_cancellation(),
            ),
        )
        .await;
    let DaemonInvocationOutcome::LspFrame {
        frame: Some(frame), ..
    } = response.outcome
    else {
        panic!("expected initialize response");
    };
    let response: serde_json::Value =
        serde_json::from_str(&frame).expect("initialize error must be JSON-RPC");
    assert_eq!(response["error"]["code"], -32602);
    assert_eq!(
        response["error"]["data"]["detail"],
        "workspace roots differ from the daemon-admitted set"
    );

    let response = service
        .invoke(
            &registry,
            None,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_acknowledge(
                "request.4",
                session.clone(),
                lsp_deadline(),
                lsp_cancellation(),
            ),
        )
        .await;
    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::LspAcknowledged { acknowledged: true }
    ));

    let initialize = r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"rootUri":"file:///authoritative","capabilities":{"general":{"positionEncodings":["utf-16"]}}}}"#;
    let response = service
        .invoke(
            &registry,
            None,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_frame(
                "request.5",
                session.clone(),
                initialize,
                lsp_deadline(),
                lsp_cancellation(),
            ),
        )
        .await;
    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::LspFrameAccepted {
            backpressured: false,
            closed: false
        }
    ));

    let response = service
        .invoke(
            &registry,
            None,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_poll(
                "request.6",
                session,
                lsp_deadline(),
                lsp_cancellation(),
            ),
        )
        .await;
    let DaemonInvocationOutcome::LspFrame {
        frame: Some(frame), ..
    } = response.outcome
    else {
        panic!("expected initialize success response");
    };
    let response: serde_json::Value =
        serde_json::from_str(&frame).expect("initialize success must be JSON-RPC");
    assert_eq!(response["id"], 2);
    assert!(response["result"]["capabilities"].is_object());
}

#[tokio::test]
async fn multi_root_payloads_are_not_served_by_the_per_project_service() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let project_root = PathBuf::from("/quarantined-multi-root");
    service
        .project_runtimes
        .publish(
            project_root.clone(),
            Arc::new(1_u32) as Arc<dyn Any + Send + Sync>,
        )
        .await
        .expect("admit project so the front door reaches per-project dispatch");
    let scope_set_id = ScopeSetId::new("scope-set.quarantined").expect("scope set");
    let observed_at = UtcMicros(1);
    let deadline = Deadline::new(UtcMicros(2)).expect("deadline");
    let cancellation =
        CancellationContext::active("cancel.multi-root.quarantined").expect("cancellation");
    let mut requests = vec![
        DaemonInvocationRequest::multi_root_scope_set_read(
            "request.multi-root.read",
            MultiRootScopeSetReadRequestV1::new(scope_set_id.clone()).expect("read request"),
            observed_at,
            deadline.clone(),
            cancellation.clone(),
        ),
        DaemonInvocationRequest::multi_root_scope_set_compare_and_swap(
            "request.multi-root.cas",
            MultiRootScopeSetCasRequestV1::new(
                scope_set_id.clone(),
                None,
                vec![
                    tracedecay_application::RegisteredRootSelectorV1::new(
                        ProjectId::new("project.quarantined").expect("project"),
                        project_root.clone(),
                    )
                    .expect("registered root selector"),
                ],
            )
            .expect("CAS request"),
            observed_at,
            deadline.clone(),
            cancellation.clone(),
        ),
    ];
    let revision = ScopeSetRevision::new(1).expect("revision");
    let digest = ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("scope digest");
    for (index, operation) in [
        tracedecay_application::MultiRootOperationV1::Work {
            request: serde_json::json!({}),
        },
        tracedecay_application::MultiRootOperationV1::Git {
            request: serde_json::json!({}),
        },
        tracedecay_application::MultiRootOperationV1::Feedback {
            request: serde_json::json!({}),
        },
        tracedecay_application::MultiRootOperationV1::Impact {
            request: serde_json::json!({}),
        },
        tracedecay_application::MultiRootOperationV1::Query {
            request: serde_json::json!({}),
        },
    ]
    .into_iter()
    .enumerate()
    {
        requests.push(DaemonInvocationRequest::multi_root_execute(
            format!("request.multi-root.execute-{index}"),
            MultiRootExecuteRequestV1::new(
                scope_set_id.clone(),
                revision,
                digest.clone(),
                operation,
                0,
                None,
            )
            .expect("execute request"),
            observed_at,
            deadline.clone(),
            cancellation.clone(),
        ));
    }

    // `invoke_for_project` owns every multi-root payload and routes it to the
    // multi-root executor before this service is consulted. Reaching the
    // per-project dispatch means the request was mis-routed.
    for request in requests {
        let response = service
            .invoke(&registry, Some(&project_root), None, None, None, request)
            .await;
        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::Problem {
                problem: DaemonInvocationProblem::InvalidRequest
            }
        ));
    }
    assert_eq!(registry.lock().await.active_sessions(), 0);
    assert!(service.lsp_sessions.lock().await.is_empty());
    assert!(service.authorized_lsp_workspaces.lock().await.is_empty());
}

#[tokio::test]
async fn federated_lsp_admission_preserves_exact_profile_factory_and_root_pairing() {
    let service = DaemonInvocationService::default();
    let home = tempfile::tempdir().expect("workspace roots");
    let profile = UserProfileId::new("profile.workspace").expect("profile");
    let registrar = DaemonLspOwnerRegistrar::new(&service);
    let mut roots = Vec::new();
    let mut owners = Vec::new();
    let mut grants = Vec::new();
    for suffix in ["a", "b"] {
        let root = home.path().join(suffix);
        std::fs::create_dir(&root).expect("workspace root");
        let root = root.canonicalize().expect("canonical workspace root");
        let scope = ResolvedScope::new(
            ProjectId::new(format!("project.workspace-{suffix}")).expect("project"),
            RepositoryId::new(format!("repository.workspace-{suffix}")).expect("repository"),
            WorktreeId::new(format!("worktree.workspace-{suffix}")).expect("worktree"),
            None,
        )
        .expect("scope");
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new(format!("grant.workspace-{suffix}")).expect("grant"),
            1,
            canonical_sha256(&("workspace grant", suffix)).expect("grant digest"),
            ActorId::new("actor.workspace").expect("actor"),
            UtcMicros(1),
            UtcMicros(i64::MAX),
            scope.clone(),
            std::collections::BTreeSet::from([
                CapabilityId::new(LSP_WORKSPACE_CAPABILITY_ID_V1).expect("capability")
            ]),
            std::collections::BTreeSet::from([
                UseCaseId::new(LSP_WORKSPACE_USE_CASE_ID_V1).expect("use case")
            ]),
            DisclosureClass::Sensitive,
        )
        .expect("grant");
        let owner = DaemonLspInvocationOwner::for_test_project(
            unavailable_lsp_session_factory(),
            profile.clone(),
            scope.project_id.clone(),
            root.clone(),
        )
        .with_scope_grant(grant.clone());
        registrar
            .register_lsp_owner(root.clone(), owner.clone())
            .await
            .expect("register owner");
        let locator = RegisteredRootLocatorV1::new(
            scope.project_id.clone(),
            profile.clone(),
            "store.workspace",
            root.clone(),
        )
        .expect("registered root");
        let uri = url::Url::from_directory_path(&root)
            .expect("root URI")
            .to_string();
        roots.push((root, uri, scope, locator));
        owners.push(owner);
        grants.push(grant);
    }
    assert!(roots[0].2.project_id < roots[1].2.project_id);
    assert!(
        roots[0].2.scope_digest > roots[1].2.scope_digest,
        "the fixture must exercise distinct application and LSP canonical orders"
    );

    let workspace = service
        .authorize_lsp_workspace(roots.clone(), UtcMicros(1))
        .await
        .expect("authorize registered workspace");
    let root_uris = roots
        .iter()
        .map(|(_, uri, _, _)| uri.clone())
        .collect::<Vec<_>>();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let opened = open_authorized_workspace(
        &service,
        &registry,
        Some(workspace.clone()),
        "request.workspace-ordered",
        &root_uris,
        &owners[0],
    )
    .await;
    let DaemonInvocationOutcome::LspOpened {
        scope_set_id: Some(_),
        scope_set_digest: Some(opened_digest),
        ..
    } = opened.outcome
    else {
        panic!("federated workspace was not admitted");
    };
    assert_eq!(Some(&opened_digest), workspace.scope_set_digest());

    let duplicate_root = roots[0].clone();
    let duplicate_workspace = service
        .authorize_lsp_workspace(vec![duplicate_root.clone(), duplicate_root], UtcMicros(1))
        .await;
    assert_lsp_authorization_refusal(
        open_authorized_workspace(
            &service,
            &registry,
            duplicate_workspace,
            "request.workspace-duplicate-root",
            &root_uris,
            &owners[0],
        )
        .await,
        "request.workspace-duplicate-root",
    );

    let missing_root_workspace = AuthorizedLspWorkspace::new(
        workspace.scope_set_digest().cloned(),
        vec![workspace.roots()[0].clone()],
    )
    .expect("one-root candidate");
    assert_lsp_authorization_refusal(
        open_authorized_workspace(
            &service,
            &registry,
            Some(missing_root_workspace),
            "request.workspace-missing-root",
            &root_uris,
            &owners[0],
        )
        .await,
        "request.workspace-missing-root",
    );

    registrar
        .register_lsp_owner(
            roots[1].0.clone(),
            DaemonLspInvocationOwner::for_test_project(
                owners[1].factory(),
                UserProfileId::new("profile.foreign").expect("foreign profile"),
                roots[1].2.project_id.clone(),
                roots[1].0.clone(),
            )
            .with_scope_grant(grants[1].clone()),
        )
        .await
        .expect("replace owner with foreign profile");
    assert_lsp_authorization_refusal(
        open_authorized_workspace(
            &service,
            &registry,
            Some(workspace.clone()),
            "request.workspace-foreign-profile",
            &root_uris,
            &owners[0],
        )
        .await,
        "request.workspace-foreign-profile",
    );

    registrar
        .register_lsp_owner(roots[1].0.clone(), owners[1].clone())
        .await
        .expect("restore exact owner");
    let substituted_factory = owners[0].factory();
    assert!(!Arc::ptr_eq(&substituted_factory, &owners[1].factory()));
    registrar
        .register_lsp_owner(
            roots[1].0.clone(),
            DaemonLspInvocationOwner::for_test_project(
                substituted_factory,
                profile,
                roots[1].2.project_id.clone(),
                roots[1].0.clone(),
            )
            .with_scope_grant(grants[1].clone()),
        )
        .await
        .expect("replace owner with another root's factory");
    assert_lsp_authorization_refusal(
        open_authorized_workspace(
            &service,
            &registry,
            Some(workspace),
            "request.workspace-substituted-factory",
            &root_uris,
            &owners[0],
        )
        .await,
        "request.workspace-substituted-factory",
    );

    assert_eq!(registry.lock().await.active_sessions(), 1);
    assert_eq!(service.lsp_sessions.lock().await.len(), 1);
}

#[tokio::test]
async fn lsp_session_admission_accepts_the_lsp_protocol_revision() {
    let service = DaemonInvocationService::default();
    let project_root = PathBuf::from("/authoritative");
    DaemonLspOwnerRegistrar::new(&service)
        .register_factory_for_project(
            project_root.clone(),
            UserProfileId::new("profile.test.lsp").expect("test LSP profile"),
            ProjectId::new("project.test.lsp").expect("test LSP project"),
            unavailable_lsp_session_factory(),
        )
        .await
        .unwrap();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));

    let response = service
        .invoke(
            &registry,
            Some(&project_root),
            Some(AuthorizedLspWorkspace::single(AdmittedRoot::new(
                "file:///authoritative",
            ))),
            None,
            None,
            DaemonInvocationRequest::lsp_open(
                "request.revision",
                "3.17",
                None,
                Vec::new(),
                lsp_deadline(),
                lsp_cancellation(),
            ),
        )
        .await;

    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::LspOpened { .. }
    ));
    assert_eq!(registry.lock().await.active_sessions(), 1);
    assert_eq!(service.lsp_sessions.lock().await.len(), 1);
}

#[tokio::test]
async fn lsp_disconnect_reconnect_and_final_detach_have_distinct_lifecycles() {
    let service = DaemonInvocationService::default();
    let project_root = PathBuf::from("/authoritative");
    DaemonLspOwnerRegistrar::new(&service)
        .register_factory_for_project(
            project_root.clone(),
            UserProfileId::new("profile.test.lsp").expect("test LSP profile"),
            ProjectId::new("project.test.lsp").expect("test LSP project"),
            unavailable_lsp_session_factory(),
        )
        .await
        .unwrap();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::new(1)));
    let open = |request_id: &'static str| {
        service.invoke(
            &registry,
            Some(&project_root),
            Some(AuthorizedLspWorkspace::single(AdmittedRoot::new(
                "file:///authoritative",
            ))),
            None,
            None,
            DaemonInvocationRequest::lsp_open(
                request_id,
                env!("CARGO_PKG_VERSION"),
                None,
                Vec::new(),
                lsp_deadline(),
                lsp_cancellation(),
            ),
        )
    };

    let DaemonInvocationOutcome::LspOpened { session, .. } = open("request.open.1").await.outcome
    else {
        panic!("expected first session");
    };
    let detached = service
        .invoke(
            &registry,
            None,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_detach(
                "request.detach",
                session,
                lsp_deadline(),
                lsp_cancellation(),
            ),
        )
        .await;
    assert!(matches!(
        detached.outcome,
        DaemonInvocationOutcome::LspDetached
    ));
    assert_eq!(registry.lock().await.active_sessions(), 0);
    assert!(service.lsp_sessions.lock().await.is_empty());

    let DaemonInvocationOutcome::LspOpened { session, .. } = open("request.open.2").await.outcome
    else {
        panic!("released capacity must admit a replacement");
    };
    service
        .disconnect_lsp_session(&registry, session.clone())
        .await;
    assert_eq!(registry.lock().await.active_sessions(), 1);
    assert_eq!(service.lsp_sessions.lock().await.len(), 1);

    let reconnected = service
        .invoke(
            &registry,
            None,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_reconnect(
                "request.reconnect",
                session.clone(),
                lsp_deadline(),
                lsp_cancellation(),
            ),
        )
        .await;
    let DaemonInvocationOutcome::LspReconnected {
        session: reconnected_session,
    } = reconnected.outcome
    else {
        panic!("expected reconnect");
    };
    let takeover = service
        .invoke(
            &registry,
            None,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_reconnect(
                "request.reconnect-race",
                reconnected_session.clone(),
                lsp_deadline(),
                lsp_cancellation(),
            ),
        )
        .await;
    let DaemonInvocationOutcome::LspReconnected {
        session: current_session,
    } = takeover.outcome
    else {
        panic!("expected active transport takeover");
    };
    service
        .disconnect_lsp_session(&registry, reconnected_session)
        .await;
    assert_eq!(
        registry.lock().await.active_sessions(),
        1,
        "a stale credential must not disconnect the current endpoint"
    );
    assert_eq!(
        service.lsp_sessions.lock().await.len(),
        1,
        "a stale credential must not remove the current runtime"
    );
    let stale_transport = service
        .invoke(
            &registry,
            None,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_poll(
                "request.stale",
                session,
                lsp_deadline(),
                lsp_cancellation(),
            ),
        )
        .await;
    assert!(matches!(
        stale_transport.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::NotFoundOrNotAuthorized
        }
    ));
    assert_eq!(service.lsp_sessions.lock().await.len(), 1);

    let detached = service
        .invoke(
            &registry,
            None,
            None,
            None,
            None,
            DaemonInvocationRequest::lsp_detach(
                "request.detach.2",
                current_session,
                lsp_deadline(),
                lsp_cancellation(),
            ),
        )
        .await;
    assert!(matches!(
        detached.outcome,
        DaemonInvocationOutcome::LspDetached
    ));
    assert_eq!(registry.lock().await.active_sessions(), 0);
    assert!(service.lsp_sessions.lock().await.is_empty());
}

#[tokio::test]
async fn feedback_handles_fail_closed_without_an_owner() {
    let service = DaemonInvocationService::default();
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let project_root = PathBuf::from("/feedback-without-owner");
    service
        .project_runtimes
        .publish(
            project_root.clone(),
            Arc::new(1_u32) as Arc<dyn Any + Send + Sync>,
        )
        .await
        .expect("admit project without a feedback owner");
    let response = service
        .invoke(
            &registry,
            Some(&project_root),
            Some(AuthorizedLspWorkspace::single(AdmittedRoot::new(
                "file:///authoritative",
            ))),
            None,
            None,
            DaemonInvocationRequest {
                protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
                revision: DAEMON_INVOCATION_REVISION,
                request_id: "request.1".to_owned(),
                delivery_route: None,
                payload: DaemonInvocationPayload::FeedbackList {
                    request_handle: "handle.1".to_owned(),
                    observed_at: UtcMicros(1),
                    deadline: Deadline::new(UtcMicros(2)).expect("deadline"),
                    cancellation: CancellationContext::active("cancel.feedback-owner")
                        .expect("cancellation"),
                },
            },
        )
        .await;
    // With no feedback owner registered the read service itself is absent,
    // so the daemon fails closed as an application-level Unavailable problem
    // (not concealment — that only applies once the service exists and a
    // caller names an unknown handle). See `execute_feedback`.
    let DaemonInvocationOutcome::ApplicationProblem { problem } = response.outcome else {
        panic!(
            "absent feedback owner must fail closed as an application problem: {:?}",
            response.outcome
        );
    };
    assert_eq!(problem.kind(), ApplicationProblemKind::Unavailable);
}

#[test]
fn feedback_invocation_retains_trusted_delivery_route() {
    let request = DaemonInvocationRequest::feedback(
        "request.delivery-route",
        ApplicationSurfaceOperation::FeedbackList,
        "handle.delivery-route".to_owned(),
        UtcMicros(1),
        Deadline::new(UtcMicros(2)).expect("deadline"),
        CancellationContext::active("cancel.delivery-route").expect("cancellation"),
    )
    .with_delivery_route(FeedbackDeliveryRouteV1::Mcp);
    assert_eq!(request.delivery_route, Some(FeedbackDeliveryRouteV1::Mcp));
    let encoded = serde_json::to_value(&request).expect("serialize request");
    assert_eq!(encoded["delivery_route"], "mcp");
    assert!(request.validate().is_ok());
}

#[test]
fn feedback_cycle_projections_use_distinct_handle_payloads() {
    for (surface_operation, daemon_operation, wire_operation) in [
        (
            ApplicationSurfaceOperation::FeedbackImpact,
            DaemonInvocationOperation::FeedbackImpact,
            "feedback_impact",
        ),
        (
            ApplicationSurfaceOperation::AffectedTests,
            DaemonInvocationOperation::AffectedTests,
            "affected_tests",
        ),
    ] {
        let request = DaemonInvocationRequest::feedback(
            format!("request.{}", surface_operation.as_str()),
            surface_operation,
            "rh_feedback-cycle.fixture".to_owned(),
            UtcMicros(1),
            Deadline::new(UtcMicros(2)).expect("deadline"),
            CancellationContext::active(format!("cancel.{}", surface_operation.as_str()))
                .expect("cancellation"),
        );

        assert_eq!(request.operation(), daemon_operation);
        assert!(request.validate().is_ok());
        let encoded = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(encoded["operation"], wire_operation);
        assert_eq!(encoded["request_handle"], "rh_feedback-cycle.fixture");
    }
}

#[test]
fn feedback_observation_invocation_accepts_only_content_free_events() {
    let subject =
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("subject digest");
    let request = DaemonInvocationRequest::feedback_observation(
        "request.feedback-observe",
        subject,
        UtcMicros(1),
        FeedbackSourceEventV1::SseLifecycle {
            lifecycle: tracedecay_application::feedback::observations::FeedbackSseLifecycleV1::Gap,
            sequence: Some(1),
            item_count: 0,
            duration_micros: None,
        },
    );
    assert_eq!(
        request.operation(),
        DaemonInvocationOperation::FeedbackObserve
    );
    assert!(request.validate().is_ok());
    let encoded = serde_json::to_string(&request).expect("serialize request");
    assert!(!encoded.contains("source"));
    assert!(!encoded.contains("comment"));
    assert!(!encoded.contains("log"));
}

//! Single-root daemon LSP gateway request boundary.
//!
//! The gateway has one already-admitted root and delegates post-edit work to
//! the feedback-cycle application boundary. It intentionally does not open a
//! store, supervise an analyzer, resolve workspace folders, or implement any
//! host-specific transport.

mod admission;
mod dto;
#[path = "operation_table.rs"]
pub(crate) mod operation_table;
mod runtime;

pub use admission::{
    AdmittedRoot, decode_uri_segment, percent_hex_nibble, strict_file_uri_path,
    strict_file_uri_segments, strict_file_url, valid_raw_uri_path,
};
pub use dto::{
    CallHierarchyItem, DiagnosticTrigger, DocumentSymbol, FeedbackCyclePort, FeedbackCycleRequest,
    FeedbackCycleResponse, GatewayMethod, GatewayResponse, Hover, IncomingCall, LspLocation,
    LspSemanticOperationOutcome, LspSemanticRequest, MethodUnavailable, MethodUnavailableReason,
    OutgoingCall, RenameCandidate, RenameCandidateResult, RenameCandidateUnavailableReason,
    SemanticProviderOutcome, SemanticRequest, SemanticResponse, SignatureHelp, TypeHierarchyItem,
    WorkspaceSymbol, lsp_semantic_request, project_semantic_outcome,
};
pub use runtime::{
    AnalyzerCancellationAdapter, DaemonLspGateway, DaemonLspProviderBundle,
    DaemonLspProviderFactory, DaemonLspRuntimeSession, FeedbackCycleAdapter,
    FeedbackCycleRuntimePort, LspAnalyzerCancellationAuthority, LspRuntimeFailure,
    LspRuntimeFuture, LspRuntimeSpawner, LspRuntimeTask, LspSemanticRequestAuthority,
    MAX_FEEDBACK_CYCLES, MAX_SEMANTIC_OPERATIONS, SemanticProviderAdapter, SemanticProviderPort,
    UnavailableSemanticProvider,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{EffectiveCapabilities, SemanticCapability};
    use crate::context::ContextProjectionPort;
    use crate::diagnostics::{LspPosition, LspRange};
    use crate::provider::AnalyzerCancellationPort;
    use crate::session::LspRequestId;
    use crate::{
        ClientCapabilities, GatewayCapabilities, UpstreamCapabilities, negotiate_capabilities,
    };
    use serde_json::json;
    use std::cell::RefCell;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll};

    #[derive(Default)]
    struct Feedback {
        requests: RefCell<Vec<FeedbackCycleRequest>>,
    }

    impl FeedbackCyclePort for Feedback {
        fn request_feedback_cycle(&self, request: FeedbackCycleRequest) -> FeedbackCycleResponse {
            self.requests.borrow_mut().push(request);
            FeedbackCycleResponse::Accepted
        }
    }

    struct Semantics;

    impl SemanticProviderPort for Semantics {
        fn definition(
            &self,
            _root: &AdmittedRoot,
            document_uri: &str,
            _position: LspPosition,
        ) -> SemanticProviderOutcome<Vec<LspLocation>> {
            SemanticProviderOutcome::Complete(vec![LspLocation {
                uri: document_uri.into(),
                range: zero_range(),
            }])
        }

        fn rename_candidate(
            &self,
            _root: &AdmittedRoot,
            document_uri: &str,
            _position: LspPosition,
        ) -> SemanticProviderOutcome<RenameCandidateResult> {
            SemanticProviderOutcome::Complete(RenameCandidateResult::Available(RenameCandidate {
                document_uri: document_uri.to_owned(),
                range: zero_range(),
                placeholder: "old_name".to_owned(),
            }))
        }
    }

    fn zero_range() -> LspRange {
        LspRange {
            start: LspPosition {
                line: 0,
                character: 0,
            },
            end: LspPosition {
                line: 0,
                character: 0,
            },
        }
    }

    fn capabilities() -> EffectiveCapabilities {
        let client = ClientCapabilities {
            supports_versioned_publish_diagnostics: true,
            publish_diagnostics_related_information: true,
            publish_diagnostics_code_description: true,
            publish_diagnostics_data: true,
            supports_document_diagnostics: true,
            semantic: SemanticCapability::ALL.into_iter().collect(),
            ..ClientCapabilities::default()
        };
        let upstream = UpstreamCapabilities {
            supports_diagnostics: true,
            semantic: SemanticCapability::ALL.into_iter().collect(),
        };
        negotiate_capabilities(&client, &GatewayCapabilities::default(), &upstream)
    }

    fn definition_request(document_uri: &str) -> SemanticRequest {
        SemanticRequest::Definition {
            document_uri: document_uri.to_owned(),
            position: LspPosition {
                line: 0,
                character: 0,
            },
        }
    }

    #[test]
    fn semantic_routes_do_not_fabricate_empty_success() {
        let unavailable = DaemonLspGateway::new(
            AdmittedRoot::new("file:///root"),
            capabilities(),
            Feedback::default(),
            UnavailableSemanticProvider,
        );
        assert!(matches!(
            unavailable.semantic_request(
                &LspRequestId::Number(7),
                &definition_request("file:///root/a.rs"),
            ),
            GatewayResponse::Unavailable(MethodUnavailable {
                reason: MethodUnavailableReason::ProviderUnavailable,
                ..
            })
        ));

        let available = DaemonLspGateway::new(
            AdmittedRoot::new("file:///root"),
            capabilities(),
            Feedback::default(),
            Semantics,
        );
        assert!(matches!(
            available.semantic_request(
                &LspRequestId::Number(7),
                &definition_request("file:///root/a.rs"),
            ),
            GatewayResponse::Value(SemanticResponse::Locations(locations))
                if locations.len() == 1
        ));
        assert!(matches!(
            available.semantic_request(
                &LspRequestId::Number(7),
                &SemanticRequest::RenameCandidate {
                    document_uri: "file:///root/a.rs".to_owned(),
                    position: LspPosition {
                        line: 0,
                        character: 0,
                    },
                },
            ),
            GatewayResponse::Value(SemanticResponse::RenameCandidate(
                RenameCandidateResult::Available(RenameCandidate { placeholder, .. })
            )) if placeholder == "old_name"
        ));
    }

    #[test]
    fn rejects_prefix_confusion_outside_the_admitted_root() {
        let gateway = DaemonLspGateway::new(
            AdmittedRoot::new("file:///root"),
            capabilities(),
            Feedback::default(),
            Semantics,
        );
        assert!(matches!(
            gateway.semantic_request(
                &LspRequestId::Number(7),
                &definition_request("file:///root-other/a.rs"),
            ),
            GatewayResponse::Unavailable(MethodUnavailable {
                reason: MethodUnavailableReason::OutsideAdmittedRoot,
                ..
            })
        ));
    }

    #[test]
    fn admitted_root_rejects_ambiguous_or_escaping_document_uris() {
        let root = AdmittedRoot::new("file:///root");
        assert!(root.contains_document("file:///root/src/lib.rs"));
        assert!(root.contains_document("file:///root/with%20space.rs"));

        for document_uri in [
            "file:///root-sibling/a.rs",
            "file:///root/%2e%2e/escape.rs",
            "file:///root/.%2E/escape.rs",
            "file:///root/%2Fescape.rs",
            "file:///root/%5cescape.rs",
            "file:///root/%00escape.rs",
            "file:///root/a.rs?outside=true",
            "file:///root/a.rs#outside",
            "https:///root/a.rs",
        ] {
            assert!(
                !root.contains_document(document_uri),
                "unexpectedly admitted {document_uri}"
            );
        }
    }

    #[test]
    fn strict_file_uris_preserve_windows_drive_unc_and_path_case() {
        // URL-level rules: UNC hosts are valid URLs on every platform even
        // though only Windows can convert them to a local path, so these
        // assertions target strict_file_url, not the path-producing wrapper.
        let drive = strict_file_url("FILE:///C:/Workspace/Src/Lib.rs").expect("drive URI");
        let unc = strict_file_url("file://Server/Share/Src/Lib.rs").expect("UNC URI");

        assert!(drive.path().contains("/C:/Workspace/Src/Lib.rs"));
        assert_eq!(unc.host_str(), Some("server"));
        assert!(unc.path().contains("/Share/Src/Lib.rs"));
        assert!(strict_file_url("https://server/Share/Src/Lib.rs").is_none());
        assert!(strict_file_url("file:///C:/Workspace/../escape.rs").is_none());
        assert!(strict_file_url(r"file:///C:\Workspace\Src\Lib.rs").is_none());
    }

    #[test]
    fn admitted_root_matches_equivalent_directory_uri() {
        let root = AdmittedRoot::new("file:///root/project");

        assert!(root.matches_root_uri("file:///root/project/"));
        assert!(!root.matches_root_uri("file:///root/project-other/"));
    }

    /// Host aliases (`/var` → `/private/var`) and Windows verbatim vs native
    /// spellings name one directory. Segment equality alone treats them as
    /// two roots and refuses initialize before any capability is negotiated.
    #[cfg(unix)]
    #[test]
    fn admitted_root_matches_host_alias_spelling_and_refuses_siblings() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let real = temp.path().join("private").join("root");
        std::fs::create_dir_all(&real).expect("create real root");
        std::os::unix::fs::symlink(temp.path().join("private"), temp.path().join("var"))
            .expect("host alias");
        let alias = temp.path().join("var").join("root");
        let sibling = temp.path().join("var").join("root-other");
        std::fs::create_dir(&sibling).expect("create sibling root");

        let admitted = url::Url::from_file_path(real.canonicalize().expect("canonical root"))
            .expect("admitted file URI")
            .to_string();
        let candidate = url::Url::from_directory_path(&alias)
            .expect("alias directory URI")
            .to_string();
        let sibling_uri = url::Url::from_directory_path(&sibling)
            .expect("sibling directory URI")
            .to_string();

        let root = AdmittedRoot::new(admitted);
        assert!(
            root.matches_root_uri(&candidate),
            "equivalent host-alias spelling must be the same admitted root"
        );
        assert!(
            !root.matches_root_uri(&sibling_uri),
            "a sibling root must stay outside the admitted identity"
        );
    }

    /// Root admission and document containment must not depend on whether the
    /// running host can convert the URI to one of its own filesystem paths.
    /// `Url::to_file_path` fails for a drive-less path on Windows and for a
    /// UNC host on Unix, so deciding admission through it rejected otherwise
    /// well-formed client roots on one platform and accepted them on another.
    #[test]
    fn root_admission_and_containment_are_host_independent() {
        for (root_uri, document_uri, sibling_uri, depth) in [
            (
                "file:///root",
                "file:///root/src/lib.rs",
                "file:///root-sibling/lib.rs",
                1,
            ),
            (
                "file:///C:/root",
                "file:///C:/root/src/lib.rs",
                "file:///C:/root-sibling/lib.rs",
                2,
            ),
            (
                "file://server/share",
                "file://server/share/src/lib.rs",
                "file://server/share-other/lib.rs",
                1,
            ),
        ] {
            let root = AdmittedRoot::new(root_uri);
            assert!(root.is_valid(), "{root_uri} must be admitted");
            assert!(
                root.matches_root_uri(&format!("{root_uri}/")),
                "{root_uri} must match its trailing-slash form"
            );
            assert!(
                root.contains_document(document_uri),
                "{root_uri} must contain {document_uri}"
            );
            assert!(
                !root.contains_document(sibling_uri),
                "{root_uri} must not contain {sibling_uri}"
            );
            assert_eq!(root.document_root_depth(document_uri), Some(depth));
        }

        // A UNC host is part of the root's identity, never a prefix match.
        let unc = AdmittedRoot::new("file://server/share");
        assert!(!unc.matches_root_uri("file://other/share"));
        assert!(!unc.contains_document("file://other/share/src/lib.rs"));
        assert!(!unc.contains_document("file:///share/src/lib.rs"));
    }

    #[test]
    fn invalid_document_uri_is_rejected_before_semantic_provider_dispatch() {
        let gateway = DaemonLspGateway::new(
            AdmittedRoot::new("file:///root"),
            capabilities(),
            Feedback::default(),
            Semantics,
        );

        for document_uri in [
            "file:///root-sibling/a.rs",
            "file:///root/%2e%2e/root/a.rs",
            "file:///root/%2fa.rs",
            "file:///root/%5ca.rs",
            "file:///root/%00a.rs",
            "file:///root/a.rs?query",
            "file:///root/a.rs#fragment",
            "untitled:///root/a.rs",
        ] {
            assert!(matches!(
                gateway
                    .semantic_request(&LspRequestId::Number(7), &definition_request(document_uri),),
                GatewayResponse::Unavailable(MethodUnavailable {
                    reason: MethodUnavailableReason::OutsideAdmittedRoot,
                    ..
                })
            ));
        }
    }

    #[test]
    fn semantic_broker_preserves_standard_wire_shape_and_typed_failure_detail() {
        let request = SemanticRequest::Definition {
            document_uri: "file:///root/lib.rs".to_owned(),
            position: LspPosition {
                line: 3,
                character: 1,
            },
        };
        let wire = lsp_semantic_request(&request).expect("standard request");
        assert_eq!(wire.method(), "textDocument/definition");
        assert_eq!(wire.params()["textDocument"]["uri"], "file:///root/lib.rs");
        assert_eq!(wire.params()["position"]["line"], 3);

        let projected = project_semantic_outcome(
            &AdmittedRoot::new("file:///root"),
            &request,
            LspSemanticOperationOutcome::Partial {
                value: serde_json::json!(null),
                coverage: "analyzer-start-failed".to_owned(),
                detail: Some(LspSemanticOperationOutcome::ANALYZER_START_FAILED_DETAIL),
            },
        );
        assert_eq!(
            projected,
            SemanticProviderOutcome::Partial {
                value: SemanticResponse::Locations(Vec::new()),
                coverage: "analyzer-start-failed".to_owned(),
                detail: Some("Analyzer failed to start.".to_owned()),
            }
        );
    }

    #[test]
    fn semantic_failure_details_are_distinct_static_and_protocol_safe() {
        let templates = [
            LspSemanticOperationOutcome::ANALYZER_START_FAILED_DETAIL,
            LspSemanticOperationOutcome::ANALYZER_CANCELLED_DETAIL,
            LspSemanticOperationOutcome::ANALYZER_RETIRED_DETAIL,
            LspSemanticOperationOutcome::ANALYZER_TIMEOUT_DETAIL,
            LspSemanticOperationOutcome::ANALYZER_REMOTE_ERROR_DETAIL,
            LspSemanticOperationOutcome::ANALYZER_TRANSPORT_FAILED_DETAIL,
            LspSemanticOperationOutcome::ANALYZER_INVALID_RESPONSE_DETAIL,
            LspSemanticOperationOutcome::GRAPH_READ_FAILED_DETAIL,
        ];
        assert_eq!(
            templates
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            templates.len()
        );
        for detail in templates {
            let serialized = serde_json::to_string(&json!({ "detail": detail })).unwrap();
            for forbidden in [
                "bearer-secret",
                "alice:hunter2",
                "file://",
                "/home/alice",
                r"C:\Users\alice",
                "\n",
            ] {
                assert!(!serialized.contains(forbidden));
            }
            assert!(
                !detail.is_empty()
                    && detail.len() <= 96
                    && detail.is_ascii()
                    && !detail.chars().any(char::is_control)
            );
        }
    }

    struct InlineTask;

    impl LspRuntimeTask for InlineTask {
        fn abort(&self) {}
    }

    struct InlineSpawner;

    impl LspRuntimeSpawner for InlineSpawner {
        fn spawn(&self, mut future: LspRuntimeFuture<()>) -> Box<dyn LspRuntimeTask> {
            // These harness futures must complete synchronously; a wake would
            // indicate that the test spawner is not a valid runtime for them.
            let mut context = Context::from_waker(std::task::Waker::noop());
            assert_eq!(future.as_mut().poll(&mut context), Poll::Ready(()));
            Box::new(InlineTask)
        }
    }

    struct SemanticAuthority {
        cancelled: Arc<AtomicBool>,
    }

    impl LspSemanticRequestAuthority for SemanticAuthority {
        fn start(
            &self,
            _root: AdmittedRoot,
            _request_id: LspRequestId,
            request: LspSemanticRequest,
        ) -> LspRuntimeFuture<LspSemanticOperationOutcome> {
            assert_eq!(request.method(), "textDocument/definition");
            Box::pin(async {
                LspSemanticOperationOutcome::Complete(json!([{
                    "uri": "file:///root/lib.rs",
                    "range": {
                        "start": { "line": 3, "character": 1 },
                        "end": { "line": 3, "character": 4 }
                    }
                }, {
                    "uri": "file:///outside/lib.rs",
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 1 }
                    }
                }]))
            })
        }

        fn cancel_request(&self, _root: &AdmittedRoot, _request_id: &LspRequestId) -> bool {
            self.cancelled.store(true, Ordering::Release);
            true
        }
    }

    #[test]
    fn semantic_broker_polls_and_cancels_by_project_scoped_request() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let adapter = SemanticProviderAdapter::new(
            Arc::new(InlineSpawner),
            Arc::new(SemanticAuthority {
                cancelled: Arc::clone(&cancelled),
            }),
        );
        let root = AdmittedRoot::new("file:///root");
        let request_id = LspRequestId::Number(8);
        let request = SemanticRequest::Definition {
            document_uri: "file:///root/lib.rs".to_owned(),
            position: LspPosition {
                line: 3,
                character: 1,
            },
        };

        assert_eq!(
            SemanticProviderPort::request(&adapter, &root, &request_id, &request),
            SemanticProviderOutcome::Pending
        );
        assert_eq!(
            SemanticProviderPort::request(&adapter, &root, &request_id, &request),
            SemanticProviderOutcome::Partial {
                value: SemanticResponse::Locations(vec![LspLocation {
                    uri: "file:///root/lib.rs".to_owned(),
                    range: LspRange {
                        start: LspPosition {
                            line: 3,
                            character: 1,
                        },
                        end: LspPosition {
                            line: 3,
                            character: 4,
                        },
                    },
                }]),
                coverage: "semantic-result-outside-admitted-root".to_owned(),
                detail: None,
            }
        );

        assert!(adapter.cancel_request(&root, &request_id));
        assert!(cancelled.load(Ordering::Acquire));
    }

    struct Cancellation;

    impl AnalyzerCancellationPort for Cancellation {
        fn cancel_upstream(&self, _root: &AdmittedRoot, _request_id: &LspRequestId) -> bool {
            false
        }
    }

    struct ContextPort;

    impl ContextProjectionPort for ContextPort {
        fn registrations(&self) -> Vec<crate::ContextProjectionRegistration> {
            Vec::new()
        }

        fn snapshot(
            &self,
            _root: &AdmittedRoot,
            _request_id: &LspRequestId,
            _request: &crate::ContextProjectionRequest,
        ) -> crate::ContextProjectionOutcome {
            crate::ContextProjectionOutcome::Unsupported
        }
    }

    #[test]
    fn provider_factory_creates_isolated_session_runtime_state() {
        let factory = DaemonLspProviderFactory::new(
            Feedback::default(),
            Semantics,
            crate::UnavailableDiagnosticSnapshotProvider,
            Cancellation,
            ContextPort,
            GatewayCapabilities::default(),
            UpstreamCapabilities::default(),
        );
        let first = factory.into_session(AdmittedRoot::new("file:///root"));
        let second = DaemonLspProviderFactory::new(
            Feedback::default(),
            Semantics,
            crate::UnavailableDiagnosticSnapshotProvider,
            Cancellation,
            ContextPort,
            GatewayCapabilities::default(),
            UpstreamCapabilities::default(),
        )
        .into_session(AdmittedRoot::new("file:///root"));

        assert!(!std::ptr::eq(first.overlays(), second.overlays()));
        assert_eq!(first.root(), second.root());
        assert_eq!(
            first.lifecycle(),
            crate::SessionLifecycle::AwaitingInitialize
        );
        assert_eq!(
            second.lifecycle(),
            crate::SessionLifecycle::AwaitingInitialize
        );
    }
}

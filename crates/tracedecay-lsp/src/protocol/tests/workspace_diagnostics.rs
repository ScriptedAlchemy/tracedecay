use super::*;

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn initialize_workspace<D>(
    roots: Vec<AdmittedRoot>,
    diagnostics: D,
) -> DaemonLspProtocolSession<Feedback, Semantics, D>
where
    D: DiagnosticSnapshotPort,
{
    let workspace = AuthorizedLspWorkspace::new(Some(digest('c')), roots.clone()).unwrap();
    let gateway_capabilities = GatewayCapabilities {
        supports_workspace_folders: true,
        supports_workspace_diagnostics: true,
        ..GatewayCapabilities::default()
    };
    let upstream = UpstreamCapabilities {
        supports_diagnostics: true,
        semantic: BTreeSet::new(),
    };
    let initial = negotiate_capabilities(
        &ClientCapabilities::default(),
        &gateway_capabilities,
        &upstream,
    );
    let mut session = DaemonLspProtocolSession::from_workspace_ports(
        workspace,
        initial,
        gateway_capabilities,
        upstream,
        Feedback::default(),
        Semantics,
        diagnostics,
    );
    let folders = roots
        .iter()
        .map(|root| json!({ "uri": root.uri(), "name": root.uri() }))
        .collect::<Vec<_>>();
    session.handle_payload(
        &serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "workspaceFolders": folders,
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "textDocument": { "diagnostic": {} },
                    "workspace": { "workspaceFolders": true },
                },
            },
        }))
        .unwrap(),
        0,
    );
    session.drain_outbound();
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        1,
    );
    session
}

#[test]
fn workspace_diagnostics_preserve_ready_roots_when_one_root_fails() {
    let roots = vec![
        AdmittedRoot::authorized("file:///left", digest('a')),
        AdmittedRoot::authorized("file:///failed", digest('b')),
    ];
    let mut session = initialize_workspace(roots, Diagnostics);
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"workspace/diagnostic","params":{"previousResultIds":[]}}"#,
        2,
    );
    let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(response["result"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(response["result"]["tracedecay"]["complete"], false);
    assert_eq!(
        response["result"]["tracedecay"]["rootFailures"][0]["rootUri"],
        "file:///failed"
    );
    assert_eq!(
        response["result"]["tracedecay"]["rootFailures"][0]["failureClass"],
        "indexed-generation-unavailable"
    );
}

#[test]
fn nested_workspace_diagnostics_route_documents_to_the_deepest_root() {
    let mut session = initialize_workspace(
        vec![
            AdmittedRoot::authorized("file:///workspace", digest('a')),
            AdmittedRoot::authorized("file:///workspace/nested", digest('b')),
        ],
        Diagnostics,
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"workspace/diagnostic","params":{"previousResultIds":[]}}"#,
        2,
    );
    let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    let uris = response["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["uri"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        uris,
        BTreeSet::from([
            "file:///workspace/src/lib.rs",
            "file:///workspace/nested/src/lib.rs",
        ])
    );
}

#[derive(Clone)]
struct MutableWorkspaceDiagnostics {
    message: Arc<Mutex<String>>,
    authority: Arc<Mutex<char>>,
}

impl DiagnosticSnapshotPort for MutableWorkspaceDiagnostics {
    fn supports_workspace_diagnostics(&self) -> bool {
        true
    }

    fn workspace_diagnostics(
        &self,
        workspace: &AuthorizedLspWorkspace,
        root: &AdmittedRoot,
        _overlays: &[OverlaySnapshot],
    ) -> WorkspaceDiagnosticSnapshotOutcome {
        let uri = format!("{}/src/lib.rs", root.uri());
        assert_eq!(workspace.resolve_document(&uri), Ok(root));
        WorkspaceDiagnosticSnapshotOutcome::Ready {
            diagnostics: WorkspaceGenerationDiagnostics {
                code_generation_id: "code-generation-7".to_owned(),
                snapshot_digest: digest('d'),
                documents: vec![WorkspaceDocumentDiagnostics {
                    uri: uri.clone(),
                    version: None,
                    content_digest: ContentDigest::of_bytes(b"same-source"),
                    diagnostics: GenerationDiagnostics {
                        generation: 7,
                        authority_digest: digest(*self.authority.lock().unwrap()),
                        upstream: vec![GatewayDiagnostic {
                            uri,
                            range: LspRange {
                                start: LspPosition {
                                    line: 0,
                                    character: 0,
                                },
                                end: LspPosition {
                                    line: 0,
                                    character: 1,
                                },
                            },
                            severity: Some(DiagnosticSeverity::Warning),
                            code: Some("mutable".to_owned()),
                            code_description_uri: None,
                            message: self.message.lock().unwrap().clone(),
                            source: DiagnosticSource::Upstream,
                            related_information: Vec::new(),
                            data: None,
                        }],
                        tracedecay: Vec::new(),
                    },
                }],
            },
            completed_operation_id: None,
        }
    }
}

#[test]
fn previous_result_id_changes_when_merged_diagnostic_contents_change() {
    let message = Arc::new(Mutex::new("first".to_owned()));
    let authority = Arc::new(Mutex::new('e'));
    let diagnostics = MutableWorkspaceDiagnostics {
        message: Arc::clone(&message),
        authority,
    };
    let mut session = initialize_workspace(
        vec![AdmittedRoot::authorized("file:///root", digest('a'))],
        diagnostics,
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"workspace/diagnostic","params":{"previousResultIds":[]}}"#,
        2,
    );
    let first: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    let first_item = &first["result"]["items"][0];
    let first_result_id = first_item["resultId"].as_str().unwrap().to_owned();
    *message.lock().unwrap() = "second".to_owned();

    session.handle_payload(
        &serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "workspace/diagnostic",
            "params": {
                "previousResultIds": [{
                    "uri": first_item["uri"],
                    "value": first_result_id,
                }],
            },
        }))
        .unwrap(),
        3,
    );
    let second: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(second["result"]["items"][0]["kind"], "full");
    assert_ne!(
        second["result"]["items"][0]["resultId"],
        first_item["resultId"]
    );
    assert_eq!(
        second["result"]["items"][0]["items"][0]["message"],
        "second"
    );
}

#[test]
fn previous_result_id_changes_when_diagnostic_authority_changes() {
    let message = Arc::new(Mutex::new("stable".to_owned()));
    let authority = Arc::new(Mutex::new('e'));
    let diagnostics = MutableWorkspaceDiagnostics {
        message,
        authority: Arc::clone(&authority),
    };
    let mut session = initialize_workspace(
        vec![AdmittedRoot::authorized("file:///root", digest('a'))],
        diagnostics,
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"workspace/diagnostic","params":{"previousResultIds":[]}}"#,
        2,
    );
    let first: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    let first_item = &first["result"]["items"][0];
    let first_result_id = first_item["resultId"].as_str().unwrap().to_owned();
    *authority.lock().unwrap() = 'f';

    session.handle_payload(
        &serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "workspace/diagnostic",
            "params": {
                "previousResultIds": [{
                    "uri": first_item["uri"],
                    "value": first_result_id,
                }],
            },
        }))
        .unwrap(),
        3,
    );
    let second: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(second["result"]["items"][0]["kind"], "full");
    assert_ne!(
        second["result"]["items"][0]["resultId"],
        first_item["resultId"]
    );
    assert_eq!(
        second["result"]["items"][0]["items"][0]["message"],
        "stable"
    );
}

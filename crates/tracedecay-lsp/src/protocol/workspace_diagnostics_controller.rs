use super::diagnostics_controller::{
    parse_workspace_previous_results, refresh_pending_failure, validate_workspace_progress_tokens,
    workspace_diagnostic_failure, workspace_result_identity, workspace_root_failure,
    workspace_root_failure_value,
};
use super::{
    AdmittedRoot, AuthorizedLspWorkspace, BTreeMap, BTreeSet, DaemonLspProtocolSession,
    DiagnosticRefreshAdmission, DiagnosticSerializationCapabilities, DiagnosticSnapshotPort,
    DocumentDiagnosticReport, FeedbackCyclePort, GatewayMethod, ManifestDigest,
    MethodUnavailableReason, RpcFailure, SemanticProviderPort, Value, diagnostic_value,
    document_diagnostic_report_value, json,
};
use crate::workspace_diagnostics::{
    MAX_WORKSPACE_DIAGNOSTIC_FANOUT, MAX_WORKSPACE_DIAGNOSTIC_RESULTS,
    WorkspaceDiagnosticSnapshotOutcome, WorkspaceGenerationDiagnostics,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WorkspaceDiagnosticResultIdentity {
    pub(super) result_id: String,
    pub(super) root_scope_digest: ManifestDigest,
    pub(super) code_generation_id: String,
    pub(super) snapshot_digest: ManifestDigest,
    pub(super) content_digest: tracedecay_domain::ContentDigest,
    pub(super) diagnostic_digest: ManifestDigest,
    pub(super) diagnostic_generation: u64,
    pub(super) version: Option<i64>,
}

impl<P, S, D> DaemonLspProtocolSession<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    pub(crate) fn pull_workspace_diagnostics(
        &mut self,
        params: &Value,
    ) -> Result<Value, RpcFailure> {
        if !self
            .lifecycle
            .gateway
            .capabilities()
            .workspace_diagnostics_supported
        {
            return Err(RpcFailure::unavailable(
                GatewayMethod::WorkspaceDiagnostic.as_lsp_method(),
                MethodUnavailableReason::CapabilityNotNegotiated,
            ));
        }
        validate_workspace_progress_tokens(params)?;
        let previous = parse_workspace_previous_results(params)?;
        // Partial root sets are never carried into another request. A later
        // request must re-read every root so a generation change in a root
        // that was previously ready cannot be combined with newly completed
        // siblings.
        self.diagnostics.workspace_snapshots.clear();
        self.diagnostics.workspace_failures.clear();

        let workspace = self.lifecycle.gateway.workspace().clone();
        let roots = workspace.roots().to_vec();
        let mut started = 0_usize;
        let mut pending = false;
        let mut failures = Vec::new();
        for root in &roots {
            if self
                .diagnostics
                .workspace_snapshots
                .contains_key(root.uri())
                || self.diagnostics.workspace_failures.contains_key(root.uri())
            {
                continue;
            }
            let overlays = self.lifecycle.overlays.snapshots_for_root(&workspace, root);
            match self
                .diagnostics
                .provider
                .workspace_diagnostics(&workspace, root, &overlays)
            {
                WorkspaceDiagnosticSnapshotOutcome::Ready { diagnostics, .. } => {
                    if valid_workspace_generation(&workspace, root, &diagnostics) {
                        self.diagnostics
                            .workspace_snapshots
                            .insert(root.uri().to_owned(), diagnostics);
                    } else {
                        self.diagnostics.workspace_failures.insert(
                            root.uri().to_owned(),
                            workspace_root_failure(
                                root,
                                "workspace-diagnostic-snapshot-invalid".to_owned(),
                            ),
                        );
                    }
                }
                WorkspaceDiagnosticSnapshotOutcome::Refreshing(_) => {
                    started += 1;
                    pending = true;
                }
                WorkspaceDiagnosticSnapshotOutcome::Partial { coverage, .. } => {
                    if started >= MAX_WORKSPACE_DIAGNOSTIC_FANOUT {
                        pending = true;
                        continue;
                    }
                    match self
                        .diagnostics
                        .provider
                        .request_workspace_refresh(&workspace, root, &overlays)
                    {
                        DiagnosticRefreshAdmission::Started(_)
                        | DiagnosticRefreshAdmission::AlreadyRunning(_) => {
                            started += 1;
                            pending = true;
                        }
                        DiagnosticRefreshAdmission::Rejected { failure_class } => {
                            self.diagnostics.workspace_failures.insert(
                                root.uri().to_owned(),
                                workspace_root_failure(root, format!("{coverage}:{failure_class}")),
                            );
                        }
                    }
                }
                WorkspaceDiagnosticSnapshotOutcome::Failed { failure_class, .. } => {
                    self.diagnostics.workspace_failures.insert(
                        root.uri().to_owned(),
                        workspace_root_failure(root, failure_class),
                    );
                }
            }
        }
        if pending {
            let ready_roots = self.diagnostics.workspace_snapshots.len();
            self.diagnostics.workspace_snapshots.clear();
            self.diagnostics.workspace_failures.clear();
            return Err(refresh_pending_failure(
                None,
                None,
                Some(format!(
                    "workspace-roots-ready={}/{}",
                    ready_roots,
                    roots.len()
                )),
                None,
            ));
        }
        failures.extend(self.diagnostics.workspace_failures.values().cloned());

        let mut documents = self
            .diagnostics
            .workspace_snapshots
            .iter()
            .flat_map(|(root_uri, snapshot)| {
                snapshot.documents.iter().cloned().map(move |document| {
                    (
                        root_uri.clone(),
                        snapshot.code_generation_id.clone(),
                        snapshot.snapshot_digest.clone(),
                        document,
                    )
                })
            })
            .collect::<Vec<_>>();
        documents.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.3.uri.cmp(&right.3.uri))
        });
        if documents.len() > MAX_WORKSPACE_DIAGNOSTIC_RESULTS {
            self.diagnostics.workspace_snapshots.clear();
            self.diagnostics.workspace_failures.clear();
            return Err(workspace_diagnostic_failure(
                "workspace-diagnostic-result-capacity",
                failures,
            ));
        }
        if documents.is_empty() && !failures.is_empty() {
            self.diagnostics.workspace_snapshots.clear();
            self.diagnostics.workspace_failures.clear();
            return Err(workspace_diagnostic_failure(
                "workspace-diagnostics-unavailable",
                failures,
            ));
        }

        let mut items = Vec::with_capacity(documents.len());
        let mut next_results = BTreeMap::new();
        let mut seen_documents = BTreeSet::new();
        for (root_uri, code_generation_id, snapshot_digest, document) in documents {
            let Some(root) = roots.iter().find(|root| root.uri() == root_uri) else {
                self.diagnostics.workspace_snapshots.clear();
                self.diagnostics.workspace_failures.clear();
                return Err(workspace_diagnostic_failure(
                    "workspace-root-changed",
                    failures,
                ));
            };
            if !seen_documents.insert(document.uri.clone()) {
                self.diagnostics.workspace_snapshots.clear();
                self.diagnostics.workspace_failures.clear();
                return Err(workspace_diagnostic_failure(
                    "workspace-diagnostic-document-duplicate",
                    failures,
                ));
            }
            let serialization =
                DiagnosticSerializationCapabilities::pull(self.lifecycle.gateway.capabilities());
            let merged = self.merge_document_diagnostics(
                &document.uri,
                document.diagnostics.upstream.clone(),
                document.diagnostics.tracedecay.clone(),
            );
            let diagnostic_identity = merged
                .items
                .iter()
                .cloned()
                .map(|diagnostic| {
                    diagnostic_value(
                        diagnostic,
                        DiagnosticSerializationCapabilities::full_identity(),
                    )
                })
                .collect::<Vec<_>>();
            let diagnostic_digest = tracedecay_domain::canonical_sha256(&(
                "tracedecay.lsp.workspace-diagnostic-contents.v1",
                &document.diagnostics.authority_digest,
                diagnostic_identity,
                merged.omitted_count,
                serialization.identity(),
                self.diagnostics.cursor_native_mode,
            ))
            .map_err(|_| {
                workspace_diagnostic_failure(
                    "workspace-diagnostic-content-identity-unavailable",
                    failures.clone(),
                )
            })?;
            let identity = workspace_result_identity(
                root,
                &code_generation_id,
                &snapshot_digest,
                &document,
                &diagnostic_digest,
            )?;
            let unchanged = previous
                .get(&document.uri)
                .is_some_and(|previous| previous == &identity.result_id)
                && self
                    .diagnostics
                    .workspace_results
                    .get(&document.uri)
                    .is_some_and(|retained| retained == &identity);
            let report = if unchanged {
                DocumentDiagnosticReport::Unchanged {
                    result_id: identity.result_id.clone(),
                }
            } else {
                DocumentDiagnosticReport::full(
                    identity.result_id.clone(),
                    self.visible_diagnostics(
                        merged.items,
                        self.lifecycle
                            .gateway
                            .capabilities()
                            .document_diagnostics_data,
                    ),
                )
            };
            let mut item = document_diagnostic_report_value(report, serialization);
            item["uri"] = Value::String(document.uri.clone());
            item["version"] = document.version.map_or(Value::Null, Value::from);
            items.push(item);
            next_results.insert(document.uri, identity);
        }
        let root_failures = failures
            .iter()
            .map(workspace_root_failure_value)
            .collect::<Vec<_>>();
        let value = json!({
            "items": items,
            "tracedecay": {
                "complete": failures.is_empty(),
                "rootFailures": root_failures,
            }
        });
        let serialized = serde_json::to_vec(&value).map_err(|_| {
            workspace_diagnostic_failure("workspace-diagnostic-encoding", failures.clone())
        })?;
        if serialized.len() > crate::MAX_WORKSPACE_DIAGNOSTIC_BYTES {
            self.diagnostics.workspace_snapshots.clear();
            self.diagnostics.workspace_failures.clear();
            return Err(workspace_diagnostic_failure(
                "workspace-diagnostic-byte-capacity",
                failures,
            ));
        }
        self.diagnostics.workspace_snapshots.clear();
        self.diagnostics.workspace_failures.clear();
        self.diagnostics.workspace_results = next_results;
        Ok(value)
    }
}

fn valid_workspace_generation(
    workspace: &AuthorizedLspWorkspace,
    root: &AdmittedRoot,
    diagnostics: &WorkspaceGenerationDiagnostics,
) -> bool {
    root.scope_digest().is_some()
        && !diagnostics.code_generation_id.is_empty()
        && diagnostics.code_generation_id.len() <= 256
        && diagnostics.documents.iter().all(|document| {
            workspace
                .resolve_document(&document.uri)
                .is_ok_and(|owner| owner == root)
        })
}

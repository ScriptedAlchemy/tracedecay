use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tracedecay_daemon_protocol::ProcessLocalRequestSequence;
use tracedecay_domain::{ManifestDigest, canonical_sha256};

use crate::gateway::operation_table::{BoundedOperationTable, OperationAdmission, OperationPoll};
use crate::gateway::{AdmittedRoot, LspRuntimeFuture, LspRuntimeSpawner};
use crate::provider::{DiagnosticRefreshAdmission, DiagnosticRefreshIdentity};
use crate::session::AuthorizedLspWorkspace;
use crate::workspace_diagnostics::{
    CanonicalWorkspaceDiagnosticRefreshRequest, MAX_WORKSPACE_DIAGNOSTIC_FANOUT,
    WorkspaceDiagnosticSnapshotOutcome,
};

use super::{CanonicalDiagnosticSnapshotAuthority, OverlaySnapshot};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct WorkspaceDiagnosticOperationKey {
    root_uri: String,
    scope_digest: Option<ManifestDigest>,
    overlay_set_digest: ManifestDigest,
}

pub(super) struct WorkspaceDiagnosticAdapter {
    runtime: Arc<dyn LspRuntimeSpawner>,
    authority: Arc<dyn CanonicalDiagnosticSnapshotAuthority>,
    next_operation: ProcessLocalRequestSequence,
    operations: BoundedOperationTable<
        WorkspaceDiagnosticOperationKey,
        DiagnosticRefreshIdentity,
        WorkspaceDiagnosticSnapshotOutcome,
    >,
    active_keys: Mutex<BTreeMap<String, WorkspaceDiagnosticOperationKey>>,
}

impl WorkspaceDiagnosticAdapter {
    pub(super) fn new(
        runtime: Arc<dyn LspRuntimeSpawner>,
        authority: Arc<dyn CanonicalDiagnosticSnapshotAuthority>,
    ) -> Self {
        Self {
            runtime,
            authority,
            next_operation: ProcessLocalRequestSequence::starting_at(1),
            operations: BoundedOperationTable::new(MAX_WORKSPACE_DIAGNOSTIC_FANOUT),
            active_keys: Mutex::new(BTreeMap::new()),
        }
    }

    pub(super) fn supports(&self) -> bool {
        self.authority.supports_workspace_diagnostics()
    }

    #[hotpath::measure(
        label = "lsp_workspace_diagnostics_snapshot",
        impl_type = "WorkspaceDiagnosticAdapter"
    )]
    pub(super) fn snapshot(
        &self,
        workspace: &AuthorizedLspWorkspace,
        root: &AdmittedRoot,
        overlays: &[OverlaySnapshot],
    ) -> WorkspaceDiagnosticSnapshotOutcome {
        let Some(key) = workspace_key(workspace, root, overlays) else {
            return WorkspaceDiagnosticSnapshotOutcome::Failed {
                code_generation_id: None,
                failure_class: "workspace-diagnostic-identity-unavailable".to_owned(),
            };
        };
        match self.operations.poll(&key) {
            OperationPoll::Ready {
                metadata: _,
                result,
            } => {
                self.release_key(&key);
                result
            }
            OperationPoll::Pending(identity) => {
                WorkspaceDiagnosticSnapshotOutcome::Refreshing(identity)
            }
            OperationPoll::Dropped(_) => {
                self.release_key(&key);
                WorkspaceDiagnosticSnapshotOutcome::Failed {
                    code_generation_id: None,
                    failure_class: "workspace-diagnostic-operation-dropped".to_owned(),
                }
            }
            OperationPoll::Missing | OperationPoll::Mismatch(_) => {
                WorkspaceDiagnosticSnapshotOutcome::Partial {
                    code_generation_id: None,
                    coverage: "refresh-required".to_owned(),
                }
            }
        }
    }

    pub(super) fn request(
        &self,
        workspace: &AuthorizedLspWorkspace,
        root: &AdmittedRoot,
        overlays: &[OverlaySnapshot],
    ) -> DiagnosticRefreshAdmission {
        if !self.supports() {
            return rejected("workspace-diagnostics-unsupported");
        }
        let Some(key) = workspace_key(workspace, root, overlays) else {
            return rejected("workspace-diagnostic-identity-unavailable");
        };
        self.adopt_key(&key);
        let request = CanonicalWorkspaceDiagnosticRefreshRequest {
            workspace: workspace.clone(),
            root: root.clone(),
            overlays: overlays.to_vec(),
        };
        let authority = Arc::clone(&self.authority);
        let admission: Result<_, tracedecay_daemon_protocol::SequenceExhausted> =
            self.operations.admit_with(key, self.runtime.as_ref(), || {
                let operation_id = self
                    .next_operation
                    .next_string("lsp-workspace-diagnostic-")?;
                let identity = DiagnosticRefreshIdentity {
                    operation_id: operation_id.clone(),
                    source_generation: None,
                    target_generation: None,
                };
                let operation = Box::pin(async move {
                    match authority.refresh_workspace(request).await {
                        Ok(diagnostics) => WorkspaceDiagnosticSnapshotOutcome::Ready {
                            diagnostics,
                            completed_operation_id: Some(operation_id),
                        },
                        Err(error) if diagnostic_refresh_is_partial(error.class()) => {
                            WorkspaceDiagnosticSnapshotOutcome::Partial {
                                code_generation_id: None,
                                coverage: error.class().to_owned(),
                            }
                        }
                        Err(error) => WorkspaceDiagnosticSnapshotOutcome::Failed {
                            code_generation_id: None,
                            failure_class: error.class().to_owned(),
                        },
                    }
                })
                    as LspRuntimeFuture<WorkspaceDiagnosticSnapshotOutcome>;
                Ok((identity, operation))
            });
        match admission {
            Ok(OperationAdmission::Started(identity)) => {
                DiagnosticRefreshAdmission::Started(identity)
            }
            Ok(OperationAdmission::Existing(identity)) => {
                DiagnosticRefreshAdmission::AlreadyRunning(identity)
            }
            Ok(OperationAdmission::Saturated) => rejected("workspace-diagnostic-capacity"),
            Err(_) => rejected("workspace-diagnostic-identity-exhausted"),
        }
    }

    fn active_keys(&self) -> MutexGuard<'_, BTreeMap<String, WorkspaceDiagnosticOperationKey>> {
        self.active_keys
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn adopt_key(&self, key: &WorkspaceDiagnosticOperationKey) {
        let mut active_keys = self.active_keys();
        if let Some(previous) = active_keys
            .get(&key.root_uri)
            .filter(|previous| *previous != key)
            .cloned()
        {
            self.operations.cancel(&previous);
        }
        active_keys.insert(key.root_uri.clone(), key.clone());
    }

    fn release_key(&self, key: &WorkspaceDiagnosticOperationKey) {
        let mut active_keys = self.active_keys();
        if active_keys.get(&key.root_uri) == Some(key) {
            active_keys.remove(&key.root_uri);
        }
    }
}

fn workspace_key(
    workspace: &AuthorizedLspWorkspace,
    root: &AdmittedRoot,
    overlays: &[OverlaySnapshot],
) -> Option<WorkspaceDiagnosticOperationKey> {
    let identities = overlays
        .iter()
        .map(|overlay| {
            (
                overlay.uri.as_str(),
                overlay.version,
                overlay.content_digest.clone(),
            )
        })
        .collect::<Vec<_>>();
    Some(WorkspaceDiagnosticOperationKey {
        root_uri: root.uri().to_owned(),
        scope_digest: root.scope_digest().cloned(),
        overlay_set_digest: canonical_sha256(&(
            workspace.scope_set_digest().map(ManifestDigest::as_str),
            identities,
        ))
        .ok()?,
    })
}

fn rejected(failure_class: &str) -> DiagnosticRefreshAdmission {
    DiagnosticRefreshAdmission::Rejected {
        failure_class: failure_class.to_owned(),
    }
}

pub(super) fn diagnostic_refresh_is_partial(failure_class: &str) -> bool {
    matches!(
        failure_class,
        "diagnostic-broker-refresh-superseded"
            | "document-content-stale"
            | "managed-diagnostic-content-identity-unavailable"
            | "managed-diagnostic-content-stale"
            | "managed-diagnostic-generation-stale"
            | "workspace-code-generation-stale"
            | "workspace-code-generation-warming"
    )
}

#[cfg(test)]
mod tests {
    use std::task::{Context, Poll};
    use std::time::Duration;

    use super::*;
    use crate::gateway::{LspRuntimeFailure, LspRuntimeTask};
    use crate::overlay::CanonicalDiagnosticRefreshRequest;
    use crate::provider::GenerationDiagnostics;
    use crate::workspace_diagnostics::WorkspaceGenerationDiagnostics;

    struct InlineTask;

    impl LspRuntimeTask for InlineTask {
        fn abort(&self) {}
    }

    struct InlineSpawner;

    impl LspRuntimeSpawner for InlineSpawner {
        fn spawn(&self, mut future: LspRuntimeFuture<()>) -> Box<dyn LspRuntimeTask> {
            let mut context = Context::from_waker(std::task::Waker::noop());
            assert_eq!(future.as_mut().poll(&mut context), Poll::Ready(()));
            Box::new(InlineTask)
        }
    }

    struct Authority;

    impl CanonicalDiagnosticSnapshotAuthority for Authority {
        fn refresh(
            &self,
            _request: CanonicalDiagnosticRefreshRequest,
        ) -> LspRuntimeFuture<Result<GenerationDiagnostics, LspRuntimeFailure>> {
            Box::pin(async { Err(LspRuntimeFailure::new("document-refresh-unused")) })
        }

        fn supports_workspace_diagnostics(&self) -> bool {
            true
        }

        fn refresh_workspace(
            &self,
            _request: CanonicalWorkspaceDiagnosticRefreshRequest,
        ) -> LspRuntimeFuture<Result<WorkspaceGenerationDiagnostics, LspRuntimeFailure>> {
            Box::pin(async { Err(LspRuntimeFailure::new("workspace-refresh-finished")) })
        }
    }

    /// A refresh request or completed poll that lands while a peer session
    /// updates the active-key map waits for it: refusing the request as busy
    /// drops a refresh, and skipping the release strands the key.
    #[test]
    fn refresh_and_release_wait_for_a_peer_updating_active_keys() {
        let adapter = Arc::new(WorkspaceDiagnosticAdapter::new(
            Arc::new(InlineSpawner),
            Arc::new(Authority),
        ));
        let root = AdmittedRoot::new("file:///admitted");
        let workspace = AuthorizedLspWorkspace::single(root.clone());

        let peer = adapter.active_keys.lock().unwrap();
        let requesting = std::thread::spawn({
            let adapter = Arc::clone(&adapter);
            let (workspace, root) = (workspace.clone(), root.clone());
            move || adapter.request(&workspace, &root, &[])
        });
        std::thread::sleep(Duration::from_millis(25));
        drop(peer);
        assert!(
            matches!(
                requesting.join().unwrap(),
                DiagnosticRefreshAdmission::Started(_)
            ),
            "a contended refresh must start, not be rejected as busy"
        );
        assert_eq!(adapter.active_keys.lock().unwrap().len(), 1);

        let peer = adapter.active_keys.lock().unwrap();
        let polling = std::thread::spawn({
            let adapter = Arc::clone(&adapter);
            move || adapter.snapshot(&workspace, &root, &[])
        });
        std::thread::sleep(Duration::from_millis(25));
        drop(peer);
        assert_eq!(
            polling.join().unwrap(),
            WorkspaceDiagnosticSnapshotOutcome::Failed {
                code_generation_id: None,
                failure_class: "workspace-refresh-finished".to_owned(),
            }
        );
        assert!(
            adapter.active_keys.lock().unwrap().is_empty(),
            "the finished operation releases its active key"
        );
    }
}

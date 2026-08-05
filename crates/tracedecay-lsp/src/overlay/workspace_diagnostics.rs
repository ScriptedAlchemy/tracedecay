use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tracedecay_domain::{ContentDigest, ManifestDigest, canonical_sha256};

use crate::gateway::operation_table::{BoundedOperationTable, OperationAdmission, OperationPoll};
use crate::gateway::{AdmittedRoot, LspRuntimeFuture, LspRuntimeSpawner};
use crate::provider::{DiagnosticRefreshAdmission, DiagnosticRefreshIdentity};
use crate::request_sequence::ProcessLocalRequestSequence;
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
            OperationPoll::Busy => WorkspaceDiagnosticSnapshotOutcome::Partial {
                code_generation_id: None,
                coverage: "runtime-busy".to_owned(),
            },
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
        if !self.adopt_key(&key) {
            return rejected("runtime-busy");
        }
        let request = CanonicalWorkspaceDiagnosticRefreshRequest {
            workspace: workspace.clone(),
            root: root.clone(),
            overlays: overlays.to_vec(),
        };
        let authority = Arc::clone(&self.authority);
        let admission: Result<_, crate::request_sequence::SequenceExhausted> =
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
            Ok(OperationAdmission::Busy) => rejected("runtime-busy"),
            Ok(OperationAdmission::Saturated) => rejected("workspace-diagnostic-capacity"),
            Err(_) => rejected("workspace-diagnostic-identity-exhausted"),
        }
    }

    fn adopt_key(&self, key: &WorkspaceDiagnosticOperationKey) -> bool {
        let Ok(mut active_keys) = self.active_keys.try_lock() else {
            return false;
        };
        if let Some(previous) = active_keys
            .get(&key.root_uri)
            .filter(|previous| *previous != key)
            .cloned()
        {
            self.operations.cancel(&previous);
        }
        active_keys.insert(key.root_uri.clone(), key.clone());
        true
    }

    fn release_key(&self, key: &WorkspaceDiagnosticOperationKey) {
        if let Ok(mut active_keys) = self.active_keys.try_lock()
            && active_keys.get(&key.root_uri) == Some(key)
        {
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
                ContentDigest::of_bytes(overlay.text.as_bytes()),
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
            | "workspace-code-generation-warming"
    )
}

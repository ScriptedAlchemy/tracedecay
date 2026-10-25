use std::collections::BTreeSet;

use tracedecay_domain::{ContentDigest, ManifestDigest};

use crate::diagnostics::DiagnosticMerge;
use crate::gateway::{AdmittedRoot, FeedbackCyclePort, SemanticProviderPort};
use crate::overlay::{OverlayLifetime, OverlaySnapshot};
use crate::provider::{
    DiagnosticRefreshIdentity, DiagnosticSnapshotOutcome, DiagnosticSnapshotPort,
    DiagnosticSnapshotValidation, GenerationDiagnostics,
};
use crate::rpc::{DiagnosticSerializationCapabilities, diagnostic_value};
use crate::workspace_diagnostics::{
    MAX_WORKSPACE_DIAGNOSTIC_BYTES, MAX_WORKSPACE_DIAGNOSTIC_RESULTS,
};

use super::DaemonLspProtocolSession;
use super::diagnostics_controller::{PublishedDiagnostic, valid_refresh_identity};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DiagnosticDocumentIdentity {
    pub(super) root: AdmittedRoot,
    scope_set_digest: Option<ManifestDigest>,
    version: i64,
    language_id: Option<String>,
    content_digest: Option<ContentDigest>,
    lifetime: Option<OverlayLifetime>,
}

impl DiagnosticDocumentIdentity {
    pub(super) fn new(
        root: &AdmittedRoot,
        scope_set_digest: Option<&ManifestDigest>,
        overlay: Option<&OverlaySnapshot>,
    ) -> Self {
        Self {
            root: root.clone(),
            scope_set_digest: scope_set_digest.cloned(),
            version: overlay.map_or(0, |overlay| overlay.version),
            language_id: overlay.map(|overlay| overlay.language_id.clone()),
            content_digest: overlay.map(|overlay| overlay.content_digest.clone()),
            lifetime: overlay.map(|overlay| overlay.lifetime.clone()),
        }
    }
}

#[derive(Clone)]
pub(super) struct PendingDiagnosticRefresh {
    pub(super) identity: DiagnosticRefreshIdentity,
    pub(super) document: DiagnosticDocumentIdentity,
    pub(super) state: DiagnosticRefreshState,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum DiagnosticRefreshState {
    Refreshing,
    ValidatingPull,
    PublicationPending,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum DiagnosticSnapshotDelivery {
    Admitted,
    SettledValidation,
}

pub(super) struct RetainedDiagnosticSnapshot {
    document: DiagnosticDocumentIdentity,
    outcome: DiagnosticSnapshotOutcome,
    delivery: DiagnosticSnapshotDelivery,
}

impl<P, S, D> DaemonLspProtocolSession<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    /// The adapter transfers a completion once. The session admits it once,
    /// then owns the bounded report used by both push and pull presentation.
    pub(super) fn document_diagnostic_snapshot(
        &mut self,
        root: &AdmittedRoot,
        uri: &str,
        overlay: Option<&OverlaySnapshot>,
        for_pull: bool,
    ) -> DiagnosticSnapshotOutcome {
        let document = DiagnosticDocumentIdentity::new(
            root,
            self.lifecycle.gateway.workspace().scope_set_digest(),
            overlay,
        );
        if self
            .diagnostics
            .active_refreshes
            .get(uri)
            .is_some_and(|pending| pending.document != document)
        {
            self.invalidate_document_diagnostics(uri);
        }
        if self
            .diagnostics
            .document_snapshots
            .get(uri)
            .is_some_and(|retained| retained.document != document)
        {
            self.diagnostics.document_snapshots.remove(uri);
        }

        let outcome = self
            .diagnostics
            .provider
            .document_diagnostics(root, uri, overlay);
        if matches!(&outcome, DiagnosticSnapshotOutcome::Partial { coverage, .. }
            if coverage == "refresh-required")
        {
            let Some(retained) = self.diagnostics.document_snapshots.get(uri) else {
                return outcome;
            };
            let DiagnosticSnapshotOutcome::Ready { diagnostics, .. } = &retained.outcome else {
                return retained.outcome.clone();
            };
            let source_generation = Some(diagnostics.generation);
            let validation = self.diagnostics.provider.validate_document_snapshot(
                root,
                uri,
                overlay,
                diagnostics,
            );
            let outcome = match validation {
                DiagnosticSnapshotValidation::Current => return retained.outcome.clone(),
                DiagnosticSnapshotValidation::Changed => DiagnosticSnapshotOutcome::Partial {
                    source_generation,
                    coverage: "refresh-required".to_owned(),
                },
                DiagnosticSnapshotValidation::Pending(identity) => {
                    if valid_refresh_identity(&identity, source_generation) {
                        self.diagnostics.active_refreshes.insert(
                            uri.to_owned(),
                            PendingDiagnosticRefresh {
                                identity: identity.clone(),
                                document,
                                state: if for_pull {
                                    DiagnosticRefreshState::ValidatingPull
                                } else {
                                    DiagnosticRefreshState::Refreshing
                                },
                            },
                        );
                        DiagnosticSnapshotOutcome::Refreshing(identity)
                    } else {
                        let _ = self.diagnostics.provider.retire_document_refresh(
                            root,
                            uri,
                            &identity.operation_id,
                        );
                        DiagnosticSnapshotOutcome::Failed {
                            source_generation,
                            failure_class: "invalid-refresh-identity".to_owned(),
                        }
                    }
                }
                DiagnosticSnapshotValidation::Failed { failure_class } => {
                    DiagnosticSnapshotOutcome::Failed {
                        source_generation,
                        failure_class,
                    }
                }
            };
            self.diagnostics.document_snapshots.remove(uri);
            self.discard_queued_document_publications(uri);
            return outcome;
        }
        self.admit_document_diagnostic_snapshot(
            uri,
            document,
            outcome,
            DiagnosticSnapshotDelivery::Admitted,
        )
    }

    pub(super) fn admit_document_diagnostic_snapshot(
        &mut self,
        uri: &str,
        document: DiagnosticDocumentIdentity,
        outcome: DiagnosticSnapshotOutcome,
        delivery: DiagnosticSnapshotDelivery,
    ) -> DiagnosticSnapshotOutcome {
        let outcome = match outcome {
            DiagnosticSnapshotOutcome::Ready {
                mut diagnostics,
                completed_operation_id,
            } => {
                let pending = self.diagnostics.active_refreshes.get(uri);
                if let Some(pending) = pending {
                    if pending.document != document
                        || completed_operation_id.as_deref()
                            != Some(pending.identity.operation_id.as_str())
                    {
                        return DiagnosticSnapshotOutcome::Refreshing(pending.identity.clone());
                    }
                } else if completed_operation_id.is_some() {
                    return DiagnosticSnapshotOutcome::Partial {
                        source_generation: None,
                        coverage: "refresh-required".to_owned(),
                    };
                }
                let invalid_generation = self
                    .diagnostics
                    .active_refreshes
                    .get(uri)
                    .filter(|_| completed_operation_id.is_some())
                    .is_some_and(|pending| {
                        pending
                            .identity
                            .target_generation
                            .is_some_and(|target| target != diagnostics.generation)
                            || pending
                                .identity
                                .source_generation
                                .is_some_and(|source| diagnostics.generation < source)
                    });
                let superseded = self
                    .diagnostics
                    .published
                    .get(uri)
                    .is_some_and(|published| {
                        published.version == document.version
                            && published.generation > diagnostics.generation
                            && published.scope_digest == document.root.scope_digest().cloned()
                            && published.scope_set_digest == document.scope_set_digest
                    })
                    || self
                        .diagnostics
                        .document_snapshots
                        .get(uri)
                        .is_some_and(|retained| {
                            matches!(&retained.outcome, DiagnosticSnapshotOutcome::Ready {
                            diagnostics: previous, ..
                        } if previous.generation > diagnostics.generation)
                        });
                if superseded || invalid_generation {
                    DiagnosticSnapshotOutcome::Failed {
                        source_generation: Some(diagnostics.generation),
                        failure_class: "superseded-generation".to_owned(),
                    }
                } else {
                    // Keep the producer lanes separate: push and pull can have
                    // different capabilities. Each lane's sorted prefix suffices
                    // for the bounded merge, including Cursor's hidden lanes.
                    diagnostics.upstream =
                        DiagnosticMerge::for_document(uri, diagnostics.upstream, Vec::new()).items;
                    diagnostics.tracedecay =
                        DiagnosticMerge::for_document(uri, Vec::new(), diagnostics.tracedecay)
                            .items;
                    match diagnostic_snapshot_fits(&diagnostics) {
                        Ok(()) => DiagnosticSnapshotOutcome::Ready {
                            diagnostics,
                            completed_operation_id,
                        },
                        Err(failure_class) => DiagnosticSnapshotOutcome::Failed {
                            source_generation: Some(diagnostics.generation),
                            failure_class: failure_class.to_owned(),
                        },
                    }
                }
            }
            DiagnosticSnapshotOutcome::Refreshing(identity) => {
                self.diagnostics.document_snapshots.remove(uri);
                return DiagnosticSnapshotOutcome::Refreshing(identity);
            }
            terminal => terminal,
        };

        if !self.reserve_document_diagnostic_snapshot(uri) {
            return DiagnosticSnapshotOutcome::Failed {
                source_generation: None,
                failure_class: "diagnostic-result-capacity".to_owned(),
            };
        }
        if let DiagnosticSnapshotOutcome::Ready { diagnostics, .. } = &outcome {
            // Admission establishes the generation floor even if pushing the
            // report is backpressured or only pull delivery is negotiated.
            self.diagnostics.published.insert(
                uri.to_owned(),
                PublishedDiagnostic {
                    version: document.version,
                    generation: diagnostics.generation,
                    scope_digest: document.root.scope_digest().cloned(),
                    scope_set_digest: document.scope_set_digest.clone(),
                },
            );
        }
        self.diagnostics.document_snapshots.insert(
            uri.to_owned(),
            RetainedDiagnosticSnapshot {
                document,
                outcome: outcome.clone(),
                delivery,
            },
        );
        outcome
    }

    pub(super) fn take_settled_diagnostic_validation(
        &mut self,
        uri: &str,
        document: &DiagnosticDocumentIdentity,
    ) -> Option<DiagnosticSnapshotOutcome> {
        let retained = self.diagnostics.document_snapshots.get_mut(uri)?;
        if retained.document != *document {
            self.diagnostics.document_snapshots.remove(uri);
            return None;
        }
        if retained.delivery != DiagnosticSnapshotDelivery::SettledValidation {
            return None;
        }
        let outcome = retained.outcome.clone();
        if matches!(&outcome, DiagnosticSnapshotOutcome::Ready { .. }) {
            retained.delivery = DiagnosticSnapshotDelivery::Admitted;
        } else {
            self.diagnostics.document_snapshots.remove(uri);
        }
        Some(outcome)
    }

    pub(super) fn pending_diagnostic_publication(
        &self,
        uri: &str,
        pending: &PendingDiagnosticRefresh,
    ) -> DiagnosticSnapshotOutcome {
        if let Some(retained) = self.diagnostics.document_snapshots.get(uri)
            && retained.document == pending.document
            && matches!(&retained.outcome, DiagnosticSnapshotOutcome::Ready {
                completed_operation_id: Some(operation_id), ..
            } if operation_id == &pending.identity.operation_id)
        {
            return retained.outcome.clone();
        }
        DiagnosticSnapshotOutcome::Failed {
            source_generation: pending.identity.source_generation,
            failure_class: "diagnostic-publication-unavailable".to_owned(),
        }
    }

    pub(super) fn reserve_document_diagnostic_snapshot(&mut self, uri: &str) -> bool {
        if self.diagnostics.document_snapshots.contains_key(uri)
            || self.diagnostics.document_snapshots.len() < MAX_WORKSPACE_DIAGNOSTIC_RESULTS
        {
            return true;
        }
        // Open documents retain their current report. Unopened pull results
        // share the existing bounded result policy and can be recomputed after
        // eviction; an active completion must keep its publication retry.
        let evicted = self
            .diagnostics
            .document_snapshots
            .keys()
            .find(|candidate| {
                self.lifecycle.overlays.version(candidate).is_none()
                    && !self.diagnostics.active_refreshes.contains_key(*candidate)
            })
            .cloned();
        let Some(evicted) = evicted else {
            return false;
        };
        self.diagnostics.document_snapshots.remove(&evicted);
        self.diagnostics.published.remove(&evicted);
        true
    }

    pub(super) fn retire_diagnostic_refresh(&mut self, uri: &str) {
        if let Some(pending) = self.diagnostics.active_refreshes.remove(uri) {
            // Removing result ownership and requesting abort do not assert that
            // accepted analyzer work has already stopped.
            let _ = self.diagnostics.provider.retire_document_refresh(
                &pending.document.root,
                uri,
                &pending.identity.operation_id,
            );
        }
    }

    /// A dependency edit can invalidate any report in its admitted root. Route
    /// each document through the workspace authority so nested roots stay isolated.
    pub(super) fn invalidate_root_diagnostics(&mut self, root: &AdmittedRoot) {
        let documents: BTreeSet<_> = self
            .diagnostics
            .document_snapshots
            .keys()
            .chain(self.diagnostics.active_refreshes.keys())
            .chain(self.diagnostics.published.keys())
            .filter(|uri| self.document_root(uri).is_ok_and(|owner| &owner == root))
            .cloned()
            .collect();
        let invalidated = !documents.is_empty();
        for uri in documents {
            self.invalidate_document_diagnostics(&uri);
            self.discard_queued_document_publications(&uri);
        }
        if invalidated {
            self.queue_diagnostic_refresh();
        }
    }

    pub(super) fn invalidate_document_diagnostics(&mut self, uri: &str) {
        self.retire_diagnostic_refresh(uri);
        self.diagnostics.document_snapshots.remove(uri);
        if self.lifecycle.overlays.version(uri).is_none() {
            self.diagnostics.published.remove(uri);
        }
    }

    pub(super) fn clear_document_diagnostics(&mut self) {
        let documents: Vec<_> = self.diagnostics.active_refreshes.keys().cloned().collect();
        for uri in documents {
            self.retire_diagnostic_refresh(&uri);
        }
        self.diagnostics.document_snapshots.clear();
    }
}

fn diagnostic_snapshot_fits(snapshot: &GenerationDiagnostics) -> Result<(), &'static str> {
    // Count every retained field against the existing diagnostic result byte
    // bound, including fields that either presentation may omit.
    let mut remaining = MAX_WORKSPACE_DIAGNOSTIC_BYTES;
    for diagnostic in snapshot.upstream.iter().chain(&snapshot.tracedecay) {
        let value = diagnostic_value(
            diagnostic.clone(),
            DiagnosticSerializationCapabilities::full_identity(),
        );
        let bytes = serde_json::to_vec(&value).map_err(|_| "diagnostic-serialization-failed")?;
        remaining = remaining
            .checked_sub(bytes.len().saturating_add(diagnostic.uri.len()))
            .ok_or("diagnostic-result-byte-capacity")?;
    }
    Ok(())
}

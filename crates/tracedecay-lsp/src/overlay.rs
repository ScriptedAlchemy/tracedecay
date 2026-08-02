//! Ephemeral per-session LSP document overlays.
//!
//! Overlays are deliberately plain in-memory values. They are never handed to
//! a storage port, never included in a clean generation, and are released on
//! `didClose`, session expiry, or daemon shutdown. A daemon-owned analyzer may
//! receive an overlay only through its explicitly admitted session adapter.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::diagnostics::{LspRange, PositionError, utf16_position_to_byte_offset};
use crate::gateway::operation_table::{BoundedOperationTable, OperationAdmission, OperationPoll};
use crate::gateway::{AdmittedRoot, LspRuntimeFailure, LspRuntimeFuture, LspRuntimeSpawner};
use crate::provider::{
    DiagnosticRefreshAdmission, DiagnosticRefreshIdentity, DiagnosticSnapshotOutcome,
    DiagnosticSnapshotPort, GenerationDiagnostics,
};
use crate::request_sequence::ProcessLocalRequestSequence;
use tracedecay_domain::ContentDigest;

/// A single unsaved document cannot consume more than two MiB of the daemon.
pub const MAX_OVERLAY_BYTES: usize = 2 * 1024 * 1024;
/// A session cannot accumulate an unbounded number of individually bounded
/// documents.
pub const MAX_OPEN_DOCUMENTS: usize = 128;
/// Debounced work is bounded independently because closing documents frees
/// overlay slots before their terminal clear is emitted.
pub const MAX_PENDING_OVERLAY_DIAGNOSTICS: usize = 128;
/// Consecutive document changes coalesce before an analyzer refresh.
pub const OVERLAY_DIAGNOSTIC_DEBOUNCE_MS: u64 = 75;
/// A stream of edits cannot postpone the latest diagnostic indefinitely.
pub const OVERLAY_DIAGNOSTIC_MAX_WAIT_MS: u64 = 250;

/// One LSP `TextDocumentContentChangeEvent` projected without JSON transport
/// details. A missing range replaces the entire document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverlayChange {
    pub range: Option<LspRange>,
    pub range_length: Option<u32>,
    pub text: String,
}

/// A read-only view passed to an admitted analyzer/provider.
///
/// `ephemeral` is intentionally explicit so adapters cannot accidentally
/// treat an unsaved view as a reusable clean-generation input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverlaySnapshot {
    pub uri: String,
    pub language_id: String,
    pub version: i64,
    pub text: String,
    pub ephemeral: bool,
}

/// Failure while admitting or applying an overlay update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OverlayError {
    AlreadyOpen,
    NotOpen,
    InvalidVersion { current: i64, received: i64 },
    InvalidRange(PositionError),
    InvalidRangeLength { expected: u32, received: u32 },
    RangeLengthWithoutRange,
    TooManyDocuments { limit: usize },
    TooLarge { size: usize, limit: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DocumentOverlay {
    language_id: String,
    version: i64,
    text: String,
}

/// In-memory overlays owned by exactly one LSP client session.
#[derive(Clone, Debug, Default)]
pub struct OverlayStore {
    documents: BTreeMap<String, DocumentOverlay>,
}

impl OverlayStore {
    pub fn open(
        &mut self,
        uri: impl Into<String>,
        language_id: impl Into<String>,
        version: i64,
        text: impl Into<String>,
    ) -> Result<OverlaySnapshot, OverlayError> {
        let uri = uri.into();
        if self.documents.contains_key(&uri) {
            return Err(OverlayError::AlreadyOpen);
        }
        if self.documents.len() >= MAX_OPEN_DOCUMENTS {
            return Err(OverlayError::TooManyDocuments {
                limit: MAX_OPEN_DOCUMENTS,
            });
        }
        let text = text.into();
        ensure_size(&text)?;
        let document = DocumentOverlay {
            language_id: language_id.into(),
            version,
            text,
        };
        let snapshot = snapshot(&uri, &document);
        self.documents.insert(uri, document);
        Ok(snapshot)
    }

    /// Applies an ordered `didChange` batch. A version must strictly advance;
    /// LSP does not require consecutive integer versions, only causal order.
    pub fn change(
        &mut self,
        uri: &str,
        version: i64,
        changes: &[OverlayChange],
    ) -> Result<OverlaySnapshot, OverlayError> {
        let Some(document) = self.documents.get_mut(uri) else {
            return Err(OverlayError::NotOpen);
        };
        if version <= document.version {
            return Err(OverlayError::InvalidVersion {
                current: document.version,
                received: version,
            });
        }

        // Apply to a temporary value so an invalid later edit cannot leave a
        // partially modified overlay behind.
        let mut text = document.text.clone();
        for change in changes {
            apply_change(&mut text, change)?;
            ensure_size(&text)?;
        }
        document.version = version;
        document.text = text;
        Ok(snapshot(uri, document))
    }

    pub fn close(&mut self, uri: &str) -> Result<OverlaySnapshot, OverlayError> {
        let Some(document) = self.documents.remove(uri) else {
            return Err(OverlayError::NotOpen);
        };
        Ok(snapshot(uri, &document))
    }

    pub fn snapshot(&self, uri: &str) -> Option<OverlaySnapshot> {
        self.documents
            .get(uri)
            .map(|document| snapshot(uri, document))
    }

    pub fn version(&self, uri: &str) -> Option<i64> {
        self.documents.get(uri).map(|document| document.version)
    }

    /// Releases every unsaved value. This is called by the session lifecycle
    /// owner; no close event is persisted or synthesized.
    pub fn clear(&mut self) {
        self.documents.clear();
    }
}

fn snapshot(uri: &str, document: &DocumentOverlay) -> OverlaySnapshot {
    OverlaySnapshot {
        uri: uri.to_owned(),
        language_id: document.language_id.clone(),
        version: document.version,
        text: document.text.clone(),
        ephemeral: true,
    }
}

fn apply_change(text: &mut String, change: &OverlayChange) -> Result<(), OverlayError> {
    let Some(range) = change.range else {
        if change.range_length.is_some() {
            return Err(OverlayError::RangeLengthWithoutRange);
        }
        text.clone_from(&change.text);
        return Ok(());
    };
    let start =
        utf16_position_to_byte_offset(text, range.start).map_err(OverlayError::InvalidRange)?;
    let end = utf16_position_to_byte_offset(text, range.end).map_err(OverlayError::InvalidRange)?;
    if start > end {
        return Err(OverlayError::InvalidRange(
            PositionError::CharacterOutOfBounds,
        ));
    }
    if let Some(received) = change.range_length {
        let expected = text[start..end].encode_utf16().count() as u32;
        if expected != received {
            return Err(OverlayError::InvalidRangeLength { expected, received });
        }
    }
    text.replace_range(start..end, &change.text);
    Ok(())
}

fn ensure_size(text: &str) -> Result<(), OverlayError> {
    if text.len() > MAX_OVERLAY_BYTES {
        return Err(OverlayError::TooLarge {
            size: text.len(),
            limit: MAX_OVERLAY_BYTES,
        });
    }
    Ok(())
}

pub const MAX_DIAGNOSTIC_OPERATIONS: usize = 128;

/// Exact input passed to canonical diagnostic refresh work.
#[derive(Clone, Debug)]
pub struct CanonicalDiagnosticRefreshRequest {
    pub root: AdmittedRoot,
    pub document_uri: String,
    pub overlay: Option<OverlaySnapshot>,
    pub source_generation: Option<u64>,
}

/// Current canonical managed diagnostics created by the feedback owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedDiagnosticSnapshot {
    pub generation: u64,
    pub diagnostics: Vec<crate::diagnostics::GatewayDiagnostic>,
}

pub trait ManagedDiagnosticSnapshotPort: Send + Sync {
    fn snapshot(
        &self,
        request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<ManagedDiagnosticSnapshot, LspRuntimeFailure>>;
}

/// Non-blocking application boundary for a complete diagnostic snapshot.
pub trait CanonicalDiagnosticSnapshotAuthority: Send + Sync {
    fn refresh(
        &self,
        request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<GenerationDiagnostics, LspRuntimeFailure>>;
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DiagnosticOperationKey {
    root_uri: String,
    document_uri: String,
    overlay_version: i64,
    overlay_language_id: Option<String>,
    overlay_digest: Option<ContentDigest>,
}

/// Bounded non-blocking broker over canonical diagnostic refresh work.
pub struct DiagnosticSnapshotAdapter {
    runtime: Arc<dyn LspRuntimeSpawner>,
    authority: Arc<dyn CanonicalDiagnosticSnapshotAuthority>,
    next_operation: ProcessLocalRequestSequence,
    operations: BoundedOperationTable<
        DiagnosticOperationKey,
        DiagnosticRefreshIdentity,
        DiagnosticSnapshotOutcome,
    >,
}

impl DiagnosticSnapshotAdapter {
    pub fn new(
        runtime: Arc<dyn LspRuntimeSpawner>,
        authority: Arc<dyn CanonicalDiagnosticSnapshotAuthority>,
    ) -> Self {
        Self {
            runtime,
            authority,
            next_operation: ProcessLocalRequestSequence::starting_at(1),
            operations: BoundedOperationTable::new(MAX_DIAGNOSTIC_OPERATIONS),
        }
    }

    fn key(
        root: &AdmittedRoot,
        document_uri: &str,
        overlay: Option<&OverlaySnapshot>,
    ) -> DiagnosticOperationKey {
        DiagnosticOperationKey {
            root_uri: root.uri().to_owned(),
            document_uri: document_uri.to_owned(),
            overlay_version: overlay.map_or(0, |overlay| overlay.version),
            overlay_language_id: overlay.map(|overlay| overlay.language_id.clone()),
            overlay_digest: overlay.map(|overlay| ContentDigest::of_bytes(overlay.text.as_bytes())),
        }
    }
}

impl DiagnosticSnapshotPort for DiagnosticSnapshotAdapter {
    fn document_diagnostics(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        overlay: Option<&OverlaySnapshot>,
    ) -> DiagnosticSnapshotOutcome {
        let key = Self::key(root, document_uri, overlay);
        match self.operations.poll(&key) {
            OperationPoll::Ready {
                metadata: _,
                result,
            } => result,
            OperationPoll::Pending(identity) => DiagnosticSnapshotOutcome::Refreshing(identity),
            OperationPoll::Dropped(_) => DiagnosticSnapshotOutcome::Failed {
                source_generation: None,
                failure_class: "diagnostic-operation-dropped".to_owned(),
            },
            OperationPoll::Missing | OperationPoll::Mismatch(_) => {
                DiagnosticSnapshotOutcome::Partial {
                    source_generation: None,
                    coverage: "refresh-required".to_owned(),
                }
            }
            OperationPoll::Busy => DiagnosticSnapshotOutcome::Partial {
                source_generation: None,
                coverage: "runtime-busy".to_owned(),
            },
        }
    }

    fn request_document_refresh(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        overlay: Option<&OverlaySnapshot>,
        source_generation: Option<u64>,
    ) -> DiagnosticRefreshAdmission {
        let key = Self::key(root, document_uri, overlay);
        let request = CanonicalDiagnosticRefreshRequest {
            root: root.clone(),
            document_uri: document_uri.to_owned(),
            overlay: overlay.cloned(),
            source_generation,
        };
        let authority = Arc::clone(&self.authority);
        let admission: Result<_, crate::request_sequence::SequenceExhausted> =
            self.operations.admit_with(key, self.runtime.as_ref(), || {
                let operation_id = self.next_operation.next_string("lsp-diagnostic-")?;
                let identity = DiagnosticRefreshIdentity {
                    operation_id: operation_id.clone(),
                    source_generation,
                    target_generation: None,
                };
                let operation = Box::pin(async move {
                    match authority.refresh(request).await {
                        Ok(diagnostics) => DiagnosticSnapshotOutcome::Ready {
                            diagnostics,
                            completed_operation_id: Some(operation_id),
                        },
                        Err(error) => DiagnosticSnapshotOutcome::Failed {
                            source_generation,
                            failure_class: error.class().to_owned(),
                        },
                    }
                }) as LspRuntimeFuture<DiagnosticSnapshotOutcome>;
                Ok((identity, operation))
            });
        match admission {
            Ok(OperationAdmission::Started(identity)) => {
                DiagnosticRefreshAdmission::Started(identity)
            }
            Ok(OperationAdmission::Existing(identity)) => {
                DiagnosticRefreshAdmission::AlreadyRunning(identity)
            }
            Ok(OperationAdmission::Busy) => DiagnosticRefreshAdmission::Rejected {
                failure_class: "runtime-busy".to_owned(),
            },
            Ok(OperationAdmission::Saturated) => DiagnosticRefreshAdmission::Rejected {
                failure_class: "diagnostic-capacity".to_owned(),
            },
            Err(_) => DiagnosticRefreshAdmission::Rejected {
                failure_class: "diagnostic-identity-exhausted".to_owned(),
            },
        }
    }
}

/// A scheduled document diagnostic operation. The protocol session turns a
/// refresh into a provider call and a clear into an empty publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DebouncedDiagnosticKind {
    Refresh,
    Clear,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebouncedDiagnostic {
    pub uri: String,
    pub version: i64,
    pub kind: DebouncedDiagnosticKind,
}

#[derive(Clone, Debug)]
struct PendingDiagnostic {
    first_scheduled_at_ms: u64,
    due_at_ms: u64,
    version: i64,
    kind: DebouncedDiagnosticKind,
}

/// Bounded debounce state, separate from overlay bytes so closing a document
/// can still schedule a deterministic diagnostic clear.
#[derive(Clone, Debug, Default)]
pub struct OverlayDiagnosticDebouncer {
    pending: BTreeMap<String, PendingDiagnostic>,
}

impl OverlayDiagnosticDebouncer {
    pub fn schedule_refresh(&mut self, uri: impl Into<String>, version: i64, now_ms: u64) -> bool {
        self.schedule(
            uri.into(),
            version,
            DebouncedDiagnosticKind::Refresh,
            now_ms,
        )
    }

    /// A save is a terminal synchronization boundary: it advances an already
    /// pending refresh instead of waiting for the edit debounce window.
    pub fn schedule_immediate_refresh(
        &mut self,
        uri: impl Into<String>,
        version: i64,
        now_ms: u64,
    ) -> bool {
        let uri = uri.into();
        if !self.schedule(
            uri.clone(),
            version,
            DebouncedDiagnosticKind::Refresh,
            now_ms,
        ) {
            return false;
        }
        if let Some(pending) = self.pending.get_mut(&uri)
            && pending.kind == DebouncedDiagnosticKind::Refresh
        {
            pending.due_at_ms = now_ms;
        }
        true
    }

    pub fn schedule_clear(&mut self, uri: impl Into<String>, version: i64, now_ms: u64) -> bool {
        self.schedule(uri.into(), version, DebouncedDiagnosticKind::Clear, now_ms)
    }

    pub fn take_due(&mut self, now_ms: u64) -> Vec<DebouncedDiagnostic> {
        let mut due = Vec::new();
        while let Some(next) = self.take_next_due(now_ms) {
            due.push(next);
        }
        due
    }

    pub fn take_next_due(&mut self, now_ms: u64) -> Option<DebouncedDiagnostic> {
        let uri = self
            .pending
            .iter()
            .find(|(_, pending)| pending.due_at_ms <= now_ms)
            .map(|(uri, _)| uri.clone())?;
        self.pending
            .remove(&uri)
            .map(|pending| DebouncedDiagnostic {
                uri,
                version: pending.version,
                kind: pending.kind,
            })
    }

    pub fn cancel(&mut self, uri: &str) -> bool {
        self.pending.remove(uri).is_some()
    }

    pub fn clear(&mut self) {
        self.pending.clear();
    }

    fn schedule(
        &mut self,
        uri: String,
        version: i64,
        kind: DebouncedDiagnosticKind,
        now_ms: u64,
    ) -> bool {
        let requested_due = now_ms.saturating_add(OVERLAY_DIAGNOSTIC_DEBOUNCE_MS);
        if let Some(pending) = self.pending.get_mut(&uri) {
            // A close is terminal for the current document version and
            // must not be overwritten by a stale refresh.
            if kind == DebouncedDiagnosticKind::Clear
                || pending.kind != DebouncedDiagnosticKind::Clear
            {
                pending.kind = kind;
                pending.version = version;
            }
            let latest_allowed = pending
                .first_scheduled_at_ms
                .saturating_add(OVERLAY_DIAGNOSTIC_MAX_WAIT_MS);
            pending.due_at_ms = requested_due.min(latest_allowed);
            true
        } else {
            if self.pending.len() >= MAX_PENDING_OVERLAY_DIAGNOSTICS {
                return false;
            }
            self.pending.insert(
                uri,
                PendingDiagnostic {
                    first_scheduled_at_ms: now_ms,
                    due_at_ms: requested_due,
                    version,
                    kind,
                },
            );
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{LspPosition, LspRange};
    use crate::gateway::{AdmittedRoot, LspRuntimeFuture, LspRuntimeSpawner, LspRuntimeTask};
    use crate::provider::{
        DiagnosticRefreshAdmission, DiagnosticRefreshIdentity, DiagnosticSnapshotOutcome,
        DiagnosticSnapshotPort, GenerationDiagnostics,
    };
    use std::sync::Arc;
    use std::task::{Context, Poll};

    fn range(start: u32, end: u32) -> LspRange {
        LspRange {
            start: LspPosition {
                line: 0,
                character: start,
            },
            end: LspPosition {
                line: 0,
                character: end,
            },
        }
    }

    #[test]
    fn incremental_edits_are_utf16_ordered_and_ephemeral() {
        let mut overlays = OverlayStore::default();
        let opened = overlays
            .open("file:///root/a.rs", "rust", 3, "a🦀b")
            .unwrap();
        assert!(opened.ephemeral);
        let changed = overlays
            .change(
                "file:///root/a.rs",
                7,
                &[OverlayChange {
                    range: Some(range(1, 3)),
                    range_length: Some(2),
                    text: "cat".into(),
                }],
            )
            .unwrap();
        assert_eq!(changed.text, "acatb");
        assert_eq!(
            overlays.change("file:///root/a.rs", 7, &[]),
            Err(OverlayError::InvalidVersion {
                current: 7,
                received: 7,
            })
        );
        assert_eq!(overlays.close("file:///root/a.rs").unwrap().version, 7);
        assert!(overlays.snapshot("file:///root/a.rs").is_none());
    }

    #[test]
    fn invalid_later_change_does_not_partially_mutate_document() {
        let mut overlays = OverlayStore::default();
        overlays
            .open("file:///root/a.rs", "rust", 1, "abc")
            .unwrap();
        let result = overlays.change(
            "file:///root/a.rs",
            2,
            &[
                OverlayChange {
                    range: None,
                    range_length: None,
                    text: "changed".into(),
                },
                OverlayChange {
                    range: Some(range(99, 99)),
                    range_length: None,
                    text: "x".into(),
                },
            ],
        );
        assert!(matches!(result, Err(OverlayError::InvalidRange(_))));
        assert_eq!(overlays.snapshot("file:///root/a.rs").unwrap().text, "abc");
    }

    #[test]
    fn full_replacement_rejects_range_length_without_mutating_document() {
        let mut overlays = OverlayStore::default();
        overlays
            .open("file:///root/a.rs", "rust", 1, "abc")
            .unwrap();
        assert_eq!(
            overlays.change(
                "file:///root/a.rs",
                2,
                &[OverlayChange {
                    range: None,
                    range_length: Some(3),
                    text: "def".into(),
                }],
            ),
            Err(OverlayError::RangeLengthWithoutRange)
        );
        assert_eq!(overlays.snapshot("file:///root/a.rs").unwrap().text, "abc");
    }

    #[test]
    fn overlay_limit_is_enforced_before_state_is_published() {
        let mut overlays = OverlayStore::default();
        let oversized = "x".repeat(MAX_OVERLAY_BYTES + 1);
        assert_eq!(
            overlays.open("file:///root/a.rs", "rust", 1, oversized),
            Err(OverlayError::TooLarge {
                size: MAX_OVERLAY_BYTES + 1,
                limit: MAX_OVERLAY_BYTES,
            })
        );
    }

    #[test]
    fn document_and_debounce_counts_are_bounded() {
        let mut overlays = OverlayStore::default();
        for index in 0..MAX_OPEN_DOCUMENTS {
            overlays
                .open(format!("file:///root/{index}.rs"), "rust", 1, "")
                .unwrap();
        }
        assert_eq!(
            overlays.open("file:///root/overflow.rs", "rust", 1, ""),
            Err(OverlayError::TooManyDocuments {
                limit: MAX_OPEN_DOCUMENTS,
            })
        );

        let mut debounce = OverlayDiagnosticDebouncer::default();
        for index in 0..MAX_PENDING_OVERLAY_DIAGNOSTICS {
            assert!(debounce.schedule_refresh(format!("file:///root/{index}.rs"), 1, 0));
        }
        assert!(!debounce.schedule_refresh("file:///root/overflow.rs", 1, 0));
        assert!(debounce.schedule_refresh("file:///root/0.rs", 2, 1));
    }

    #[test]
    fn debounce_coalesces_churn_but_not_terminal_close() {
        let mut debounce = OverlayDiagnosticDebouncer::default();
        assert!(debounce.schedule_refresh("file:///root/a.rs", 1, 0));
        assert!(debounce.schedule_refresh("file:///root/a.rs", 2, 40));
        assert!(debounce.take_due(114).is_empty());
        assert_eq!(
            debounce.take_due(115),
            vec![DebouncedDiagnostic {
                uri: "file:///root/a.rs".into(),
                version: 2,
                kind: DebouncedDiagnosticKind::Refresh,
            }]
        );

        assert!(debounce.schedule_refresh("file:///root/a.rs", 3, 120));
        assert!(debounce.schedule_clear("file:///root/a.rs", 3, 130));
        assert_eq!(
            debounce.take_due(205),
            vec![DebouncedDiagnostic {
                uri: "file:///root/a.rs".into(),
                version: 3,
                kind: DebouncedDiagnosticKind::Clear,
            }]
        );
    }

    #[test]
    fn immediate_refresh_flushes_pending_edit_debounce() {
        let mut debounce = OverlayDiagnosticDebouncer::default();
        assert!(debounce.schedule_refresh("file:///root/a.rs", 1, 0));
        assert!(debounce.schedule_immediate_refresh("file:///root/a.rs", 2, 10));

        assert_eq!(
            debounce.take_due(10),
            vec![DebouncedDiagnostic {
                uri: "file:///root/a.rs".into(),
                version: 2,
                kind: DebouncedDiagnosticKind::Refresh,
            }]
        );
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

    struct Diagnostics;

    impl CanonicalDiagnosticSnapshotAuthority for Diagnostics {
        fn refresh(
            &self,
            _request: CanonicalDiagnosticRefreshRequest,
        ) -> LspRuntimeFuture<Result<GenerationDiagnostics, LspRuntimeFailure>> {
            Box::pin(async {
                Ok(GenerationDiagnostics {
                    generation: 7,
                    upstream: Vec::new(),
                    tracedecay: Vec::new(),
                })
            })
        }
    }

    #[test]
    fn diagnostic_broker_reuses_exact_overlay_identity_and_polls_completion() {
        let adapter =
            DiagnosticSnapshotAdapter::new(Arc::new(InlineSpawner), Arc::new(Diagnostics));
        let root = AdmittedRoot::new("file:///root");
        let overlay = OverlaySnapshot {
            uri: "file:///root/a.rs".to_owned(),
            language_id: "rust".to_owned(),
            version: 3,
            text: "fn main() {}".to_owned(),
            ephemeral: true,
        };
        assert_eq!(
            adapter.request_document_refresh(&root, "file:///root/a.rs", Some(&overlay), None,),
            DiagnosticRefreshAdmission::Started(DiagnosticRefreshIdentity {
                operation_id: "lsp-diagnostic-1".to_owned(),
                source_generation: None,
                target_generation: None,
            })
        );
        assert!(matches!(
            adapter.document_diagnostics(&root, "file:///root/a.rs", Some(&overlay)),
            DiagnosticSnapshotOutcome::Ready {
                diagnostics: GenerationDiagnostics { generation: 7, .. },
                ..
            }
        ));
    }
}

//! Bounded custom context projections carried beside standard LSP methods.
//!
//! The daemon/application owner computes and stores projection truth. This
//! module defines only compact transport DTOs and the read-only port used by
//! one already-authorized, single-root LSP session.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_domain::ContentDigest;

use crate::gateway::operation_table::{
    BoundedOperationCapacity, BoundedOperationTable, OperationAdmission, OperationPoll,
};
use crate::gateway::{AdmittedRoot, LspRuntimeFuture, LspRuntimeSpawner};
use crate::session::{LspRequestId, MAX_PENDING_REQUESTS};

pub const TRACEDECAY_CONTEXT_REVISION: u32 = 1;
pub const TRACEDECAY_CONTEXT_METHOD: &str = "tracedecay/context";
pub const TRACEDECAY_CONTEXT_EXPAND_METHOD: &str = "tracedecay/context/expand";
pub const TRACEDECAY_SUBSCRIBE_METHOD: &str = "tracedecay/subscribe";
pub const TRACEDECAY_CONTEXT_CHANGED_METHOD: &str = "tracedecay/contextChanged";
pub const MAX_CONTEXT_PROJECTION_ITEMS: usize = 64;
pub const MAX_CONTEXT_PROJECTION_BYTES: usize = 128 * 1024;
pub const MAX_CONTEXT_PROJECTION_KINDS: usize = 8;
pub const MAX_CONTEXT_CHANGES_PER_POLL: usize = 16;
pub const MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES: usize = 256;
pub const MAX_CONTEXT_SUMMARY_BYTES: usize = 512;

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ContextProjectionKind(String);

impl ContextProjectionKind {
    pub const DIAGNOSTICS: &'static str = "diagnostics";
    pub const POST_EDIT_IMPACT: &'static str = "postEditImpact";
    pub const AFFECTED_TESTS: &'static str = "affectedTests";
    pub const TEST_RUN_RESULTS: &'static str = "testRunResults";
    /// GitHub review ingest contribution of the composed advisory cycle.
    pub const GITHUB_REVIEW: &'static str = "githubReview";
    /// CI failure localization contribution of the composed advisory cycle.
    pub const CI_FAILURE_LOCALIZATION: &'static str = "ciFailureLocalization";
    /// Agent proximity contribution of the composed advisory cycle.
    pub const AGENT_PROXIMITY: &'static str = "agentProximity";

    /// Advisory contributions the canonical feedback source can expose through
    /// the typed context projection authority.
    pub const ADVISORY: [&'static str; 3] = [
        Self::GITHUB_REVIEW,
        Self::CI_FAILURE_LOCALIZATION,
        Self::AGENT_PROXIMITY,
    ];

    pub fn diagnostics() -> Self {
        Self(Self::DIAGNOSTICS.to_owned())
    }

    pub fn post_edit_impact() -> Self {
        Self(Self::POST_EDIT_IMPACT.to_owned())
    }

    pub fn affected_tests() -> Self {
        Self(Self::AFFECTED_TESTS.to_owned())
    }

    pub fn test_run_results() -> Self {
        Self(Self::TEST_RUN_RESULTS.to_owned())
    }

    pub fn github_review() -> Self {
        Self(Self::GITHUB_REVIEW.to_owned())
    }

    pub fn ci_failure_localization() -> Self {
        Self(Self::CI_FAILURE_LOCALIZATION.to_owned())
    }

    pub fn agent_proximity() -> Self {
        Self(Self::AGENT_PROXIMITY.to_owned())
    }

    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        Self::is_valid_str(&value).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_valid(&self) -> bool {
        Self::is_valid_str(&self.0)
    }

    pub fn is_supported(&self) -> bool {
        matches!(
            self.as_str(),
            Self::DIAGNOSTICS
                | Self::POST_EDIT_IMPACT
                | Self::AFFECTED_TESTS
                | Self::TEST_RUN_RESULTS
        ) || self.is_advisory()
    }

    /// Whether this kind is one of the advisory-cycle contributions carried by
    /// the context extension rather than a baseline reader projection.
    pub fn is_advisory(&self) -> bool {
        Self::ADVISORY.contains(&self.as_str())
    }

    fn is_valid_str(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ContextProjectionRegistration {
    pub kind: ContextProjectionKind,
    pub revision: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ContextCoverage {
    Complete,
    Partial,
    Unavailable,
    Failed,
}

/// Exact immutable code-index identity carried by every projection.
///
/// `document_content_digest` is present only for a document-scoped request;
/// root-scoped projections remain bound to the complete snapshot digest and
/// content identity without inventing a document identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ContextProjectionIdentity {
    pub head_commit_id: String,
    pub code_generation_id: String,
    pub snapshot_digest: String,
    pub invalidation_digest: String,
    pub snapshot_content_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_content_digest: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ContextFreshness {
    Current,
    Stale,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ContextProducerState {
    Complete,
    Partial,
    Indexing,
    Unavailable,
    Failed,
    Cancelled,
    TimedOut,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ContextProjectionRequest {
    pub kind: ContextProjectionKind,
    #[serde(default)]
    pub document_uri: Option<String>,
    /// Exact session-owned overlay identity. The protocol actor binds this
    /// after decoding; clients cannot claim a trusted content identity.
    #[serde(skip)]
    pub(crate) document_content_digest: Option<ContentDigest>,
}

impl ContextProjectionRequest {
    /// Builds a request with no bound content identity. Only the protocol
    /// actor may bind one, so a caller-constructed request always starts
    /// unbound — the same state a decoded client payload starts in.
    pub fn new(kind: ContextProjectionKind, document_uri: Option<String>) -> Self {
        Self {
            kind,
            document_uri,
            document_content_digest: None,
        }
    }

    /// The overlay content identity the protocol actor bound for this request,
    /// or `None` when the request is not document-scoped.
    pub fn document_content_digest(&self) -> Option<&ContentDigest> {
        self.document_content_digest.as_ref()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ContextExpansionRequest {
    pub retrieval_handle: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ContextSubscribeRequest {
    pub projections: Vec<ContextProjectionRegistration>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextProjectionItem {
    pub stable_id: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrieval_handle: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextProjectionEnvelope {
    pub root_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_uri: Option<String>,
    pub kind: ContextProjectionKind,
    pub generation: u64,
    pub identity: ContextProjectionIdentity,
    pub freshness: ContextFreshness,
    pub producer_state: ContextProducerState,
    pub coverage: ContextCoverage,
    pub revision: u32,
    pub items: Vec<ContextProjectionItem>,
    pub omitted_count: usize,
    pub omission_reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrieval_handle: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextProjectionChange {
    pub root_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_uri: Option<String>,
    pub kind: ContextProjectionKind,
    pub generation: u64,
    pub identity: ContextProjectionIdentity,
    pub freshness: ContextFreshness,
    pub producer_state: ContextProducerState,
    pub coverage: ContextCoverage,
    pub revision: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrieval_handle: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextExpansionScope {
    pub scope_digest: String,
    pub identity: ContextProjectionIdentity,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextExpansionEnvelope {
    pub root_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_uri: Option<String>,
    pub kind: ContextProjectionKind,
    pub stable_id: String,
    pub generation: u64,
    pub scope: ContextExpansionScope,
    pub expires_at: i64,
    pub coverage: ContextCoverage,
    pub revision: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omission_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_retrieval_handle: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
// Boxing the large variant would ripple through in-flight construction/match
// sites; the size gap is accepted here.
#[allow(clippy::large_enum_variant)]
pub enum ContextProjectionOutcome {
    Ready(ContextProjectionEnvelope),
    Unsupported,
    Denied,
    /// An admitted application operation is still running. The protocol actor
    /// retains the existing request correlation and polls this port without
    /// blocking its Tokio runtime thread.
    Pending,
    Deferred {
        reason: String,
    },
    Failed {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
// Boxing the large variant would ripple through in-flight construction/match
// sites; the size gap is accepted here.
#[allow(clippy::large_enum_variant)]
pub enum ContextExpansionOutcome {
    Ready(ContextExpansionEnvelope),
    Denied,
    Pending,
    Failed { reason: String },
}

pub const MAX_CONTEXT_OPERATIONS: usize = MAX_PENDING_REQUESTS * 2;

/// Canonical application boundary for versioned context projections.
pub trait CanonicalContextProjectionAuthority: Send + Sync {
    fn registrations(&self) -> Vec<ContextProjectionRegistration>;

    fn snapshot(
        &self,
        root: AdmittedRoot,
        request_id: LspRequestId,
        request: ContextProjectionRequest,
    ) -> LspRuntimeFuture<ContextProjectionOutcome>;

    fn expand(
        &self,
        _root: AdmittedRoot,
        _request_id: LspRequestId,
        _request: ContextExpansionRequest,
    ) -> LspRuntimeFuture<ContextExpansionOutcome> {
        Box::pin(async { ContextExpansionOutcome::Denied })
    }

    fn cancel_request(&self, _root: &AdmittedRoot, _request_id: &LspRequestId) -> bool {
        false
    }

    fn poll_changes(
        &self,
        _root: &AdmittedRoot,
        _subscriptions: &BTreeSet<ContextProjectionRegistration>,
    ) -> Vec<ContextProjectionChange> {
        Vec::new()
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ContextRequestKey {
    root_uri: String,
    request_id: LspRequestId,
}

type DeliveredContextChanges =
    BTreeMap<(String, Option<String>, ContextProjectionKind), ContextProjectionChange>;

/// Bounded non-blocking broker for canonical context projection work.
pub struct ContextProjectionAdapter {
    runtime: Arc<dyn LspRuntimeSpawner>,
    authority: Arc<dyn CanonicalContextProjectionAuthority>,
    in_flight: BoundedOperationTable<ContextRequestKey, (), ContextProjectionOutcome>,
    expansions: BoundedOperationTable<ContextRequestKey, (), ContextExpansionOutcome>,
    delivered_changes: Mutex<DeliveredContextChanges>,
}

impl ContextProjectionAdapter {
    pub fn new(
        runtime: Arc<dyn LspRuntimeSpawner>,
        authority: Arc<dyn CanonicalContextProjectionAuthority>,
    ) -> Self {
        let capacity = BoundedOperationCapacity::new(MAX_CONTEXT_OPERATIONS);
        Self {
            runtime,
            authority,
            in_flight: BoundedOperationTable::with_capacity(capacity.clone()),
            expansions: BoundedOperationTable::with_capacity(capacity),
            delivered_changes: Mutex::new(BTreeMap::new()),
        }
    }

    fn key(root: &AdmittedRoot, request_id: &LspRequestId) -> ContextRequestKey {
        ContextRequestKey {
            root_uri: root.uri().to_owned(),
            request_id: request_id.clone(),
        }
    }
}

impl ContextProjectionPort for ContextProjectionAdapter {
    fn registrations(&self) -> Vec<ContextProjectionRegistration> {
        self.authority.registrations()
    }

    fn snapshot(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &ContextProjectionRequest,
    ) -> ContextProjectionOutcome {
        let key = Self::key(root, request_id);
        let authority = Arc::clone(&self.authority);
        let root = root.clone();
        let request_id = request_id.clone();
        let request = request.clone();
        match self
            .in_flight
            .admit(key, (), self.runtime.as_ref(), move || {
                authority.snapshot(root, request_id, request)
            }) {
            OperationAdmission::Started(()) | OperationAdmission::Existing(()) => {
                ContextProjectionOutcome::Pending
            }
            OperationAdmission::Busy => ContextProjectionOutcome::Deferred {
                reason: "runtime-busy".to_owned(),
            },
            OperationAdmission::Saturated => ContextProjectionOutcome::Deferred {
                reason: "context-projection-capacity".to_owned(),
            },
        }
    }

    fn poll_snapshot(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
    ) -> Option<ContextProjectionOutcome> {
        let key = Self::key(root, request_id);
        match self.in_flight.poll(&key) {
            OperationPoll::Ready {
                metadata: (),
                result,
            } => Some(result),
            OperationPoll::Dropped(()) => Some(ContextProjectionOutcome::Failed {
                reason: "context-operation-dropped".to_owned(),
            }),
            OperationPoll::Pending(())
            | OperationPoll::Missing
            | OperationPoll::Busy
            | OperationPoll::Mismatch(()) => None,
        }
    }

    fn expand(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &ContextExpansionRequest,
    ) -> ContextExpansionOutcome {
        let key = Self::key(root, request_id);
        let authority = Arc::clone(&self.authority);
        let root = root.clone();
        let request_id = request_id.clone();
        let request = request.clone();
        match self
            .expansions
            .admit(key, (), self.runtime.as_ref(), move || {
                authority.expand(root, request_id, request)
            }) {
            OperationAdmission::Started(()) | OperationAdmission::Existing(()) => {
                ContextExpansionOutcome::Pending
            }
            OperationAdmission::Busy => ContextExpansionOutcome::Failed {
                reason: "runtime-busy".to_owned(),
            },
            OperationAdmission::Saturated => ContextExpansionOutcome::Failed {
                reason: "context-expansion-capacity".to_owned(),
            },
        }
    }

    fn poll_expansion(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
    ) -> Option<ContextExpansionOutcome> {
        let key = Self::key(root, request_id);
        match self.expansions.poll(&key) {
            OperationPoll::Ready {
                metadata: (),
                result,
            } => Some(result),
            OperationPoll::Dropped(()) => Some(ContextExpansionOutcome::Failed {
                reason: "context-expansion-operation-dropped".to_owned(),
            }),
            OperationPoll::Pending(())
            | OperationPoll::Missing
            | OperationPoll::Busy
            | OperationPoll::Mismatch(()) => None,
        }
    }

    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        let key = Self::key(root, request_id);
        let cancelled_projection = self.in_flight.cancel(&key);
        let cancelled_expansion = self.expansions.cancel(&key);
        self.authority.cancel_request(root, request_id)
            || cancelled_projection
            || cancelled_expansion
    }

    fn poll_changes(
        &self,
        root: &AdmittedRoot,
        subscriptions: &BTreeSet<ContextProjectionRegistration>,
        maximum: usize,
    ) -> Vec<ContextProjectionChange> {
        if maximum == 0 {
            return Vec::new();
        }
        let changes = self.authority.poll_changes(root, subscriptions);
        let Ok(mut delivered) = self.delivered_changes.try_lock() else {
            return Vec::new();
        };
        let mut ready = Vec::with_capacity(maximum);
        for change in changes {
            if ready.len() == maximum {
                break;
            }
            let key = (
                change.root_uri.clone(),
                change.document_uri.clone(),
                change.kind.clone(),
            );
            if matches!(delivered.get(&key), Some(previous) if previous == &change) {
                continue;
            }
            delivered.insert(key, change.clone());
            ready.push(change);
        }
        ready
    }

    fn update_subscriptions(
        &self,
        root: &AdmittedRoot,
        subscriptions: &BTreeSet<ContextProjectionRegistration>,
    ) {
        if let Ok(mut delivered) = self.delivered_changes.try_lock() {
            delivered.retain(|(root_uri, _, kind), _| {
                root_uri != root.uri()
                    || subscriptions.contains(&ContextProjectionRegistration {
                        kind: kind.clone(),
                        revision: TRACEDECAY_CONTEXT_REVISION,
                    })
            });
        }
    }
}

pub trait ContextProjectionPort {
    /// Registrations currently mounted for this admitted daemon owner. The
    /// initialize response advertises only the intersection of these values,
    /// gateway policy, and client support.
    fn registrations(&self) -> Vec<ContextProjectionRegistration>;

    fn snapshot(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &ContextProjectionRequest,
    ) -> ContextProjectionOutcome;

    /// Returns a terminal result for an admitted pending request, if one is
    /// available. `None` retains the protocol actor's bounded in-flight entry.
    fn poll_snapshot(
        &self,
        _root: &AdmittedRoot,
        _request_id: &LspRequestId,
    ) -> Option<ContextProjectionOutcome> {
        None
    }

    fn expand(
        &self,
        _root: &AdmittedRoot,
        _request_id: &LspRequestId,
        _request: &ContextExpansionRequest,
    ) -> ContextExpansionOutcome {
        ContextExpansionOutcome::Denied
    }

    fn poll_expansion(
        &self,
        _root: &AdmittedRoot,
        _request_id: &LspRequestId,
    ) -> Option<ContextExpansionOutcome> {
        None
    }

    fn cancel_request(&self, _root: &AdmittedRoot, _request_id: &LspRequestId) -> bool {
        false
    }

    /// Returns at most `maximum` undelivered changes. Implementors must not
    /// mark a change as delivered unless it is included in this result, so the
    /// session can resume a bounded notification stream on its next poll.
    fn poll_changes(
        &self,
        _root: &AdmittedRoot,
        _subscriptions: &BTreeSet<ContextProjectionRegistration>,
        _maximum: usize,
    ) -> Vec<ContextProjectionChange> {
        Vec::new()
    }

    fn update_subscriptions(
        &self,
        _root: &AdmittedRoot,
        _subscriptions: &BTreeSet<ContextProjectionRegistration>,
    ) {
    }
}

impl<T> ContextProjectionPort for Arc<T>
where
    T: ContextProjectionPort + ?Sized,
{
    fn registrations(&self) -> Vec<ContextProjectionRegistration> {
        (**self).registrations()
    }

    fn snapshot(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &ContextProjectionRequest,
    ) -> ContextProjectionOutcome {
        (**self).snapshot(root, request_id, request)
    }

    fn poll_snapshot(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
    ) -> Option<ContextProjectionOutcome> {
        (**self).poll_snapshot(root, request_id)
    }

    fn expand(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &ContextExpansionRequest,
    ) -> ContextExpansionOutcome {
        (**self).expand(root, request_id, request)
    }

    fn poll_expansion(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
    ) -> Option<ContextExpansionOutcome> {
        (**self).poll_expansion(root, request_id)
    }

    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        (**self).cancel_request(root, request_id)
    }

    fn poll_changes(
        &self,
        root: &AdmittedRoot,
        subscriptions: &BTreeSet<ContextProjectionRegistration>,
        maximum: usize,
    ) -> Vec<ContextProjectionChange> {
        (**self).poll_changes(root, subscriptions, maximum)
    }

    fn update_subscriptions(
        &self,
        root: &AdmittedRoot,
        subscriptions: &BTreeSet<ContextProjectionRegistration>,
    ) {
        (**self).update_subscriptions(root, subscriptions);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{LspRuntimeFuture, LspRuntimeSpawner, LspRuntimeTask};
    use std::sync::Arc;
    use std::task::{Context, Poll};

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

    struct Authority {
        change_count: usize,
    }

    fn identity() -> ContextProjectionIdentity {
        ContextProjectionIdentity {
            head_commit_id: "0123456789abcdef".to_owned(),
            code_generation_id: "generation:7".to_owned(),
            snapshot_digest: format!("sha256:{}", "a".repeat(64)),
            invalidation_digest: format!("sha256:{}", "b".repeat(64)),
            snapshot_content_digest: format!("sha256:{}", "c".repeat(64)),
            document_content_digest: None,
        }
    }

    impl CanonicalContextProjectionAuthority for Authority {
        fn registrations(&self) -> Vec<ContextProjectionRegistration> {
            vec![ContextProjectionRegistration {
                kind: ContextProjectionKind::diagnostics(),
                revision: TRACEDECAY_CONTEXT_REVISION,
            }]
        }

        fn snapshot(
            &self,
            root: AdmittedRoot,
            _request_id: LspRequestId,
            request: ContextProjectionRequest,
        ) -> LspRuntimeFuture<ContextProjectionOutcome> {
            Box::pin(async move {
                ContextProjectionOutcome::Ready(ContextProjectionEnvelope {
                    root_uri: root.uri().to_owned(),
                    document_uri: request.document_uri,
                    kind: request.kind,
                    generation: 7,
                    identity: identity(),
                    freshness: ContextFreshness::Current,
                    producer_state: ContextProducerState::Complete,
                    coverage: ContextCoverage::Complete,
                    revision: TRACEDECAY_CONTEXT_REVISION,
                    items: Vec::new(),
                    omitted_count: 0,
                    omission_reasons: Vec::new(),
                    retrieval_handle: None,
                })
            })
        }

        fn expand(
            &self,
            root: AdmittedRoot,
            _request_id: LspRequestId,
            _request: ContextExpansionRequest,
        ) -> LspRuntimeFuture<ContextExpansionOutcome> {
            Box::pin(async move {
                ContextExpansionOutcome::Ready(ContextExpansionEnvelope {
                    root_uri: root.uri().to_owned(),
                    document_uri: None,
                    kind: ContextProjectionKind::diagnostics(),
                    stable_id: "finding.1".to_owned(),
                    generation: 7,
                    scope: ContextExpansionScope {
                        scope_digest: "sha256:scope".to_owned(),
                        identity: identity(),
                    },
                    expires_at: 10_000,
                    coverage: ContextCoverage::Complete,
                    revision: TRACEDECAY_CONTEXT_REVISION,
                    evidence: Some(serde_json::json!({ "canonical": true })),
                    omission_reason: None,
                    next_retrieval_handle: None,
                })
            })
        }

        fn poll_changes(
            &self,
            root: &AdmittedRoot,
            subscriptions: &BTreeSet<ContextProjectionRegistration>,
        ) -> Vec<ContextProjectionChange> {
            let registration = ContextProjectionRegistration {
                kind: ContextProjectionKind::diagnostics(),
                revision: TRACEDECAY_CONTEXT_REVISION,
            };
            if subscriptions.contains(&registration) {
                (0..self.change_count)
                    .map(|index| ContextProjectionChange {
                        root_uri: root.uri().to_owned(),
                        document_uri: Some(format!("file:///root/{index}.rs")),
                        kind: ContextProjectionKind::diagnostics(),
                        generation: 7,
                        identity: identity(),
                        freshness: ContextFreshness::Current,
                        producer_state: ContextProducerState::Complete,
                        coverage: ContextCoverage::Complete,
                        revision: TRACEDECAY_CONTEXT_REVISION,
                        retrieval_handle: None,
                    })
                    .collect()
            } else {
                Vec::new()
            }
        }
    }

    #[test]
    fn context_broker_correlates_pending_work_by_project_and_request() {
        let adapter = ContextProjectionAdapter::new(
            Arc::new(InlineSpawner),
            Arc::new(Authority { change_count: 1 }),
        );
        let root = AdmittedRoot::new("file:///root");
        let request_id = LspRequestId::Number(4);
        let request = ContextProjectionRequest::new(
            ContextProjectionKind::diagnostics(),
            Some("file:///root/a.rs".to_owned()),
        );

        assert_eq!(
            adapter.snapshot(&root, &request_id, &request),
            ContextProjectionOutcome::Pending
        );
        assert!(matches!(
            adapter.poll_snapshot(&root, &request_id),
            Some(ContextProjectionOutcome::Ready(ContextProjectionEnvelope {
                generation: 7,
                ..
            }))
        ));

        let expansion_id = LspRequestId::Number(5);
        let expansion = ContextExpansionRequest {
            retrieval_handle: "rh_0123456789abcdef01234567".to_owned(),
        };
        assert_eq!(
            adapter.expand(&root, &expansion_id, &expansion),
            ContextExpansionOutcome::Pending
        );
        assert!(matches!(
            adapter.poll_expansion(&root, &expansion_id),
            Some(ContextExpansionOutcome::Ready(ContextExpansionEnvelope {
                evidence: Some(_),
                ..
            }))
        ));

        let subscriptions = [ContextProjectionRegistration {
            kind: ContextProjectionKind::diagnostics(),
            revision: TRACEDECAY_CONTEXT_REVISION,
        }]
        .into_iter()
        .collect();
        adapter.update_subscriptions(&root, &subscriptions);
        assert_eq!(adapter.poll_changes(&root, &subscriptions, 1).len(), 1);
        assert!(adapter.poll_changes(&root, &subscriptions, 1).is_empty());
    }

    #[test]
    fn bounded_change_delivery_resumes_without_acknowledging_the_tail() {
        let root = AdmittedRoot::new("file:///root");
        let subscriptions = [ContextProjectionRegistration {
            kind: ContextProjectionKind::diagnostics(),
            revision: TRACEDECAY_CONTEXT_REVISION,
        }]
        .into_iter()
        .collect();
        let adapter = ContextProjectionAdapter::new(
            Arc::new(InlineSpawner),
            Arc::new(Authority { change_count: 17 }),
        );

        let first = adapter.poll_changes(&root, &subscriptions, 16);
        assert_eq!(first.len(), 16);
        assert_eq!(first[0].document_uri.as_deref(), Some("file:///root/0.rs"));
        assert_eq!(
            first[15].document_uri.as_deref(),
            Some("file:///root/15.rs")
        );

        let second = adapter.poll_changes(&root, &subscriptions, 16);
        assert_eq!(second.len(), 1);
        assert_eq!(
            second[0].document_uri.as_deref(),
            Some("file:///root/16.rs")
        );
        assert!(adapter.poll_changes(&root, &subscriptions, 16).is_empty());
    }
}

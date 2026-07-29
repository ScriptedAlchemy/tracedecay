//! Bounded custom context projections carried beside standard LSP methods.
//!
//! The daemon/application owner computes and stores projection truth. This
//! module defines only compact transport DTOs and the read-only port used by
//! one already-authorized, single-root LSP session.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_domain::ContentDigest;

use super::gateway::AdmittedRoot;
use super::session::LspRequestId;

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

    pub fn is_pr12_supported(&self) -> bool {
        matches!(
            self.as_str(),
            Self::DIAGNOSTICS
                | Self::POST_EDIT_IMPACT
                | Self::AFFECTED_TESTS
                | Self::TEST_RUN_RESULTS
        )
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

    fn poll_changes(
        &self,
        _root: &AdmittedRoot,
        _subscriptions: &BTreeSet<ContextProjectionRegistration>,
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
    ) -> Vec<ContextProjectionChange> {
        (**self).poll_changes(root, subscriptions)
    }

    fn update_subscriptions(
        &self,
        root: &AdmittedRoot,
        subscriptions: &BTreeSet<ContextProjectionRegistration>,
    ) {
        (**self).update_subscriptions(root, subscriptions);
    }
}

//! Read-only handoff action discovery and gated issuance contracts.
//!
//! Discovery exposes only bounded, non-secret action references. Issuance is a
//! separate authenticated operation, and its URI may be delivered only through
//! a concrete destination that explicitly accepts it.

use crate::{AdmittedRoot, LspRange, LspRequestId};

pub const MAX_HANDOFF_ACTIONS: usize = 8;
pub const MAX_HANDOFF_ACTION_REF_BYTES: usize = 256;
pub const MAX_HANDOFF_ACTION_TITLE_BYTES: usize = 160;
pub const MAX_HANDOFF_SHOW_DOCUMENT_URI_BYTES: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandoffContractError {
    InvalidActionReference,
    InvalidActionTitle,
    TooManyActions,
    InvalidShowDocumentUri,
}

/// Stable, non-secret reference resolved again by the daemon at issuance time.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct HandoffActionRef(String);

impl HandoffActionRef {
    pub fn new(value: impl Into<String>) -> Result<Self, HandoffContractError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_HANDOFF_ACTION_REF_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
        {
            return Err(HandoffContractError::InvalidActionReference);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandoffAction {
    pub title: String,
    pub action_ref: HandoffActionRef,
}

impl HandoffAction {
    pub fn new(
        title: impl Into<String>,
        action_ref: HandoffActionRef,
    ) -> Result<Self, HandoffContractError> {
        let title = title.into();
        if title.trim().is_empty() || title.len() > MAX_HANDOFF_ACTION_TITLE_BYTES {
            return Err(HandoffContractError::InvalidActionTitle);
        }
        Ok(Self { title, action_ref })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandoffActionDiscoveryRequest {
    pub root: AdmittedRoot,
    pub request_id: LspRequestId,
    pub document_uri: String,
    pub range: LspRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HandoffActionDiscoveryOutcome {
    Available(Vec<HandoffAction>),
    Unavailable(HandoffUnavailableReason),
}

impl HandoffActionDiscoveryOutcome {
    pub fn available(actions: Vec<HandoffAction>) -> Result<Self, HandoffContractError> {
        if actions.len() > MAX_HANDOFF_ACTIONS {
            return Err(HandoffContractError::TooManyActions);
        }
        Ok(Self::Available(actions))
    }
}

pub trait HandoffActionDiscoveryPort {
    fn discover(&self, request: &HandoffActionDiscoveryRequest) -> HandoffActionDiscoveryOutcome;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandoffIssueRequest {
    pub root: AdmittedRoot,
    pub request_id: LspRequestId,
    pub action_ref: HandoffActionRef,
}

/// URI carrying the short-lived handoff token. It intentionally implements
/// neither `Clone` nor `Debug` so logs and diagnostic data cannot copy it.
pub struct HandoffShowDocumentUri(String);

impl HandoffShowDocumentUri {
    pub fn new(value: impl Into<String>) -> Result<Self, HandoffContractError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_HANDOFF_SHOW_DOCUMENT_URI_BYTES
            || url::Url::parse(&value)
                .ok()
                .is_none_or(|uri| uri.cannot_be_a_base())
        {
            return Err(HandoffContractError::InvalidShowDocumentUri);
        }
        Ok(Self(value))
    }

    pub fn scheme(&self) -> &str {
        self.0.split_once(':').map_or("", |(scheme, _)| scheme)
    }

    pub fn into_uri(self) -> String {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandoffUnavailableReason {
    NotFound,
    Stale,
    Unauthorized,
    Unsupported,
    Cancelled,
    TimedOut,
    DestinationUnavailable,
    ProviderUnavailable,
}

pub enum HandoffIssueOutcome {
    ShowDocument(HandoffShowDocumentUri),
    Unavailable(HandoffUnavailableReason),
}

pub trait HandoffActionIssuerPort {
    fn issue(&self, request: &HandoffIssueRequest) -> HandoffIssueOutcome;

    fn cancel(&self, request_id: &LspRequestId) -> bool;
}

/// Host-owned proof that a returned URI scheme has a concrete consumer.
/// Merely having a client `window/showDocument` capability is insufficient.
pub trait HandoffDestinationPort {
    fn accepts_show_document_scheme(&self, scheme: &str) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LspPosition;

    #[test]
    fn action_references_are_bounded_non_secret_handles() {
        let action_ref = HandoffActionRef::new("finding:current.42").expect("action ref");
        let action = HandoffAction::new("Open finding handoff", action_ref).expect("action");
        assert!(matches!(
            HandoffActionDiscoveryOutcome::available(vec![action]),
            Ok(HandoffActionDiscoveryOutcome::Available(_))
        ));
        assert!(HandoffActionRef::new("bearer token").is_err());
    }

    #[test]
    fn show_document_uri_is_consumed_without_debug_or_clone_surface() {
        let request = HandoffActionDiscoveryRequest {
            root: AdmittedRoot::new("file:///workspace"),
            request_id: LspRequestId::String("request.handoff".to_owned()),
            document_uri: "file:///workspace/src/lib.rs".to_owned(),
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
        };
        assert_eq!(request.root.uri(), "file:///workspace");
        let uri = HandoffShowDocumentUri::new("tracedecay://handoff/opaque").expect("URI");
        assert_eq!(uri.scheme(), "tracedecay");
        assert_eq!(uri.into_uri(), "tracedecay://handoff/opaque");
    }
}

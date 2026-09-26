use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::errors::TraceDecayError;

/// Largest rendered problem message, the [`super::SafeDiagnostic`] bound.
const MAX_RENDERED_MESSAGE_BYTES: usize = 512;

/// The structured facts behind a problem. Adapters read these fields; the
/// problem's `message` is only their one human rendering.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationProblemDetailV1 {
    /// The worktree's code index is parked until the operator applies
    /// `remedy`; repeating the request cannot change the answer.
    Parked {
        cause: String,
        remedy: String,
        retries_on_wake: bool,
    },
    /// A session refresh asked to begin from a source frontier the
    /// committed projection has already passed.
    StaleRefreshFrontier {
        requested: u64,
        committed: u64,
        active: u64,
    },
    /// A writer lock stayed held by other writers past its admission
    /// deadline.
    LockDeadline { resource: String, deadline_ms: u64 },
}

impl ApplicationProblemDetailV1 {
    /// The typed detail of a lock that missed its admission deadline.
    pub fn from_lock_deadline(error: &TraceDecayError) -> Option<Self> {
        match error {
            TraceDecayError::LockDeadline {
                resource,
                deadline_ms,
            } => Some(Self::LockDeadline {
                resource: (*resource).to_owned(),
                deadline_ms: *deadline_ms,
            }),
            _ => None,
        }
    }

    /// Stable diagnostic code of the problem this detail names.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Parked { .. } => "application.code-index.parked",
            Self::StaleRefreshFrontier { .. } => "application.retained.refresh-frontier-stale",
            Self::LockDeadline { .. } => "application.lock-deadline",
        }
    }

    /// The one human rendering of this detail, folded and bounded to the
    /// safe diagnostic message limit.
    pub fn message(&self) -> String {
        let text = match self {
            // The remedy leads so a long cause is what the bound cuts.
            Self::Parked { cause, remedy, .. } => format!(
                "The code index for this worktree is parked; remedy: {remedy}; cause: {cause}"
            ),
            Self::StaleRefreshFrontier { active, .. } => format!(
                "The refresh window no longer contains the committed projection frontier \
                 {active}; begin again from source frontier {active}."
            ),
            Self::LockDeadline {
                resource,
                deadline_ms,
            } => format!(
                "The {resource} stayed busy past its {deadline_ms}ms admission deadline; retry \
                 the operation."
            ),
        };
        let folded = tracedecay_domain::fold_control_characters(&text);
        tracedecay_domain::utf8_prefix_at_or_before(folded.trim(), MAX_RENDERED_MESSAGE_BYTES)
            .trim_end()
            .to_owned()
    }

    /// Labelled fields for line-oriented adapters, in display order.
    pub fn labelled_fields(&self) -> Vec<(&'static str, String)> {
        match self {
            Self::Parked {
                cause,
                remedy,
                retries_on_wake,
            } => vec![
                ("Parked cause", cause.clone()),
                ("Parked remedy", remedy.clone()),
                ("Retries on wake", retries_on_wake.to_string()),
            ],
            Self::StaleRefreshFrontier {
                requested,
                committed,
                active,
            } => vec![
                ("Requested frontier", requested.to_string()),
                ("Committed frontier", committed.to_string()),
                ("Active frontier", active.to_string()),
            ],
            Self::LockDeadline {
                resource,
                deadline_ms,
            } => vec![
                ("Lock resource", resource.clone()),
                ("Lock deadline", format!("{deadline_ms}ms")),
            ],
        }
    }
}

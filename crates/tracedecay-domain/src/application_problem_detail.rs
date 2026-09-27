use crate::errors::TraceDecayError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Largest rendered problem message, matching the safe-diagnostic bound.
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
    /// A persisted store whose shape this binary does not open. It is served
    /// in this typed state until the operator runs `remedy`, which deletes
    /// the old data; nothing is migrated or backed up.
    ResetRequired {
        authority: String,
        found_version: Option<i64>,
        required_version: Option<i64>,
        reason: String,
        remedy: String,
    },
    /// No compiler runs automatically for the diagnostics scope: no tsconfig
    /// owns `file`, or, for a workspace read (`file` null), none exists under
    /// the project root. `searched` lists the owner search's candidates,
    /// nearest first; it is empty for a workspace read or a file outside the
    /// project root.
    DiagnosticsUnsupported {
        file: Option<String>,
        searched: Vec<DiagnosticsSearchedTsconfigV1>,
    },
    /// The TypeScript `producer` has not published diagnostics for the
    /// current code generation yet. `generation` is the last generation it
    /// published, null when it has published none.
    DiagnosticsPending {
        producer: String,
        generation: Option<String>,
    },
}

/// One tsconfig location the diagnostics owner search checked.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsSearchedTsconfigV1 {
    /// Relative to the project root, forward-slash separated.
    pub path: String,
    /// The config exists, but neither it nor its references include the
    /// file.
    pub present: bool,
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

    /// The typed detail of a persisted-shape refusal, reset by `remedy`.
    pub fn from_reset_required(error: &TraceDecayError, remedy: impl Into<String>) -> Option<Self> {
        let (authority, found_version, required_version) = match error {
            TraceDecayError::ResetRequired { authority, .. } => (authority.clone(), None, None),
            TraceDecayError::ProfileResetRequired {
                component,
                found_version,
                required_version,
            } => (
                (*component).to_owned(),
                *found_version,
                Some(*required_version),
            ),
            _ => return None,
        };
        let reason = match error {
            TraceDecayError::ResetRequired { reason, .. } => reason.clone(),
            _ => error.to_string(),
        };
        Some(Self::ResetRequired {
            authority,
            found_version,
            required_version,
            reason,
            remedy: remedy.into(),
        })
    }

    /// Stable diagnostic code of the problem this detail names.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Parked { .. } => "application.code-index.parked",
            Self::StaleRefreshFrontier { .. } => "application.retained.refresh-frontier-stale",
            Self::LockDeadline { .. } => "application.lock-deadline",
            Self::ResetRequired { .. } => "application.reset-required",
            Self::DiagnosticsUnsupported { .. } => "application.diagnostics.unsupported",
            Self::DiagnosticsPending { .. } => "application.diagnostics.pending",
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
            // The remedy leads so a long reason is what the bound cuts.
            Self::ResetRequired {
                authority,
                reason,
                remedy,
                ..
            } => format!("The {authority} requires an explicit reset with `{remedy}`: {reason}"),
            Self::DiagnosticsUnsupported { file, searched } => {
                let reason = match file {
                    None => "no tsconfig.json was found under the project root".to_owned(),
                    Some(file) if searched.is_empty() => {
                        format!("`{file}` is outside the project root")
                    }
                    Some(file) => format!(
                        "no tsconfig owns `{file}` (searched {}, and their project references)",
                        searched_list(searched)
                    ),
                };
                format!(
                    "No diagnostic producer is configured for this scope: {reason}, so no \
                     compiler runs automatically. Run the project's own build or type check and \
                     publish its output with tracedecay_diagnose (`cargo_output`), then read \
                     again."
                )
            }
            Self::DiagnosticsPending {
                producer,
                generation,
            } => {
                let last = generation.as_ref().map_or_else(String::new, |generation| {
                    format!(" (its last publication was generation {generation})")
                });
                format!(
                    "The TypeScript producer ({producer}) has not published diagnostics for this \
                     project's current generation yet{last}; it runs after the code index seals \
                     a complete generation. Retry shortly."
                )
            }
        };
        let folded = crate::fold_control_characters(&text);
        crate::utf8_prefix_at_or_before(folded.trim(), MAX_RENDERED_MESSAGE_BYTES)
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
            Self::ResetRequired {
                authority,
                found_version,
                required_version,
                reason,
                remedy,
            } => {
                let mut fields = vec![("Reset authority", authority.clone())];
                if let Some(required_version) = required_version {
                    fields.push((
                        "Found version",
                        found_version.map_or_else(
                            || "unversioned".to_owned(),
                            |version| version.to_string(),
                        ),
                    ));
                    fields.push(("Required version", required_version.to_string()));
                }
                fields.push(("Reset reason", reason.clone()));
                fields.push(("Reset remedy", remedy.clone()));
                fields
            }
            Self::DiagnosticsUnsupported { file, searched } => vec![
                (
                    "Diagnostics file",
                    file.clone().unwrap_or_else(|| "workspace".to_owned()),
                ),
                (
                    "Searched tsconfigs",
                    if searched.is_empty() {
                        "none".to_owned()
                    } else {
                        searched_list(searched)
                    },
                ),
            ],
            Self::DiagnosticsPending {
                producer,
                generation,
            } => vec![
                ("Diagnostics producer", producer.clone()),
                (
                    "Last published generation",
                    generation.clone().unwrap_or_else(|| "none".to_owned()),
                ),
            ],
        }
    }
}

fn searched_list(searched: &[DiagnosticsSearchedTsconfigV1]) -> String {
    searched
        .iter()
        .map(|candidate| {
            if candidate.present {
                format!("{} (does not include it)", candidate.path)
            } else {
                candidate.path.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

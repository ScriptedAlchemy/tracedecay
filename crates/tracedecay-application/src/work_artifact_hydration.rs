//! Typed artifact and evidence hydration over the durable attempt rows.
//!
//! This read answers, for one authority-scoped page of attempts, which
//! artifacts each attempt declared and which sealed terminal evidence record
//! backs them. It pages exactly like the attempt list — a cursor pinned to
//! the verified Work topology generation it was minted under — and it answers
//! coverage as a typed state, never a silently truncated list.
//!
//! Artifact bytes are deliberately not part of this contract. An artifact is
//! answered by its durable reference (identity, content digest, byte length),
//! so a payload the retention contract has released is never materialized by
//! this read; byte access stays with the execution paths that verify a
//! payload against its declared reference before use.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{WorkArtifactRefV1, WorkAttemptIdentityV1, WorkAuthority};

use crate::work::work_authority;
use crate::work_attempt::{
    MAX_WORK_ATTEMPT_LIST_PAGE_SIZE, WorkAttemptEvidenceRecordV1, WorkAttemptListCoverageV1,
    WorkAttemptListCursorV1, WorkAttemptStorageError, WorkAttemptTopologyBindingV1,
    WorkAttemptTopologyStateV1,
};
use crate::{ApplicationProblem, LegalAction, RequestContext, RetryDirective, SafeDiagnostic};

/// One page of attempt rows joined with their sealed evidence records, in the
/// same stable task/run/attempt identity order as the attempt list, read
/// under one consistent storage view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkAttemptEvidencePageV1 {
    pub rows: Vec<WorkAttemptEvidenceRowV1>,
    /// Attempts in scope strictly after the page start, including this page.
    pub remaining: u32,
}

/// One durable attempt row projected to exactly what hydration serves: the
/// attempt identity, its declared artifact references, and the terminal
/// evidence record sealed for it, when one has been sealed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkAttemptEvidenceRowV1 {
    pub identity: WorkAttemptIdentityV1,
    pub artifacts: Vec<WorkArtifactRefV1>,
    pub evidence: Option<WorkAttemptEvidenceRecordV1>,
}

/// Read access to the attempt rows together with their sealed evidence.
///
/// This is a separate port from the attempt lease store on purpose: hydration
/// is a pure read and composes against storage that can answer the evidence
/// column, while the transition port stays the only writer.
pub trait WorkAttemptEvidenceReadPort: Send + Sync {
    /// One page of attempts with their evidence, in stable task/run/attempt
    /// identity order, strictly after `start_after`.
    ///
    /// The page and its remaining count are read under one consistent view,
    /// so `remaining` always covers exactly the rows the cursor has not yet
    /// returned (this page included).
    fn evidence_page(
        &self,
        authority: &WorkAuthority,
        start_after: Option<&WorkAttemptIdentityV1>,
        limit: u32,
    ) -> Result<WorkAttemptEvidencePageV1, WorkAttemptStorageError>;
}

/// One authority-scoped artifact hydration request, paged like the attempt
/// list and pinned to the same verified topology generation.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkArtifactHydrationRequestV1 {
    pub page_size: u32,
    #[serde(default)]
    pub cursor: Option<WorkAttemptListCursorV1>,
}

/// Whether an attempt's terminal evidence has been sealed. An attempt that
/// has not reported an outcome yet is a typed state, not a missing record.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkAttemptEvidenceStateV1 {
    /// The attempt has not sealed terminal evidence yet.
    Pending,
    /// The sealed terminal evidence record, exactly as it was written.
    Sealed { record: WorkAttemptEvidenceRecordV1 },
}

/// The artifacts and evidence one attempt declared.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptArtifactsV1 {
    pub identity: WorkAttemptIdentityV1,
    /// Every artifact reference the attempt declared, in its canonical
    /// stored order. References carry digest and byte length; bytes are
    /// never part of this read.
    pub artifacts: Vec<WorkArtifactRefV1>,
    pub evidence: WorkAttemptEvidenceStateV1,
}

/// One authority-scoped artifact hydration read. Absence of any Work in
/// scope is a typed state, distinct from an authorized-but-empty page.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkArtifactHydrationV1 {
    /// No Work exists in this authority scope, so there is no attempt set to
    /// hydrate. Concealed scopes never reach this state: they are refused as
    /// not-found-or-not-authorized before any read.
    Absent,
    /// One page of attempt artifact sets under the verified topology
    /// snapshot.
    Hydrated {
        topology: WorkAttemptTopologyBindingV1,
        attempts: Vec<WorkAttemptArtifactsV1>,
        coverage: WorkAttemptListCoverageV1,
    },
}

/// The artifact and evidence hydration read authority.
pub struct WorkArtifactHydrationService<S> {
    attempts: S,
}

impl<S> WorkArtifactHydrationService<S>
where
    S: WorkAttemptEvidenceReadPort,
{
    pub const fn new(attempts: S) -> Self {
        Self { attempts }
    }

    /// Hydrates one page-bounded slice of attempt artifact sets under the
    /// verified Work topology snapshot the caller resolves through the graph
    /// publication mount.
    ///
    /// Every non-success is typed: an out-of-bounds page size is an invalid
    /// request, a cursor minted under a superseded topology generation is
    /// stale, a scope with no Work at all is the explicit `Absent` state,
    /// and an authorized scope with no attempts is an explicit zero-complete
    /// page.
    pub fn hydrate(
        &self,
        context: &RequestContext,
        request: &WorkArtifactHydrationRequestV1,
        topology: impl FnOnce(&WorkAuthority) -> Result<WorkAttemptTopologyStateV1, ApplicationProblem>,
    ) -> Result<WorkArtifactHydrationV1, ApplicationProblem> {
        if request.page_size == 0 || request.page_size > MAX_WORK_ATTEMPT_LIST_PAGE_SIZE {
            return Err(invalid_problem(
                "application.work-artifact-hydration.invalid-page-size",
                "The Work artifact hydration page size must be between 1 and 1000.",
            ));
        }
        let authority = work_authority(context)?;
        let binding = match topology(&authority)? {
            WorkAttemptTopologyStateV1::Absent => {
                return if request.cursor.is_some() {
                    // The snapshot the cursor was minted under no longer
                    // exists for this scope; resuming would fabricate a page.
                    Err(stale_cursor_problem())
                } else {
                    Ok(WorkArtifactHydrationV1::Absent)
                };
            }
            WorkAttemptTopologyStateV1::Verified(binding) => binding,
        };
        if let Some(cursor) = &request.cursor
            && cursor.generation != binding.generation
        {
            return Err(stale_cursor_problem());
        }
        let page = self
            .attempts
            .evidence_page(
                &authority,
                request.cursor.as_ref().map(|cursor| &cursor.start_after),
                request.page_size,
            )
            .map_err(storage_problem)?;
        let returned = u32::try_from(page.rows.len())
            .ok()
            .filter(|returned| *returned <= request.page_size && *returned <= page.remaining)
            .ok_or_else(page_contract_problem)?;
        let coverage = if returned == page.remaining {
            WorkAttemptListCoverageV1::Complete { returned }
        } else {
            let resume = page
                .rows
                .last()
                .map(|row| WorkAttemptListCursorV1 {
                    generation: binding.generation.clone(),
                    start_after: row.identity.clone(),
                })
                .ok_or_else(page_contract_problem)?;
            WorkAttemptListCoverageV1::Capped {
                returned,
                remaining: page.remaining - returned,
                resume,
            }
        };
        let attempts = page
            .rows
            .into_iter()
            .map(|row| WorkAttemptArtifactsV1 {
                identity: row.identity,
                artifacts: row.artifacts,
                evidence: match row.evidence {
                    None => WorkAttemptEvidenceStateV1::Pending,
                    Some(record) => WorkAttemptEvidenceStateV1::Sealed { record },
                },
            })
            .collect();
        Ok(WorkArtifactHydrationV1::Hydrated {
            topology: binding,
            attempts,
            coverage,
        })
    }
}

fn storage_problem(error: WorkAttemptStorageError) -> ApplicationProblem {
    match error {
        WorkAttemptStorageError::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        WorkAttemptStorageError::AttemptConflict | WorkAttemptStorageError::FenceConflict => {
            // Hydration never writes, so a conflict from the storage port is
            // a contract violation of the read path, not a caller race.
            page_contract_problem()
        }
        WorkAttemptStorageError::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.work-artifact-hydration.storage-unavailable".to_owned(),
            message: "The Work attempt authority is unavailable.".to_owned(),
        }),
    }
}

fn stale_cursor_problem() -> ApplicationProblem {
    ApplicationProblem::stale(SafeDiagnostic {
        code: "application.work-artifact-hydration.stale-cursor".to_owned(),
        message:
            "The Work artifact hydration cursor was minted under a superseded topology snapshot."
                .to_owned(),
    })
}

fn page_contract_problem() -> ApplicationProblem {
    ApplicationProblem::unavailable(SafeDiagnostic {
        code: "application.work-artifact-hydration.page-inconsistent".to_owned(),
        message: "The Work attempt storage returned an inconsistent hydration page.".to_owned(),
    })
}

fn invalid_problem(code: &str, message: &str) -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: code.to_owned(),
            message: message.to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    }
}
